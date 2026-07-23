use std::{
    collections::HashMap,
    ffi::OsString,
    future, io,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::Request as AxumRequest,
};
use media::{
    composition::{
        StorageIdempotencyAdapter, StorageOperationCompletionAdapter, migrate, prepare_service,
    },
    config::{ConfigSource, DatabaseConfig, ServerConfig},
};
use media_api::{
    ApiState, IdempotencyError, IdempotencyHandle, IdempotencyRequest, IdempotencyStore,
    Reservation, StoredHttpResponse,
};
use media_contract::{JobDto, LeaseDto};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, BootstrapClient, ClientRole, ClientStore, CredentialDigest,
    JobApplication, LeaseApplication, RUNNER_CLIENT_ID, SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
use media_storage::{
    SeaOrmClientStore, SeaOrmIdempotencyRepository, SeaOrmJobStore, SeaOrmLeaseStore,
    SeaOrmOperationReceiptRepository, SeaOrmReadiness,
};
use reqwest::{Client, Response, StatusCode, header};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sha2::{Digest, Sha256};
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tokio::{net::TcpListener, sync::oneshot};
use tower::ServiceExt;

const POSTGRES_IMAGE: &str = "postgres";
const POSTGRES_TAG_AND_DIGEST: &str = concat!(
    "17-alpine@sha256:",
    "742f40ea20b9ff2ff31db5458d127452988a2164df9e17441e191f3b72252193"
);
const POSTGRES_PORT: u16 = 5432;
const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const PRIMARY_TOKEN: &str = "task-9-primary-token";
const SECONDARY_TOKEN: &str = "task-9-secondary-token";
const RUNNER_TOKEN: &str = "task-9-runner-token";

#[derive(Clone)]
struct FailingCompletionStore {
    inner: StorageIdempotencyAdapter,
}

#[derive(Clone)]
struct HangingCompletionStore {
    inner: StorageIdempotencyAdapter,
    entered: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    abort_calls: Arc<AtomicUsize>,
}

impl HangingCompletionStore {
    fn new(inner: StorageIdempotencyAdapter) -> (Self, oneshot::Receiver<()>, Arc<AtomicUsize>) {
        let (entered, receiver) = oneshot::channel();
        let abort_calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                inner,
                entered: Arc::new(Mutex::new(Some(entered))),
                abort_calls: abort_calls.clone(),
            },
            receiver,
            abort_calls,
        )
    }
}

#[async_trait::async_trait]
impl IdempotencyStore for HangingCompletionStore {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        self.inner.reserve(request).await
    }

    async fn complete(
        &self,
        _: &IdempotencyHandle,
        _: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        future::pending().await
    }

    async fn abort_in_progress(&self, handle: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        self.abort_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.abort_in_progress(handle).await
    }
}

#[async_trait::async_trait]
impl IdempotencyStore for FailingCompletionStore {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        self.inner.reserve(request).await
    }

    async fn complete(
        &self,
        _: &IdempotencyHandle,
        _: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        Err(IdempotencyError::Infrastructure)
    }

    async fn abort_in_progress(&self, handle: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        self.inner.abort_in_progress(handle).await
    }
}

fn storage_idempotency(database: &DatabaseConnection) -> StorageIdempotencyAdapter {
    StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(database.clone()))
}

fn storage_operations(database: &DatabaseConnection) -> StorageOperationCompletionAdapter {
    StorageOperationCompletionAdapter::new(SeaOrmOperationReceiptRepository::new(database.clone()))
}

fn token_digest(token: &str) -> CredentialDigest {
    let bytes: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    CredentialDigest::from(bytes)
}

