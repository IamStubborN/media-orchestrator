use std::{
    future::{Future, IntoFuture},
    sync::Arc,
    time::Duration,
};

use media_api::{
    ApiState, IdempotencyError, IdempotencyGeneration, IdempotencyHandle, IdempotencyRequest,
    IdempotencyStore, OperationCompletionStore, PlexReconcileService, PlexServiceError,
    Reservation, StoredHttpResponse,
};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, BootstrapClient, ClientRole, ClientStore, CredentialDigest,
    EpisodeDiscoveryPort, JobApplication, LIFECYCLE_CLIENT_ID, LeaseApplication,
    NotificationDispatcher, NotificationId, OperationKey, PortError, RUNNER_CLIENT_ID,
    ReadinessPort, RunnerLifecycleApplication, TrackingApplication, TrackingRuntime,
    SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
use media_storage::{
    ReservationGeneration as StorageReservationGeneration, ReservationHandle, ReservationRecord,
    SeaOrmClientStore, SeaOrmIdempotencyRepository, SeaOrmJobStore, SeaOrmLeaseStore,
    SeaOrmMaintenanceStore, SeaOrmMetricsSource, SeaOrmNotificationOutbox,
    SeaOrmOperationReceiptRepository, SeaOrmReadiness, SeaOrmRunnerLifecycleStore,
    SeaOrmTrackingStore, StoredResponseRecord,
};
use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use secrecy::{ExposeSecret, SecretBox};
use sha2::{Digest, Sha256};

use crate::{
    config::{
        DatabaseConfig, NotificationConfig, RezkaCompositionConfig, RunnerConfig, ServerConfig,
    },
    search::{ConcreteSearchProvider, DurableSearchService, StorageSearchPersistence},
};

