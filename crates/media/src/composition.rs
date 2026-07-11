use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};

use media_api::{
    ApiState, IdempotencyError, IdempotencyGeneration, IdempotencyHandle, IdempotencyRequest,
    IdempotencyStore, Reservation, StoredHttpResponse,
};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, BootstrapClient, ClientId, ClientRole, ClientStore,
    CredentialDigest, JobApplication, LeaseApplication, RUNNER_CLIENT_ID, ReadinessPort,
    SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
use media_storage::{
    ReservationHandle, ReservationRecord, SeaOrmClientStore, SeaOrmIdempotencyRepository,
    SeaOrmJobStore, SeaOrmLeaseStore, SeaOrmReadiness, StoredResponseRecord,
};
use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};

use crate::config::{DatabaseConfig, ServerConfig};

const IDEMPOTENCY_TTL: time::Duration = time::Duration::hours(24);

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
}

/// Composition-local bridge between the API's idempotency port and SeaORM storage.
#[derive(Clone)]
pub struct StorageIdempotencyAdapter {
    repository: SeaOrmIdempotencyRepository,
    handles: Arc<Mutex<HashMap<HandleKey, ReservationHandle>>>,
}

impl std::fmt::Debug for StorageIdempotencyAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StorageIdempotencyAdapter { repository: [REDACTED] }")
    }
}

impl StorageIdempotencyAdapter {
    #[must_use]
    pub fn new(repository: SeaOrmIdempotencyRepository) -> Self {
        Self {
            repository,
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn remember(
        &self,
        api: &IdempotencyHandle,
        storage: ReservationHandle,
    ) -> Result<(), IdempotencyError> {
        self.handles
            .lock()
            .map_err(|_| IdempotencyError::Infrastructure)?
            .insert(HandleKey::from(api), storage);
        Ok(())
    }

    fn storage_handle(
        &self,
        handle: &IdempotencyHandle,
    ) -> Result<ReservationHandle, IdempotencyError> {
        self.handles
            .lock()
            .map_err(|_| IdempotencyError::Infrastructure)?
            .get(&HandleKey::from(handle))
            .cloned()
            .ok_or(IdempotencyError::Infrastructure)
    }

    fn forget(&self, handle: &IdempotencyHandle) {
        if let Ok(mut handles) = self.handles.lock() {
            handles.remove(&HandleKey::from(handle));
        }
    }
}

#[derive(Eq, Hash, PartialEq)]
struct HandleKey {
    client_id: ClientId,
    key: String,
    fingerprint: [u8; 32],
    generation: uuid::Uuid,
}

impl From<&IdempotencyHandle> for HandleKey {
    fn from(value: &IdempotencyHandle) -> Self {
        Self {
            client_id: value.client_id(),
            key: value.key().to_owned(),
            fingerprint: *value.fingerprint(),
            generation: *value.generation().as_uuid(),
        }
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
            .map_err(|_| IdempotencyError::Infrastructure)?;
        Ok(match record {
            ReservationRecord::Reserved(storage) => {
                let api = api_handle(&storage);
                self.remember(&api, storage)?;
                Reservation::Reserved(api)
            }
            ReservationRecord::Replay { handle, response } => {
                let api = api_handle(&handle);
                self.remember(&api, handle)?;
                Reservation::Replay {
                    handle: api,
                    response: api_response(response),
                }
            }
            ReservationRecord::Conflict => Reservation::Conflict,
            ReservationRecord::InProgress => Reservation::InProgress,
        })
    }

    async fn complete(
        &self,
        handle: &IdempotencyHandle,
        response: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        let storage = self.storage_handle(handle)?;
        self.repository
            .complete(&storage, storage_response(response))
            .await
            .map_err(|_| IdempotencyError::Infrastructure)?;
        self.forget(handle);
        Ok(())
    }

    async fn abort_in_progress(&self, handle: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        let storage = self.storage_handle(handle)?;
        self.repository
            .abort_in_progress(&storage)
            .await
            .map_err(|_| IdempotencyError::Infrastructure)?;
        self.forget(handle);
        Ok(())
    }

    async fn discard_completed(&self, handle: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        let storage = self.storage_handle(handle)?;
        self.repository
            .discard_completed(&storage)
            .await
            .map_err(|_| IdempotencyError::Infrastructure)?;
        self.forget(handle);
        Ok(())
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
        axum::serve(listener, self.router)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|_| ServiceError::Server)
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
        SeaOrmIdempotencyRepository::new(database),
    ));
    let state = ApiState::new(jobs, leases, clients, idempotency, readiness);

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
        .serve_with_shutdown(listener, std::future::pending())
        .await
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
