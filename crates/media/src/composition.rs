use std::{
    future::{Future, IntoFuture},
    sync::Arc,
    time::Duration,
};

use media_api::{
    ApiState, IdempotencyError, IdempotencyGeneration, IdempotencyHandle, IdempotencyRequest,
    IdempotencyStore, OperationCompletionStore, Reservation, StoredHttpResponse,
};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, BootstrapClient, ClientRole, ClientStore, CredentialDigest,
    JobApplication, LeaseApplication, OperationKey, PortError, RUNNER_CLIENT_ID, ReadinessPort,
    SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
use media_storage::{
    ReservationGeneration as StorageReservationGeneration, ReservationHandle, ReservationRecord,
    SeaOrmClientStore, SeaOrmIdempotencyRepository, SeaOrmJobStore, SeaOrmLeaseStore,
    SeaOrmOperationReceiptRepository, SeaOrmReadiness, StoredResponseRecord,
};
use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use secrecy::{ExposeSecret, SecretBox};
use sha2::{Digest, Sha256};

use crate::{
    config::{DatabaseConfig, RezkaCompositionConfig, RunnerConfig, ServerConfig},
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
    let http = Arc::new(
        media_runner::ReqwestHttpAdapter::new(Duration::from_secs(30))
            .map_err(|_| RunnerError::Configuration)?,
    );
    let process = Arc::new(media_runner::TokioProcessAdapter::new(
        "ffprobe",
        "ffmpeg",
        Duration::from_secs(6 * 60 * 60),
    ));
    let (service_url, service_token) = config.service().cloned_parts();
    let plex_service = Arc::new(
        media_runner::HttpRunnerServiceAdapter::new(
            service_url,
            service_token,
            Duration::from_secs(30),
        )
        .map_err(|_| RunnerError::Configuration)?,
    );
    let pipeline = media_runner::EpisodePipeline::new(filesystem, http, process, plex_service);
    let qbittorrent = match config.qbittorrent() {
        Some(config) => {
            let config = media_integrations::qbittorrent::QbittorrentConfig::new(
                config.base_url().clone(),
                config.category(),
                config.username(),
                config.password().clone(),
                Duration::from_secs(30),
            )
            .map_err(|_| RunnerError::Configuration)?;
            Some(Arc::new(
                media_integrations::qbittorrent::QbittorrentClient::connect(config)
                    .await
                    .map_err(|_| RunnerError::Configuration)?,
            ))
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
    let executor = Arc::new(crate::runner::MediaJobExecutor::new(
        rezka,
        pipeline,
        qbittorrent,
        gluetun,
        config.storage_roots().clone(),
        config.vaapi_device().to_owned(),
    ));
    let api = Arc::new(crate::runner::HttpRunnerApi::new(config.service().clone())?);
    crate::runner::run_loop(api, executor, Duration::from_secs(20)).await
}

pub struct PreparedRunnerSession {
    pub client: rezka_client::RezkaClient,
    pub credentials: rezka_client::RezkaCredentials,
    pub probe: rezka_client::SessionValidationProbe,
    pub store: media_runner::EncryptedRezkaSessionStore,
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
    prepare_rezka_session(config.rezka())
}

pub fn prepare_rezka_session(
    config: &RezkaCompositionConfig,
) -> Result<PreparedRunnerSession, RunnerCompositionError> {
    let mirrors = rezka_client::MirrorSet::new(config.mirrors().to_vec())
        .map_err(|_| RunnerCompositionError::Client)?;
    let client = rezka_client::RezkaClient::new(rezka_client::RezkaClientConfig {
        mirrors,
        user_agent: config.user_agent().to_owned(),
        request_timeout: time::Duration::seconds(30),
        max_retries: 2,
        anubis_max_nonce: 5_000_000,
    })
    .map_err(|_| RunnerCompositionError::Client)?;
    let credentials = rezka_client::RezkaCredentials {
        username: config.username().clone(),
        password: config.password().clone(),
    };
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

    Ok(PreparedRunnerSession {
        client,
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
}

impl std::fmt::Debug for PreparedService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PreparedService { router: [REDACTED] }")
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
        let (observed_shutdown, shutdown_observed) = tokio::sync::oneshot::channel();
        let server = axum::serve(listener, self.router)
            .with_graceful_shutdown(async move {
                shutdown.await;
                let _ = observed_shutdown.send(());
            })
            .into_future();
        tokio::pin!(server);

        tokio::select! {
            result = &mut server => result.map_err(|_| ServiceError::Server),
            observed = shutdown_observed => {
                if observed.is_err() {
                    return server.await.map_err(|_| ServiceError::Server);
                }
                match tokio::time::timeout(timeout, &mut server).await {
                    Ok(result) => result.map_err(|_| ServiceError::Server),
                    Err(_) => Err(ServiceError::ShutdownTimeout),
                }
            }
        }
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
    );
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
                    Duration::from_secs(30),
                )
                .map_err(|_| ServiceError::Bootstrap)?;
                media_integrations::prowlarr::ProwlarrClient::new(config)
                    .map_err(|_| ServiceError::Bootstrap)
            })
            .transpose()?;
        let persistence = Arc::new(StorageSearchPersistence::new(
            media_storage::SeaOrmSearchRepository::new(database),
        ));
        let provider = Arc::new(ConcreteSearchProvider::new(rezka, prowlarr));
        state = state.with_search(Arc::new(DurableSearchService::new(
            persistence,
            provider,
            jobs,
        )));
    }

    Ok(PreparedService {
        router: media_api::router(state),
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