const IDEMPOTENCY_TTL: time::Duration = time::Duration::hours(24);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ServiceError {
    #[error("database connection failed")]
    Database,
    #[error("database migration failed")]
    Migration,
    #[error("database migrations are pending")]
    PendingMigrations,
    #[error("service readiness check failed")]
    Readiness,
    #[error("fixed client bootstrap failed")]
    Bootstrap,
    #[error("HTTP listener failed")]
    Listener,
    #[error("HTTP server failed")]
    Server,
    #[error("graceful shutdown timed out")]
    ShutdownTimeout,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum RunnerCompositionError {
    #[error("Rezka client construction failed")]
    Client,
    #[error("Rezka validation probe construction failed")]
    Probe,
    #[error("Rezka session store construction failed")]
    Store,
}

pub type RunnerError = crate::runner::RunnerError;

pub async fn run_runner(config: RunnerConfig) -> Result<(), RunnerError> {
    let rezka = prepare_runner_session(&config).map_err(|_| RunnerError::Configuration)?;
    let filesystem = Arc::new(media_runner::TokioFileSystem);
    filesystem
        .prepare_storage_roots(config.storage_roots())
        .await
        .map_err(|_| RunnerError::Configuration)?;
    // Media transfers stream for far longer than any fixed request deadline, so
    // this client has no total timeout: a slow connect is bounded by
    // connect_timeout and a stalled body by the idle read_timeout, while logical
    // hangs rely on cooperative cancellation.
    let http = Arc::new(
        media_runner::ReqwestHttpAdapter::streaming(
            Duration::from_secs(30),
            Duration::from_secs(60),
        )
        .map_err(|_| RunnerError::Configuration)?,
    );
    let process = Arc::new(media_runner::TokioProcessAdapter::new(
        "ffprobe",
        "ffmpeg",
        Duration::from_secs(6 * 60 * 60),
    ));
    let (service_url, service_token) = config.service().cloned_parts();
    // The server-side Plex reconcile polls up to 30s (PlexReconcileAdapter
    // max_wait); the client needs headroom over that deadline so it does not time
    // out first when Plex is slow.
    let plex_service = Arc::new(
        media_runner::HttpRunnerServiceAdapter::new(
            service_url,
            service_token,
            Duration::from_secs(45),
        )
        .map_err(|_| RunnerError::Configuration)?,
    );
    let pipeline = media_runner::EpisodePipeline::new(filesystem, http, process, plex_service)
        .with_storage_reserve_bytes(config.storage_reserve_bytes());
    let qbittorrent = match config.qbittorrent() {
        Some(config) => {
            let config = media_integrations::qbittorrent::QbittorrentConfig::new(
                config.base_url().clone(),
                config.tv_category(),
                config.username(),
                config.password().clone(),
                Duration::from_secs(30),
            )
            .map_err(|_| RunnerError::Configuration)?;
            Some(Arc::new(connect_qbittorrent(config).await?))
        }
        None => None,
    };
    let gluetun = config
        .gluetun()
        .map(|config| {
            let config = media_integrations::gluetun::GluetunConfig::new(
                config.base_url().clone(),
                config.api_key().clone(),
                Duration::from_secs(30),
            )
            .map_err(|_| RunnerError::Configuration)?;
            media_integrations::gluetun::GluetunClient::new(config)
                .map(Arc::new)
                .map_err(|_| RunnerError::Configuration)
        })
        .transpose()?;
    let broker_config = media_integrations::credential_broker::CredentialBrokerConfig::new(
        config.credential_broker().base_url().clone(),
        config.credential_broker().token().clone(),
        Duration::from_secs(15),
        config.credential_broker().private_http_hosts(),
    )
    .map_err(|_| RunnerError::Configuration)?;
    let credential_broker = Arc::new(
        media_integrations::credential_broker::CredentialBrokerClient::new(broker_config)
            .map_err(|_| RunnerError::Configuration)?,
    );
    let executor = Arc::new(crate::runner::MediaJobExecutor::new(
        rezka,
        pipeline,
        crate::runner::TorrentRouting::new(
            qbittorrent,
            config
                .qbittorrent()
                .map_or_else(|| "tv".to_owned(), |value| value.tv_category().to_owned()),
            config.qbittorrent().map_or_else(
                || "movies".to_owned(),
                |value| value.movies_category().to_owned(),
            ),
        ),
        gluetun,
        credential_broker,
        config.storage_roots().clone(),
        config.vaapi_device().to_owned(),
    ));
    let api = Arc::new(crate::runner::HttpRunnerApi::new(config.service().clone())?);
    crate::runner::run_loop(
        api,
        executor,
        Duration::from_secs(20),
        config.exit_after_job(),
    )
    .await
}

async fn connect_qbittorrent(
    config: media_integrations::qbittorrent::QbittorrentConfig,
) -> Result<media_integrations::qbittorrent::QbittorrentClient, RunnerError> {
    const ATTEMPTS: usize = 12;
    const RETRY_DELAY: Duration = Duration::from_secs(5);

    for attempt in 1..=ATTEMPTS {
        match media_integrations::qbittorrent::QbittorrentClient::connect(config.clone()).await {
            Ok(client) => return Ok(client),
            Err(error)
                if error.code()
                    == media_integrations::qbittorrent::QbittorrentErrorCode::Transport =>
            {
                if attempt == ATTEMPTS {
                    return Err(RunnerError::Execution);
                }
                tracing::warn!(attempt, "qBittorrent is not ready; retrying connection");
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(_) => return Err(RunnerError::Configuration),
        }
    }
    Err(RunnerError::Execution)
}

pub struct PreparedRunnerSession {
    pub client: rezka_client::RezkaClient,
    client_config: rezka_client::RezkaClientConfig,
    pub credentials: Option<rezka_client::RezkaCredentials>,
    pub probe: rezka_client::SessionValidationProbe,
    pub store: media_runner::EncryptedRezkaSessionStore,
}

impl PreparedRunnerSession {
    pub fn reload_session(&mut self) -> Result<(), RunnerCompositionError> {
        let Some(snapshot) = self
            .store
            .load()
            .map_err(|_| RunnerCompositionError::Store)?
        else {
            return Ok(());
        };
        self.client =
            rezka_client::RezkaClient::from_snapshot(self.client_config.clone(), &snapshot)
                .map_err(|_| RunnerCompositionError::Client)?;
        Ok(())
    }
}

impl std::fmt::Debug for PreparedRunnerSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedRunnerSession")
            .field("client", &"[REDACTED]")
            .field("credentials", &"[REDACTED]")
            .field("probe", &"[REDACTED]")
            .field("store", &"[REDACTED]")
            .finish()
    }
}