async fn bootstrap_database_clients(database: &DatabaseConnection) {
    let store = SeaOrmClientStore::new(database.clone());
    for client in [
        (
            PRIMARY_CLIENT_ID,
            PRIMARY_USER_ID,
            PRIMARY_TOKEN,
            "test-hermes-primary",
        ),
        (
            SECONDARY_CLIENT_ID,
            SECONDARY_USER_ID,
            SECONDARY_TOKEN,
            "test-hermes-secondary",
        ),
    ] {
        store
            .upsert_client(
                BootstrapClient::new(
                    client.0,
                    client.3.to_owned(),
                    ClientRole::Hermes,
                    Some(client.1),
                    token_digest(client.2),
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }
    store
        .upsert_client(
            BootstrapClient::new(
                RUNNER_CLIENT_ID,
                "test-runner".to_owned(),
                ClientRole::Runner,
                None,
                token_digest(RUNNER_TOKEN),
            )
            .unwrap(),
        )
        .await
        .unwrap();
}

async fn mark_runner_ready(database: &DatabaseConnection) {
    database
        .execute_raw(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE runner_lifecycle SET state = 'ready', reason = NULL \
             WHERE singleton = true",
        ))
        .await
        .expect("test runner lifecycle must become ready");
}

fn database_router(
    database: &DatabaseConnection,
    idempotency: Arc<dyn IdempotencyStore>,
) -> axum::Router {
    let jobs = Arc::new(JobApplication::new(Arc::new(SeaOrmJobStore::new(
        database.clone(),
    ))));
    let leases = Arc::new(
        LeaseApplication::new(
            Arc::new(SeaOrmLeaseStore::new(database.clone())),
            time::Duration::seconds(60),
        )
        .unwrap(),
    );
    media_api::router(ApiState::new(
        jobs,
        leases,
        Arc::new(SeaOrmClientStore::new(database.clone())),
        idempotency,
        Arc::new(storage_operations(database)),
        Arc::new(SeaOrmReadiness::new(database.clone())),
    ))
}

fn post_request(path: &str, token: &str, key: &str, body: Body) -> AxumRequest<Body> {
    AxumRequest::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("idempotency-key", key)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn query_one(database: &DatabaseConnection, sql: &str) -> sea_orm::QueryResult {
    database
        .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
}

async fn crash_before_http_completion(
    app: axum::Router,
    request: AxumRequest<Body>,
    entered: oneshot::Receiver<()>,
    abort_calls: &AtomicUsize,
) {
    let task = tokio::spawn(async move { app.oneshot(request).await });
    tokio::time::timeout(TEST_TIMEOUT, entered)
        .await
        .expect("business mutation must reach HTTP completion")
        .expect("completion hook must remain alive");
    task.abort();
    assert!(
        task.await.unwrap_err().is_cancelled(),
        "request task must model abrupt process cancellation",
    );
    assert_eq!(abort_calls.load(Ordering::SeqCst), 0);
}

#[derive(Default)]
struct FakeSource {
    environment: HashMap<&'static str, OsString>,
    files: HashMap<PathBuf, Vec<u8>>,
}

impl FakeSource {
    fn set_secret(&mut self, name: &'static str, value: impl AsRef<[u8]>) {
        let path = PathBuf::from(format!("/{name}.secret"));
        self.environment.insert(name, path.clone().into_os_string());
        self.files.insert(path, value.as_ref().to_vec());
    }

    fn set_environment(&mut self, name: &'static str, value: impl Into<OsString>) {
        self.environment.insert(name, value.into());
    }
}

impl ConfigSource for FakeSource {
    fn var_os(&self, name: &'static str) -> Option<OsString> {
        self.environment.get(name).cloned()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing test secret"))
    }
}

fn database_config(url: &str) -> DatabaseConfig {
    let mut source = FakeSource::default();
    source.set_secret("MEDIA_DATABASE_URL_FILE", url);
    DatabaseConfig::load_from(&source).expect("database config must load")
}

fn server_config(url: &str, listen_addr: SocketAddr) -> ServerConfig {
    let mut source = FakeSource::default();
    source.set_secret("MEDIA_DATABASE_URL_FILE", url);
    source.set_secret("MEDIA_PRIMARY_TOKEN_FILE", PRIMARY_TOKEN);
    source.set_secret("MEDIA_SECONDARY_TOKEN_FILE", SECONDARY_TOKEN);
    source.set_secret("MEDIA_RUNNER_TOKEN_FILE", RUNNER_TOKEN);
    source.set_secret("MEDIA_LIFECYCLE_TOKEN_FILE", "lifecycle-token");
    source.set_environment("MEDIA_LISTEN_ADDR", listen_addr.to_string());
    ServerConfig::load_from(&source).expect("server config must load")
}

fn authenticated(
    client: &Client,
    method: reqwest::Method,
    url: impl reqwest::IntoUrl,
    token: &str,
) -> reqwest::RequestBuilder {
    client
        .request(method, url)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
}

async fn send(request: reqwest::RequestBuilder) -> Response {
    tokio::time::timeout(TEST_TIMEOUT, request.send())
        .await
        .expect("HTTP request must complete within the test timeout")
        .expect("HTTP request must succeed")
}

#[tokio::test]
async fn postgres_api_foundation_works_end_to_end() {
    let container = GenericImage::new(POSTGRES_IMAGE, POSTGRES_TAG_AND_DIGEST)
        .with_exposed_port(POSTGRES_PORT.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_DB", "media_orchestrator")
        .with_env_var("POSTGRES_USER", "media")
        .with_env_var("POSTGRES_PASSWORD", "media-test-password")
        .start()
        .await
        .expect("Docker must run the pinned PostgreSQL image");
    let port = container
        .get_host_port_ipv4(POSTGRES_PORT.tcp())
        .await
        .expect("PostgreSQL must expose its port");
    let database_url =
        format!("postgres://media:media-test-password@127.0.0.1:{port}/media_orchestrator");

    migrate(&database_config(&database_url))
        .await
        .expect("explicit migrations must apply");
    let database = Database::connect(&database_url).await.unwrap();
    mark_runner_ready(&database).await;
    database.close().await.unwrap();
    let config = server_config(&database_url, "127.0.0.1:0".parse().unwrap());
    let service = prepare_service(&config)
        .await
        .expect("service must bootstrap fixed clients");
    let listener = TcpListener::bind(config.listen_addr()).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receiver) = oneshot::channel();
    let service_task = tokio::spawn(service.serve_with_shutdown(listener, async move {
        let _ = receiver.await;
    }));

    let client = Client::new();
    let base_url = format!("http://{address}");
    let create_body = serde_json::json!({
        "provider": "rezka",
        "result_ref": "rezka:series:42:season:1",
        "notify_scope": "initiator"
    });
    let create = || {
        authenticated(
            &client,
            reqwest::Method::POST,
            format!("{base_url}/v1/jobs"),
            PRIMARY_TOKEN,
        )
        .header("idempotency-key", "task-9-create-job")
        .json(&create_body)
    };

    let created = send(create()).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_content_type = created.headers()[header::CONTENT_TYPE].clone();
    let created_body = created.bytes().await.unwrap();
    let job: JobDto = serde_json::from_slice(&created_body).unwrap();

    let replayed = send(create()).await;
    assert_eq!(replayed.status(), StatusCode::CREATED);
    assert_eq!(
        replayed.headers()[header::CONTENT_TYPE],
        created_content_type
    );
    assert_eq!(replayed.bytes().await.unwrap(), created_body);

    let owner_read = send(authenticated(
        &client,
        reqwest::Method::GET,
        format!("{base_url}/v1/jobs/{}", job.id),
        PRIMARY_TOKEN,
    ))
    .await;
    assert_eq!(owner_read.status(), StatusCode::OK);
    assert_eq!(owner_read.json::<JobDto>().await.unwrap(), job);

    let cross_owner_read = send(authenticated(
        &client,
        reqwest::Method::GET,
        format!("{base_url}/v1/jobs/{}", job.id),
        SECONDARY_TOKEN,
    ))
    .await;
    assert_eq!(cross_owner_read.status(), StatusCode::NOT_FOUND);

    let lease = |key: &'static str| {
        authenticated(
            &client,
            reqwest::Method::POST,
            format!("{base_url}/v1/runner/leases"),
            RUNNER_TOKEN,
        )
        .header("idempotency-key", key)
    };
    let (first_lease, second_lease) = tokio::join!(
        send(lease("task-9-lease-first")),
        send(lease("task-9-lease-second"))
    );
    let (winner, loser) = match (first_lease.status(), second_lease.status()) {
        (StatusCode::OK, StatusCode::NO_CONTENT) => (first_lease, second_lease),
        (StatusCode::NO_CONTENT, StatusCode::OK) => (second_lease, first_lease),
        statuses => panic!("expected exactly one lease winner, got {statuses:?}"),
    };
    assert_eq!(loser.bytes().await.unwrap().len(), 0);
    let lease: LeaseDto = winner.json().await.unwrap();
    assert_eq!(lease.job.id, job.id);

    let heartbeat = send(
        authenticated(
            &client,
            reqwest::Method::POST,
            format!("{base_url}/v1/runner/leases/{}/heartbeat", lease.lease_id),
            RUNNER_TOKEN,
        )
        .header("idempotency-key", "task-9-heartbeat"),
    )
    .await;
    assert_eq!(heartbeat.status(), StatusCode::OK);
    let heartbeat_lease: LeaseDto = heartbeat.json().await.unwrap();
    assert_eq!(heartbeat_lease.lease_id, lease.lease_id);
    assert_eq!(heartbeat_lease.job.id, job.id);

    let queue = send(authenticated(
        &client,
        reqwest::Method::GET,
        format!("{base_url}/v1/queue/status"),
        PRIMARY_TOKEN,
    ))
    .await;
    assert_eq!(queue.status(), StatusCode::OK);
    assert_eq!(
        queue.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({ "queued": 0, "active": true, "runner_state": "ready" })
    );

    shutdown.send(()).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, service_task)
        .await
        .expect("service shutdown must be bounded")
        .expect("service task must not panic")
        .expect("service must stop cleanly");
    drop(client);
    container
        .rm()
        .await
        .expect("PostgreSQL test container must be removed synchronously");
}