pub fn prepare_runner_session(
    config: &RunnerConfig,
) -> Result<PreparedRunnerSession, RunnerCompositionError> {
    prepare_rezka_session_inner(config.rezka(), false)
}

pub fn prepare_rezka_session(
    config: &RezkaCompositionConfig,
) -> Result<PreparedRunnerSession, RunnerCompositionError> {
    prepare_rezka_session_inner(config, true)
}

fn prepare_rezka_session_inner(
    config: &RezkaCompositionConfig,
    include_credentials: bool,
) -> Result<PreparedRunnerSession, RunnerCompositionError> {
    let mirrors = rezka_client::MirrorSet::new(config.mirrors().to_vec())
        .map_err(|_| RunnerCompositionError::Client)?;
    let client_config = rezka_client::RezkaClientConfig {
        mirrors,
        user_agent: config.user_agent().to_owned(),
        request_timeout: time::Duration::seconds(30),
        max_retries: 2,
        anubis_max_nonce: 5_000_000,
    };
    let credentials = include_credentials.then(|| rezka_client::RezkaCredentials {
        username: config.username().clone(),
        password: config.password().clone(),
    });
    let probe = rezka_client::SessionValidationProbe::new(
        config.session_probe_url().clone(),
        config.session_valid_markers().to_vec(),
        config.session_invalid_markers().to_vec(),
    )
    .map_err(|_| RunnerCompositionError::Probe)?;
    let key = SecretBox::<[u8; 32]>::init_with_mut(|key| {
        key.copy_from_slice(config.cookie_key().expose_secret());
    });
    let store =
        media_runner::EncryptedRezkaSessionStore::new(media_runner::RezkaSessionStoreConfig {
            path: config.session_store_path().to_owned(),
            key,
        })
        .map_err(|_| RunnerCompositionError::Store)?;
    let snapshot = store.load().map_err(|_| RunnerCompositionError::Store)?;
    let client = match snapshot.as_ref() {
        Some(snapshot) => rezka_client::RezkaClient::from_snapshot(client_config.clone(), snapshot),
        None => rezka_client::RezkaClient::new(client_config.clone()),
    }
    .map_err(|_| RunnerCompositionError::Client)?;

    Ok(PreparedRunnerSession {
        client,
        client_config,
        credentials,
        probe,
        store,
    })
}

/// Composition-local bridge between the API's idempotency port and SeaORM storage.
#[derive(Clone)]
pub struct StorageIdempotencyAdapter {
    repository: SeaOrmIdempotencyRepository,
}

/// Composition-local bridge for checking only durable operation completion.
#[derive(Clone)]
pub struct StorageOperationCompletionAdapter {
    repository: SeaOrmOperationReceiptRepository,
}

impl std::fmt::Debug for StorageOperationCompletionAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StorageOperationCompletionAdapter { repository: [REDACTED] }")
    }
}

impl StorageOperationCompletionAdapter {
    #[must_use]
    pub fn new(repository: SeaOrmOperationReceiptRepository) -> Self {
        Self { repository }
    }
}

#[async_trait::async_trait]
impl OperationCompletionStore for StorageOperationCompletionAdapter {
    async fn is_completed(&self, operation: OperationKey) -> Result<bool, IdempotencyError> {
        self.repository
            .is_completed(operation)
            .await
            .map_err(map_idempotency_error)
    }
}

impl std::fmt::Debug for StorageIdempotencyAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StorageIdempotencyAdapter { repository: [REDACTED] }")
    }
}

impl StorageIdempotencyAdapter {
    #[must_use]
    pub fn new(repository: SeaOrmIdempotencyRepository) -> Self {
        Self { repository }
    }

    fn storage_handle(handle: &IdempotencyHandle) -> Result<ReservationHandle, IdempotencyError> {
        ReservationHandle::rehydrate(
            handle.client_id(),
            handle.key().to_owned(),
            *handle.fingerprint(),
            StorageReservationGeneration::from_uuid(*handle.generation().as_uuid()),
        )
        .map_err(|_| IdempotencyError::Infrastructure)
    }
}

#[async_trait::async_trait]
impl IdempotencyStore for StorageIdempotencyAdapter {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        let record = self
            .repository
            .reserve(
                request.client_id(),
                request.key(),
                *request.fingerprint(),
                time::OffsetDateTime::now_utc() + IDEMPOTENCY_TTL,
            )
            .await
            .map_err(map_idempotency_error)?;
        Ok(match record {
            ReservationRecord::Reserved(storage) => {
                let api = api_handle(&storage);
                Reservation::Reserved(api)
            }
            ReservationRecord::Replay { handle, response } => {
                let api = api_handle(&handle);
                Reservation::Replay {
                    handle: api,
                    response: api_response(response),
                }
            }
            ReservationRecord::Conflict => Reservation::Conflict,
            ReservationRecord::InProgress(storage) => Reservation::InProgress(api_handle(&storage)),
        })
    }

    async fn complete(
        &self,
        handle: &IdempotencyHandle,
        response: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        let storage = Self::storage_handle(handle)?;
        self.repository
            .complete(&storage, storage_response(response))
            .await
            .map_err(map_idempotency_error)
    }

    async fn abort_in_progress(&self, handle: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        let storage = Self::storage_handle(handle)?;
        self.repository
            .abort_in_progress(&storage)
            .await
            .map_err(map_idempotency_error)
    }
}

const fn map_idempotency_error(error: PortError) -> IdempotencyError {
    match error {
        PortError::Conflict => IdempotencyError::Conflict,
        PortError::Infrastructure => IdempotencyError::Infrastructure,
    }
}

fn api_handle(storage: &ReservationHandle) -> IdempotencyHandle {
    IdempotencyHandle::new(
        IdempotencyRequest::new(
            storage.client_id(),
            storage.key().to_owned(),
            *storage.request_hash(),
        ),
        IdempotencyGeneration::from_uuid(*storage.generation().as_uuid()),
    )
}

fn api_response(storage: StoredResponseRecord) -> StoredHttpResponse {
    StoredHttpResponse::new(
        storage.status(),
        storage.content_type().to_owned(),
        storage.body().to_vec(),
    )
}

fn storage_response(api: StoredHttpResponse) -> StoredResponseRecord {
    StoredResponseRecord::new(api.status(), api.content_type().to_owned(), api.into_body())
        .expect("media-api responses always satisfy media-storage response validation")
}

pub struct PreparedService {
    router: axum::Router,
    notifications: Option<PreparedNotificationDispatcher>,
    tracking: Option<TrackingRuntime>,
    maintenance: SeaOrmMaintenanceStore,
}

pub struct PreparedNotificationDispatcher {
    dispatcher: NotificationDispatcher,
    worker: NotificationId,
}

impl PreparedNotificationDispatcher {
    pub async fn run_once(&self) -> Result<media_core::NotificationDispatchResult, PortError> {
        self.dispatcher
            .run_once(self.worker, time::OffsetDateTime::now_utc(), 25)
            .await
    }
}