#[tokio::test]
async fn postgres_mutations_reenter_after_replay_completion_failure_across_router_instances() {
    let container = GenericImage::new(POSTGRES_IMAGE, POSTGRES_TAG_AND_DIGEST)
        .with_exposed_port(POSTGRES_PORT.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_DB", "media_orchestrator")
        .with_env_var("POSTGRES_USER", "media")
        .with_env_var("POSTGRES_PASSWORD", "media-test-password")
        .start()
        .await
        .expect("Docker must run the pinned PostgreSQL image");
    let port = container
        .get_host_port_ipv4(POSTGRES_PORT.tcp())
        .await
        .expect("PostgreSQL must expose its port");
    let database_url =
        format!("postgres://media:media-test-password@127.0.0.1:{port}/media_orchestrator");
    migrate(&database_config(&database_url)).await.unwrap();
    let database = Database::connect(&database_url).await.unwrap();
    bootstrap_database_clients(&database).await;
    mark_runner_ready(&database).await;
    let failing = || {
        database_router(
            &database,
            Arc::new(FailingCompletionStore {
                inner: storage_idempotency(&database),
            }),
        )
    };
    let completing = || database_router(&database, Arc::new(storage_idempotency(&database)));

    let create_body =
        r#"{"provider":"rezka","result_ref":"crash-window-job","notify_scope":"initiator"}"#;
    let first_create = failing()
        .oneshot(post_request(
            "/v1/jobs",
            PRIMARY_TOKEN,
            "crash-window-create",
            Body::from(create_body),
        ))
        .await
        .unwrap();
    assert_eq!(first_create.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let persisted_job_id = query_one(
        &database,
        "SELECT id::text AS id FROM jobs WHERE result_ref = 'crash-window-job'",
    )
    .await
    .try_get::<String>("", "id")
    .unwrap();

    let retried_create = completing()
        .oneshot(post_request(
            "/v1/jobs",
            PRIMARY_TOKEN,
            "crash-window-create",
            Body::from(create_body),
        ))
        .await
        .unwrap();
    assert_eq!(retried_create.status(), StatusCode::CREATED);
    let retried_job: JobDto = serde_json::from_slice(
        &to_bytes(retried_create.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(retried_job.id.to_string(), persisted_job_id);

    let first_lease = failing()
        .oneshot(post_request(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "crash-window-lease",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(first_lease.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let lease_row = query_one(
        &database,
        "SELECT id::text AS id, expires_at FROM job_leases WHERE slot = 1",
    )
    .await;
    let persisted_lease_id = lease_row.try_get::<String>("", "id").unwrap();
    let leased_at = lease_row
        .try_get::<time::OffsetDateTime>("", "expires_at")
        .unwrap();

    let retried_lease = completing()
        .oneshot(post_request(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "crash-window-lease",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(retried_lease.status(), StatusCode::OK);
    let retried_lease: LeaseDto = serde_json::from_slice(
        &to_bytes(retried_lease.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(retried_lease.lease_id.to_string(), persisted_lease_id);
    assert_eq!(
        query_one(
            &database,
            "SELECT expires_at FROM job_leases WHERE slot = 1",
        )
        .await
        .try_get::<time::OffsetDateTime>("", "expires_at")
        .unwrap(),
        leased_at,
    );

    let heartbeat_path = format!("/v1/runner/leases/{}/heartbeat", retried_lease.lease_id);
    let first_heartbeat = failing()
        .oneshot(post_request(
            &heartbeat_path,
            RUNNER_TOKEN,
            "crash-window-heartbeat",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(first_heartbeat.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let heartbeat_expiry = query_one(
        &database,
        "SELECT expires_at FROM job_leases WHERE slot = 1",
    )
    .await
    .try_get::<time::OffsetDateTime>("", "expires_at")
    .unwrap();

    let retried_heartbeat = completing()
        .oneshot(post_request(
            &heartbeat_path,
            RUNNER_TOKEN,
            "crash-window-heartbeat",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(retried_heartbeat.status(), StatusCode::OK);
    let retried_heartbeat: LeaseDto = serde_json::from_slice(
        &to_bytes(retried_heartbeat.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(retried_heartbeat.lease_id, retried_lease.lease_id);
    assert_eq!(
        query_one(
            &database,
            "SELECT expires_at FROM job_leases WHERE slot = 1",
        )
        .await
        .try_get::<time::OffsetDateTime>("", "expires_at")
        .unwrap(),
        heartbeat_expiry,
    );

    let counts = query_one(
        &database,
        "SELECT (SELECT count(*)::bigint FROM jobs) AS jobs, \
         (SELECT count(*)::bigint FROM operation_receipts) AS receipts, \
         (SELECT attempt_count FROM jobs WHERE result_ref = 'crash-window-job') AS attempts",
    )
    .await;
    assert_eq!(counts.try_get::<i64>("", "jobs").unwrap(), 1);
    assert_eq!(counts.try_get::<i64>("", "receipts").unwrap(), 3);
    assert_eq!(counts.try_get::<i32>("", "attempts").unwrap(), 1);
    database
        .close()
        .await
        .expect("database pool must close before container removal");
    container
        .rm()
        .await
        .expect("PostgreSQL test container must be removed synchronously");
}

#[tokio::test]
async fn stranded_http_reservations_reconcile_completed_mutations_without_abort() {
    let container = GenericImage::new(POSTGRES_IMAGE, POSTGRES_TAG_AND_DIGEST)
        .with_exposed_port(POSTGRES_PORT.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_DB", "media_orchestrator")
        .with_env_var("POSTGRES_USER", "media")
        .with_env_var("POSTGRES_PASSWORD", "media-test-password")
        .start()
        .await
        .expect("Docker must run the pinned PostgreSQL image");
    let port = container
        .get_host_port_ipv4(POSTGRES_PORT.tcp())
        .await
        .expect("PostgreSQL must expose its port");
    let database_url =
        format!("postgres://media:media-test-password@127.0.0.1:{port}/media_orchestrator");
    migrate(&database_config(&database_url)).await.unwrap();
    let database = Database::connect(&database_url).await.unwrap();
    bootstrap_database_clients(&database).await;
    mark_runner_ready(&database).await;
    let completing = || database_router(&database, Arc::new(storage_idempotency(&database)));

    let create_body =
        r#"{"provider":"rezka","result_ref":"stranded-create","notify_scope":"initiator"}"#;
    let (hanging, entered, aborts) = HangingCompletionStore::new(storage_idempotency(&database));
    crash_before_http_completion(
        database_router(&database, Arc::new(hanging)),
        post_request(
            "/v1/jobs",
            PRIMARY_TOKEN,
            "stranded-create",
            Body::from(create_body),
        ),
        entered,
        aborts.as_ref(),
    )
    .await;
    assert_eq!(
        query_one(
            &database,
            "SELECT status FROM idempotency_records \
             WHERE idempotency_key = 'stranded-create'",
        )
        .await
        .try_get::<String>("", "status")
        .unwrap(),
        "in_progress",
    );
    let persisted_job_id = query_one(
        &database,
        "SELECT id::text AS id FROM jobs WHERE result_ref = 'stranded-create'",
    )
    .await
    .try_get::<String>("", "id")
    .unwrap();

    let retried_create = completing()
        .oneshot(post_request(
            "/v1/jobs",
            PRIMARY_TOKEN,
            "stranded-create",
            Body::from(create_body),
        ))
        .await
        .unwrap();
    assert_eq!(retried_create.status(), StatusCode::CREATED);
    let retried_create_body = to_bytes(retried_create.into_body(), usize::MAX)
        .await
        .unwrap();
    let retried_job: JobDto = serde_json::from_slice(&retried_create_body).unwrap();
    assert_eq!(retried_job.id.to_string(), persisted_job_id);
    let replayed_create = completing()
        .oneshot(post_request(
            "/v1/jobs",
            PRIMARY_TOKEN,
            "stranded-create",
            Body::from(create_body),
        ))
        .await
        .unwrap();
    assert_eq!(replayed_create.status(), StatusCode::CREATED);
    assert_eq!(
        to_bytes(replayed_create.into_body(), usize::MAX)
            .await
            .unwrap(),
        retried_create_body,
    );

    let (hanging, entered, aborts) = HangingCompletionStore::new(storage_idempotency(&database));
    crash_before_http_completion(
        database_router(&database, Arc::new(hanging)),
        post_request(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "stranded-lease",
            Body::empty(),
        ),
        entered,
        aborts.as_ref(),
    )
    .await;
    let lease_row = query_one(
        &database,
        "SELECT id::text AS id, expires_at FROM job_leases WHERE slot = 1",
    )
    .await;
    let persisted_lease_id = lease_row.try_get::<String>("", "id").unwrap();
    let persisted_lease_expiry = lease_row
        .try_get::<time::OffsetDateTime>("", "expires_at")
        .unwrap();

    let retried_lease = completing()
        .oneshot(post_request(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "stranded-lease",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(retried_lease.status(), StatusCode::OK);
    let retried_lease_body = to_bytes(retried_lease.into_body(), usize::MAX)
        .await
        .unwrap();
    let retried_lease: LeaseDto = serde_json::from_slice(&retried_lease_body).unwrap();
    assert_eq!(retried_lease.lease_id.to_string(), persisted_lease_id);
    assert_eq!(
        query_one(
            &database,
            "SELECT expires_at FROM job_leases WHERE slot = 1",
        )
        .await
        .try_get::<time::OffsetDateTime>("", "expires_at")
        .unwrap(),
        persisted_lease_expiry,
    );
    let replayed_lease = completing()
        .oneshot(post_request(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "stranded-lease",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(replayed_lease.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(replayed_lease.into_body(), usize::MAX)
            .await
            .unwrap(),
        retried_lease_body,
    );

    let heartbeat_path = format!("/v1/runner/leases/{}/heartbeat", retried_lease.lease_id);
    let (hanging, entered, aborts) = HangingCompletionStore::new(storage_idempotency(&database));
    crash_before_http_completion(
        database_router(&database, Arc::new(hanging)),
        post_request(
            &heartbeat_path,
            RUNNER_TOKEN,
            "stranded-heartbeat",
            Body::empty(),
        ),
        entered,
        aborts.as_ref(),
    )
    .await;
    let persisted_heartbeat_expiry = query_one(
        &database,
        "SELECT expires_at FROM job_leases WHERE slot = 1",
    )
    .await
    .try_get::<time::OffsetDateTime>("", "expires_at")
    .unwrap();

    let retried_heartbeat = completing()
        .oneshot(post_request(
            &heartbeat_path,
            RUNNER_TOKEN,
            "stranded-heartbeat",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(retried_heartbeat.status(), StatusCode::OK);
    let retried_heartbeat_body = to_bytes(retried_heartbeat.into_body(), usize::MAX)
        .await
        .unwrap();
    let retried_heartbeat: LeaseDto = serde_json::from_slice(&retried_heartbeat_body).unwrap();
    assert_eq!(retried_heartbeat.lease_id, retried_lease.lease_id);
    assert_eq!(
        query_one(
            &database,
            "SELECT expires_at FROM job_leases WHERE slot = 1",
        )
        .await
        .try_get::<time::OffsetDateTime>("", "expires_at")
        .unwrap(),
        persisted_heartbeat_expiry,
    );
    let replayed_heartbeat = completing()
        .oneshot(post_request(
            &heartbeat_path,
            RUNNER_TOKEN,
            "stranded-heartbeat",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(replayed_heartbeat.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(replayed_heartbeat.into_body(), usize::MAX)
            .await
            .unwrap(),
        retried_heartbeat_body,
    );

    let counts = query_one(
        &database,
        "SELECT (SELECT count(*)::bigint FROM jobs) AS jobs, \
         (SELECT count(*)::bigint FROM operation_receipts) AS receipts, \
         (SELECT count(*)::bigint FROM idempotency_records \
          WHERE status = 'completed') AS completed_http, \
         (SELECT attempt_count FROM jobs WHERE result_ref = 'stranded-create') AS attempts",
    )
    .await;
    assert_eq!(counts.try_get::<i64>("", "jobs").unwrap(), 1);
    assert_eq!(counts.try_get::<i64>("", "receipts").unwrap(), 3);
    assert_eq!(counts.try_get::<i64>("", "completed_http").unwrap(), 3);
    assert_eq!(counts.try_get::<i32>("", "attempts").unwrap(), 1);
    database
        .close()
        .await
        .expect("database pool must close before container removal");
    container
        .rm()
        .await
        .expect("PostgreSQL test container must be removed synchronously");
}