pub fn prepare_notification_dispatcher(
    database: DatabaseConnection,
    config: &NotificationConfig,
) -> Result<PreparedNotificationDispatcher, ServiceError> {
    let (primary_endpoint, secondary_endpoint, primary_secret, secondary_secret) = config.parts();
    let webhook = media_integrations::hermes::HermesWebhookClient::new(
        media_integrations::hermes::HermesWebhookConfig::new(
            primary_endpoint,
            secondary_endpoint,
            primary_secret,
            secondary_secret,
        ),
    )
    .map_err(|_| ServiceError::Bootstrap)?;
    Ok(PreparedNotificationDispatcher {
        dispatcher: NotificationDispatcher::new(
            Arc::new(SeaOrmNotificationOutbox::new(database)),
            Arc::new(webhook),
        ),
        worker: NotificationId::new(),
    })
}

#[must_use]
pub fn prepare_tracking_scheduler(
    database: DatabaseConnection,
    discovery: Arc<dyn EpisodeDiscoveryPort>,
) -> TrackingRuntime {
    TrackingRuntime::new(Arc::new(SeaOrmTrackingStore::new(database)), discovery)
}

pub struct PlexReconcileAdapter {
    client: media_integrations::plex::PlexClient,
    tv_section: u32,
    movies_section: u32,
    poll_interval: Duration,
    max_wait: Duration,
}

impl PlexReconcileAdapter {
    #[must_use]
    pub fn new(
        client: media_integrations::plex::PlexClient,
        tv_section: u32,
        movies_section: u32,
    ) -> Self {
        Self {
            client,
            tv_section,
            movies_section,
            poll_interval: Duration::from_secs(1),
            max_wait: Duration::from_secs(30),
        }
    }

    #[must_use]
    pub fn with_polling(mut self, poll_interval: Duration, max_wait: Duration) -> Self {
        assert!(
            !poll_interval.is_zero(),
            "Plex poll interval must be positive"
        );
        assert!(
            !max_wait.is_zero(),
            "Plex polling deadline must be positive"
        );
        self.poll_interval = poll_interval;
        self.max_wait = max_wait;
        self
    }
}

#[async_trait::async_trait]
impl PlexReconcileService for PlexReconcileAdapter {
    async fn reconcile(
        &self,
        request: media_contract::PlexReconcileRequest,
    ) -> Result<media_contract::PlexReconcileResponse, PlexServiceError> {
        let path = std::path::PathBuf::from(&request.path);
        let (season, episode, section) = match (request.season, request.episode) {
            (Some(season), Some(episode)) => (
                Some(u16::try_from(season).map_err(|_| PlexServiceError::InvalidRequest)?),
                Some(u16::try_from(episode).map_err(|_| PlexServiceError::InvalidRequest)?),
                self.tv_section,
            ),
            (None, None) => (None, None, self.movies_section),
            _ => return Err(PlexServiceError::InvalidRequest),
        };
        let scan_path = path.parent().ok_or(PlexServiceError::InvalidRequest)?;
        let scan = media_integrations::plex::ScanRequest::new(section, scan_path)
            .map_err(|_| PlexServiceError::InvalidRequest)?;
        if let Err(error) = self.client.trigger_scan(&scan).await {
            return if transient_plex_error(&error) {
                Ok(pending_plex_response())
            } else {
                Err(PlexServiceError::Infrastructure)
            };
        }
        let deadline = tokio::time::Instant::now() + self.max_wait;
        let verification = loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break media_integrations::plex::PlexVerification::NotFound;
            }
            match tokio::time::timeout(
                remaining,
                self.client
                    .verify_path(section, &path, &request.canonical_id, season, episode),
            )
            .await
            {
                Ok(Ok(media_integrations::plex::PlexVerification::NotFound)) => {}
                Ok(Ok(verification)) => break verification,
                Ok(Err(error)) if !transient_plex_error(&error) => {
                    return Err(PlexServiceError::Infrastructure);
                }
                Ok(Err(_)) | Err(_) => {
                    break media_integrations::plex::PlexVerification::NotFound;
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break media_integrations::plex::PlexVerification::NotFound;
            }
            tokio::time::sleep(self.poll_interval.min(remaining)).await;
        };
        let (status, observation) = match verification {
            media_integrations::plex::PlexVerification::Matched { .. } => (
                media_contract::PlexReconcileStatus::Matched,
                Some(media_contract::PlexObservationDto {
                    path: request.path,
                    canonical_id: request.canonical_id,
                    season: request.season,
                    episode: request.episode,
                }),
            ),
            media_integrations::plex::PlexVerification::NotFound => {
                (media_contract::PlexReconcileStatus::Pending, None)
            }
            media_integrations::plex::PlexVerification::Mismatch(_) => {
                (media_contract::PlexReconcileStatus::Mismatch, None)
            }
        };
        Ok(media_contract::PlexReconcileResponse {
            status,
            observation,
        })
    }
}

fn transient_plex_error(error: &media_integrations::plex::PlexError) -> bool {
    matches!(
        error.code(),
        media_integrations::plex::PlexErrorCode::Transport
            | media_integrations::plex::PlexErrorCode::ProviderResponse
    )
}

fn pending_plex_response() -> media_contract::PlexReconcileResponse {
    media_contract::PlexReconcileResponse {
        status: media_contract::PlexReconcileStatus::Pending,
        observation: None,
    }
}

impl std::fmt::Debug for PreparedService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "PreparedService { router: [REDACTED], notifications: [REDACTED], tracking: [REDACTED], maintenance: [REDACTED] }",
        )
    }
}

impl PreparedService {
    pub async fn serve_with_shutdown<F>(
        self,
        listener: tokio::net::TcpListener,
        shutdown: F,
    ) -> Result<(), ServiceError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.serve_with_shutdown_timeout(listener, shutdown, DEFAULT_SHUTDOWN_TIMEOUT)
            .await
    }

    pub async fn serve_with_shutdown_timeout<F>(
        self,
        listener: tokio::net::TcpListener,
        shutdown: F,
        timeout: Duration,
    ) -> Result<(), ServiceError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self {
            router,
            notifications,
            tracking,
            maintenance,
        } = self;
        let maintenance_task = tokio::spawn(async move {
            loop {
                match maintenance.run(time::OffsetDateTime::now_utc()).await {
                    Ok(report) => tracing::info!(
                        search_sessions_deleted = report.search_sessions_deleted,
                        search_executions_deleted = report.search_executions_deleted,
                        jobs_deleted = report.jobs_deleted,
                        notifications_deleted = report.notifications_deleted,
                        outbox_events_deleted = report.outbox_events_deleted,
                        idempotency_records_deleted = report.idempotency_records_deleted,
                        operation_receipts_deleted = report.operation_receipts_deleted,
                        "database retention pass completed"
                    ),
                    Err(_) => tracing::warn!("database retention pass failed"),
                }
                tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
            }
        });
        let notification_task = notifications.map(|dispatcher| {
            tokio::spawn(async move {
                loop {
                    match dispatcher.run_once().await {
                        // A dead-lettered notification will never be retried, so
                        // surface the count (no message content, which may carry
                        // recipient detail) for operator visibility.
                        Ok(result) if result.dead > 0 => tracing::warn!(
                            dead = result.dead,
                            "notifications permanently dead-lettered"
                        ),
                        Ok(_) => {}
                        Err(_) => tracing::warn!("notification dispatch pass failed"),
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            })
        });
        let tracking_task = tracking.map(|runtime| {
            tokio::spawn(async move {
                loop {
                    if runtime
                        .run_once(time::OffsetDateTime::now_utc(), 25)
                        .await
                        .is_err()
                    {
                        tracing::warn!("tracking discovery pass failed");
                    }
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            })
        });
        let (observed_shutdown, shutdown_observed) = tokio::sync::oneshot::channel();
        let server = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                shutdown.await;
                let _ = observed_shutdown.send(());
            })
            .into_future();
        tokio::pin!(server);

        let result = tokio::select! {
            result = &mut server => result.map_err(|_| ServiceError::Server),
            observed = shutdown_observed => {
                if observed.is_err() {
                    server.await.map_err(|_| ServiceError::Server)
                } else {
                    match tokio::time::timeout(timeout, &mut server).await {
                        Ok(result) => result.map_err(|_| ServiceError::Server),
                        Err(_) => Err(ServiceError::ShutdownTimeout),
                    }
                }
            }
        };
        if let Some(task) = notification_task {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = tracking_task {
            task.abort();
            let _ = task.await;
        }
        maintenance_task.abort();
        let _ = maintenance_task.await;
        result
    }
}

pub async fn migrate(config: &DatabaseConfig) -> Result<(), ServiceError> {
    let database = connect(config.database_url()).await?;
    media_storage::Migrator::up(&database, None)
        .await
        .map_err(|_| ServiceError::Migration)
}

pub async fn prepare_service(config: &ServerConfig) -> Result<PreparedService, ServiceError> {
    let database = connect(config.database_url()).await?;
    let readiness = Arc::new(SeaOrmReadiness::new(database.clone()));
    match readiness
        .is_ready()
        .await
        .map_err(|_| ServiceError::Readiness)?
    {
        true => {}
        false => return Err(ServiceError::PendingMigrations),
    }

    let clients = Arc::new(SeaOrmClientStore::new(database.clone()));
    bootstrap_clients(clients.as_ref(), config).await?;

    let jobs = Arc::new(JobApplication::new(Arc::new(SeaOrmJobStore::new(
        database.clone(),
    ))));
    let leases = Arc::new(
        LeaseApplication::new(
            Arc::new(SeaOrmLeaseStore::new(database.clone())),
            config.lease_ttl(),
        )
        .map_err(|_| ServiceError::Bootstrap)?,
    );
    let idempotency = Arc::new(StorageIdempotencyAdapter::new(
        SeaOrmIdempotencyRepository::new(database.clone()),
    ));
    let tracking = Arc::new(TrackingApplication::new(Arc::new(
        SeaOrmTrackingStore::new(database.clone()),
    )));
    let notifications = config
        .notifications()
        .map(|notification| prepare_notification_dispatcher(database.clone(), notification))
        .transpose()?;
    let operations = Arc::new(StorageOperationCompletionAdapter::new(
        SeaOrmOperationReceiptRepository::new(database.clone()),
    ));
    let mut state = ApiState::new(
        jobs.clone(),
        leases,
        clients,
        idempotency,
        operations,
        readiness,
    )
    .with_tracking(tracking)
    .with_release_metadata(Arc::new(media_core::ReleaseMetadataService::new(Arc::new(
        media_integrations::tvmaze::TvmazeClient::new(
            media_integrations::tvmaze::TvmazeConfig::new(
                config.tvmaze().base_url().clone(),
                Duration::from_secs(15),
                config.tvmaze().user_agent().to_owned(),
                2,
            )
            .map_err(|_| ServiceError::Bootstrap)?,
        )
        .map_err(|_| ServiceError::Bootstrap)?,
    ))))
    .with_lifecycle(Arc::new(RunnerLifecycleApplication::new(Arc::new(
        SeaOrmRunnerLifecycleStore::new(database.clone()),
    ))))
    .with_metrics_source(Arc::new(SeaOrmMetricsSource::new(database.clone())));
    let mut tracking_runtime = None;
    if config.rezka().is_some() || config.prowlarr().is_some() {
        let rezka = config
            .rezka()
            .map(prepare_rezka_session)
            .transpose()
            .map_err(|_| ServiceError::Bootstrap)?;
        let prowlarr = config
            .prowlarr()
            .map(|config| {
                let config = media_integrations::prowlarr::ProwlarrConfig::new(
                    config.base_url().clone(),
                    config.api_key().clone(),
                    Duration::from_secs(120),
                )
                .map_err(|_| ServiceError::Bootstrap)?;
                media_integrations::prowlarr::ProwlarrClient::new(config)
                    .map_err(|_| ServiceError::Bootstrap)
            })
            .transpose()?;
        let persistence = Arc::new(StorageSearchPersistence::new(
            media_storage::SeaOrmSearchRepository::new(database.clone()),
        ));
        let rezka_tracking_enabled = rezka.is_some();
        let provider = Arc::new(ConcreteSearchProvider::new(rezka, prowlarr));
        if rezka_tracking_enabled {
            tracking_runtime = Some(prepare_tracking_scheduler(
                database.clone(),
                Arc::new(crate::search::ProviderEpisodeDiscovery::new(
                    provider.clone(),
                )),
            ));
        }
        state = state.with_search(Arc::new(DurableSearchService::new(
            persistence,
            provider,
            jobs,
        )));
    }
    if let Some(config) = config.plex() {
        let plex_config = media_integrations::plex::PlexConfig::new(
            config.base_url().clone(),
            config.token().clone(),
            Duration::from_secs(30),
        )
        .map_err(|_| ServiceError::Bootstrap)?;
        let client = media_integrations::plex::PlexClient::new(plex_config)
            .map_err(|_| ServiceError::Bootstrap)?;
        state = state.with_plex(Arc::new(PlexReconcileAdapter::new(
            client,
            config.tv_section(),
            config.movies_section(),
        )));
    }

    Ok(PreparedService {
        router: media_api::router(state),
        notifications,
        tracking: tracking_runtime,
        maintenance: SeaOrmMaintenanceStore::new(database),
    })
}

pub async fn serve(config: ServerConfig) -> Result<(), ServiceError> {
    let address = config.listen_addr();
    let prepared = prepare_service(&config).await?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|_| ServiceError::Listener)?;
    tracing::info!(listen_addr = %address, "media service listening");
    prepared
        .serve_with_shutdown(listener, shutdown_signal())
        .await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return std::future::pending::<()>().await;
        };
        signal.recv().await;
    };

    #[cfg(unix)]
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    #[cfg(not(unix))]
    ctrl_c.await;

    tracing::info!("media service shutdown requested");
}

async fn connect(database_url: &secrecy::SecretString) -> Result<DatabaseConnection, ServiceError> {
    Database::connect(database_url.expose_secret())
        .await
        .map_err(|_| ServiceError::Database)
}

async fn bootstrap_clients(
    store: &SeaOrmClientStore,
    config: &ServerConfig,
) -> Result<(), ServiceError> {
    for client in [
        BootstrapClient::new(
            PRIMARY_CLIENT_ID,
            "Primary".to_owned(),
            ClientRole::Hermes,
            Some(PRIMARY_USER_ID),
            digest(config.primary_token()),
        ),
        BootstrapClient::new(
            SECONDARY_CLIENT_ID,
            "Secondary".to_owned(),
            ClientRole::Hermes,
            Some(SECONDARY_USER_ID),
            digest(config.secondary_token()),
        ),
        BootstrapClient::new(
            RUNNER_CLIENT_ID,
            "runner".to_owned(),
            ClientRole::Runner,
            None,
            digest(config.runner_token()),
        ),
        BootstrapClient::new(
            LIFECYCLE_CLIENT_ID,
            "lifecycle".to_owned(),
            ClientRole::Lifecycle,
            None,
            digest(config.lifecycle_token()),
        ),
    ] {
        store
            .upsert_client(client.map_err(|_| ServiceError::Bootstrap)?)
            .await
            .map_err(|_| ServiceError::Bootstrap)?;
    }
    Ok(())
}

fn digest(token: &secrecy::SecretString) -> CredentialDigest {
    let bytes: [u8; 32] = Sha256::digest(token.expose_secret().as_bytes()).into();
    CredentialDigest::from(bytes)
}
