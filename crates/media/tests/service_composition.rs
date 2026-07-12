use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use media::{
    composition::{ServiceError, StorageIdempotencyAdapter, migrate, prepare_service, serve},
    config::{ConfigSource, DatabaseConfig, ServerConfig},
};
use media_api::{
    IdempotencyError, IdempotencyGeneration, IdempotencyHandle, IdempotencyRequest,
    IdempotencyStore, Reservation, StoredHttpResponse,
};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, BootstrapClient, ClientRole, ClientStore, CredentialDigest,
    RUNNER_CLIENT_ID, SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
use media_storage::{Migrator, SeaOrmClientStore, SeaOrmIdempotencyRepository};
use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
#[cfg(unix)]
use wait_timeout::ChildExt;

const POSTGRES_IMAGE: &str = "postgres";
const POSTGRES_TAG_AND_DIGEST: &str = concat!(
    "17-alpine@sha256:",
    "742f40ea20b9ff2ff31db5458d127452988a2164df9e17441e191f3b72252193"
);
const POSTGRES_PORT: u16 = 5432;
const TEST_TIMEOUT: Duration = Duration::from_secs(5);
static SECRET_FILE_ID: AtomicU64 = AtomicU64::new(0);

struct SecretFile(PathBuf);

impl SecretFile {
    fn new(contents: &str) -> Self {
        let id = SECRET_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "media-service-composition-{}-{id}.secret",
            std::process::id()
        ));
        std::fs::write(&path, contents).unwrap();
        Self(path)
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct TestDatabase {
    connection: DatabaseConnection,
    url: String,
    container: ContainerAsync<GenericImage>,
}

impl TestDatabase {
    async fn start() -> Self {
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
        let url =
            format!("postgres://media:media-test-password@127.0.0.1:{port}/media_orchestrator");
        let connection = Database::connect(&url)
            .await
            .expect("PostgreSQL must accept connections");

        Self {
            connection,
            url,
            container,
        }
    }

    async fn connect(&self) -> DatabaseConnection {
        Database::connect(&self.url)
            .await
            .expect("an independent database connection must open")
    }

    async fn shutdown(self) {
        self.connection
            .close()
            .await
            .expect("root database pool must close");
        self.container
            .rm()
            .await
            .expect("PostgreSQL test container must be removed synchronously");
    }
}

#[cfg(unix)]
fn wait_for_output(mut child: Child, timeout: Duration) -> Result<Output, String> {
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("child did not stop within {timeout:?}"));
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("child wait failed: {error}"));
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .expect("child stdout must be piped")
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .expect("child stderr must be piped")
        .read_to_end(&mut stderr)
        .unwrap();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(unix)]
async fn wait_until_healthy(child: &mut Child, address: SocketAddr) {
    for _ in 0..100 {
        assert!(
            child.try_wait().unwrap().is_none(),
            "serve process exited before becoming healthy"
        );
        if reqwest::get(format!("http://{address}/v1/health"))
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("serve process did not become healthy");
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
    source.set_secret("MEDIA_PRIMARY_TOKEN_FILE", "primary-token-task-8b");
    source.set_secret("MEDIA_SECONDARY_TOKEN_FILE", "secondary-token-task-8b");
    source.set_secret("MEDIA_RUNNER_TOKEN_FILE", "runner-token-task-8b");
    source.set_secret("MEDIA_LIFECYCLE_TOKEN_FILE", "lifecycle-token-task-8b");
    source.set_environment("MEDIA_LISTEN_ADDR", listen_addr.to_string());
    ServerConfig::load_from(&source).expect("server config must load")
}

fn request(key: &str, fingerprint: u8) -> IdempotencyRequest {
    IdempotencyRequest::new(PRIMARY_CLIENT_ID, key.to_owned(), [fingerprint; 32])
}

fn digest(token: &str) -> CredentialDigest {
    let bytes: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    CredentialDigest::from(bytes)
}

async fn bootstrap_database_clients(database: &DatabaseConnection) {
    let store = SeaOrmClientStore::new(database.clone());
    for client in [
        BootstrapClient::new(
            PRIMARY_CLIENT_ID,
            "hermes-primary".to_owned(),
            ClientRole::Hermes,
            Some(PRIMARY_USER_ID),
            digest("primary-token-task-8b"),
        )
        .unwrap(),
        BootstrapClient::new(
            SECONDARY_CLIENT_ID,
            "hermes-secondary".to_owned(),
            ClientRole::Hermes,
            Some(SECONDARY_USER_ID),
            digest("secondary-token-task-8b"),
        )
        .unwrap(),
        BootstrapClient::new(
            RUNNER_CLIENT_ID,
            "runner".to_owned(),
            ClientRole::Runner,
            None,
            digest("runner-token-task-8b"),
        )
        .unwrap(),
    ] {
        store.upsert_client(client).await.unwrap();
    }
}

async fn migrated_service() -> (TestDatabase, ServerConfig) {
    let database = TestDatabase::start().await;
    migrate(&database_config(&database.url))
        .await
        .expect("explicit migration command must apply the schema");
    let config = server_config(&database.url, "127.0.0.1:0".parse().unwrap());
    bootstrap_database_clients(&database.connection).await;
    (database, config)
}

async fn reserved(
    adapter: &StorageIdempotencyAdapter,
    key: &str,
    fingerprint: u8,
) -> IdempotencyHandle {
    match adapter.reserve(request(key, fingerprint)).await.unwrap() {
        Reservation::Reserved(handle) => handle,
        other => panic!("expected reservation, got {other:?}"),
    }
}

#[tokio::test]
async fn migrate_applies_the_explicit_schema() {
    let database = TestDatabase::start().await;

    migrate(&database_config(&database.url))
        .await
        .expect("media migrate must apply every explicit migration");

    assert!(
        Migrator::get_pending_migrations(&database.connection)
            .await
            .unwrap()
            .is_empty()
    );
    database.shutdown().await;
}

#[tokio::test]
async fn migrate_command_loads_only_the_database_config_and_applies_the_schema() {
    let database = TestDatabase::start().await;
    let secret = SecretFile::new(&database.url);
    let mut command = assert_cmd::cargo::cargo_bin_cmd!("media");
    command
        .env_clear()
        .env("MEDIA_DATABASE_URL_FILE", &secret.0)
        .arg("migrate");
    let output: Output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();

    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!rendered.contains(&database.url));
    assert!(
        Migrator::get_pending_migrations(&database.connection)
            .await
            .unwrap()
            .is_empty()
    );
    database.shutdown().await;
}

#[tokio::test]
async fn pending_migrations_fail_before_an_unavailable_listener_is_bound() {
    let database = TestDatabase::start().await;
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = server_config(&database.url, occupied.local_addr().unwrap());

    let error = serve(config)
        .await
        .expect_err("pending migrations must stop startup");

    assert_eq!(error, ServiceError::PendingMigrations);
    drop(occupied);
    database.shutdown().await;
}

#[tokio::test]
async fn startup_bootstraps_fixed_clients_with_sha256_digests_without_secret_leakage() {
    let database = TestDatabase::start().await;
    migrate(&database_config(&database.url)).await.unwrap();
    let config = server_config(&database.url, "127.0.0.1:0".parse().unwrap());

    let prepared = prepare_service(&config)
        .await
        .expect("service preparation must be repeatable");
    let rendered = format!("{prepared:?}");
    for secret in [
        config.database_url().expose_secret(),
        config.primary_token().expose_secret(),
        config.secondary_token().expose_secret(),
        config.runner_token().expose_secret(),
    ] {
        assert!(!rendered.contains(secret));
    }

    let clients = SeaOrmClientStore::new(database.connection.clone());
    assert_eq!(
        clients
            .find_by_digest(digest("primary-token-task-8b"))
            .await
            .unwrap()
            .unwrap()
            .client_id(),
        PRIMARY_CLIENT_ID,
    );
    assert_eq!(
        clients
            .find_by_digest(digest("secondary-token-task-8b"))
            .await
            .unwrap()
            .unwrap()
            .client_id(),
        SECONDARY_CLIENT_ID,
    );
    assert_eq!(
        clients
            .find_by_digest(digest("runner-token-task-8b"))
            .await
            .unwrap()
            .unwrap()
            .client_id(),
        RUNNER_CLIENT_ID,
    );
    drop(clients);
    drop(prepared);
    database.shutdown().await;
}

#[tokio::test]
async fn storage_adapter_maps_every_reservation_variant_and_preserves_generations() {
    let (database, _) = migrated_service().await;
    let adapter = StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(
        database.connection.clone(),
    ));

    let in_progress = reserved(&adapter, "in-progress", 1).await;
    assert!(matches!(
        adapter.reserve(request("in-progress", 1)).await.unwrap(),
        Reservation::InProgress(_)
    ));
    assert!(matches!(
        adapter.reserve(request("in-progress", 2)).await.unwrap(),
        Reservation::Conflict
    ));

    let replay_source = reserved(&adapter, "replay", 3).await;
    adapter
        .complete(
            &replay_source,
            StoredHttpResponse::new(201, String::new(), b"created".to_vec()),
        )
        .await
        .unwrap();
    match adapter.reserve(request("replay", 3)).await.unwrap() {
        Reservation::Replay { handle, response } => {
            assert_eq!(handle.generation(), replay_source.generation());
            assert_eq!(response.status(), 201);
            assert_eq!(response.content_type(), "");
            assert_eq!(response.body(), b"created");
        }
        other => panic!("expected replay, got {other:?}"),
    };

    adapter.abort_in_progress(&in_progress).await.unwrap();
    drop(adapter);
    database.shutdown().await;
}

#[tokio::test]
async fn storage_adapter_forwards_the_exact_generation_for_complete_and_abort() {
    let (database, _) = migrated_service().await;
    let adapter = StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(
        database.connection.clone(),
    ));

    let abort_handle = reserved(&adapter, "abort", 4).await;
    let wrong_abort = IdempotencyHandle::new(
        request("abort", 4),
        IdempotencyGeneration::from_uuid(uuid::Uuid::new_v4()),
    );
    assert_eq!(
        adapter.abort_in_progress(&wrong_abort).await,
        Err(IdempotencyError::Conflict)
    );
    assert!(matches!(
        adapter.reserve(request("abort", 4)).await.unwrap(),
        Reservation::InProgress(_)
    ));
    adapter.abort_in_progress(&abort_handle).await.unwrap();

    let complete_handle = reserved(&adapter, "complete", 5).await;
    let wrong_complete = IdempotencyHandle::new(
        request("complete", 5),
        IdempotencyGeneration::from_uuid(uuid::Uuid::new_v4()),
    );
    let response = StoredHttpResponse::new(204, String::new(), Vec::new());
    assert_eq!(
        adapter.complete(&wrong_complete, response.clone()).await,
        Err(IdempotencyError::Conflict)
    );
    adapter.complete(&complete_handle, response).await.unwrap();
    let replay_handle = match adapter.reserve(request("complete", 5)).await.unwrap() {
        Reservation::Replay { handle, .. } => handle,
        other => panic!("expected replay, got {other:?}"),
    };
    assert_eq!(replay_handle.generation(), complete_handle.generation());
    assert!(matches!(
        adapter.reserve(request("complete", 5)).await.unwrap(),
        Reservation::Replay { .. }
    ));
    drop(adapter);
    database.shutdown().await;
}

#[tokio::test]
async fn storage_adapter_lifecycle_is_stateless_across_adapter_instances() {
    let (database, _) = migrated_service().await;
    let reserving = StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(
        database.connection.clone(),
    ));
    let handle = reserved(&reserving, "stateless", 6).await;
    drop(reserving);

    StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(
        database.connection.clone(),
    ))
    .complete(
        &handle,
        StoredHttpResponse::new(204, String::new(), Vec::new()),
    )
    .await
    .expect("a fresh adapter must reconstruct the exact storage handle");
    let replay = match StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(
        database.connection.clone(),
    ))
    .reserve(request("stateless", 6))
    .await
    .unwrap()
    {
        Reservation::Replay { handle, .. } => handle,
        other => panic!("expected replay, got {other:?}"),
    };
    assert_eq!(replay.generation(), handle.generation());

    let aborting = StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(
        database.connection.clone(),
    ));
    let abort_handle = reserved(&aborting, "stateless-abort", 7).await;
    drop(aborting);
    StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(
        database.connection.clone(),
    ))
    .abort_in_progress(&abort_handle)
    .await
    .expect("a fresh adapter must abort by the exact reservation generation");
    database.shutdown().await;
}

#[tokio::test]
async fn storage_adapter_keeps_infrastructure_failures_distinct_from_conflicts() {
    let (database, _) = migrated_service().await;
    let closed = database.connect().await;
    closed.close_by_ref().await.unwrap();
    let adapter = StorageIdempotencyAdapter::new(SeaOrmIdempotencyRepository::new(closed));
    let handle = IdempotencyHandle::new(request("disconnected", 8), IdempotencyGeneration::new());

    assert_eq!(
        adapter.abort_in_progress(&handle).await,
        Err(IdempotencyError::Infrastructure)
    );
    drop(adapter);
    database.shutdown().await;
}

#[tokio::test]
async fn prepared_service_serves_on_an_ephemeral_loopback_listener_and_stops_gracefully() {
    let (database, config) = migrated_service().await;
    let prepared = prepare_service(&config).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receiver) = oneshot::channel();
    let task = tokio::spawn(prepared.serve_with_shutdown(listener, async move {
        let _ = receiver.await;
    }));

    let response = tokio::time::timeout(
        TEST_TIMEOUT,
        reqwest::get(format!("http://{address}/v1/health")),
    )
    .await
    .expect("listener must accept requests")
    .expect("health request must complete");
    assert!(response.status().is_success());
    let client = reqwest::Client::new();
    let created = client
        .post(format!("http://{address}/v1/tracking"))
        .bearer_auth("primary-token-task-8b")
        .header("idempotency-key", "service-tracking-add")
        .json(&serde_json::json!({
            "provider": "rezka",
            "title": "Ongoing Show",
            "translation": "Studio Dub",
            "known_episodes": [{"season": 1, "episode": 4}],
            "scope": "family",
            "series_ongoing": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let listed: serde_json::Value = client
        .get(format!("http://{address}/v1/tracking"))
        .bearer_auth("secondary-token-task-8b")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["tracking"].as_array().unwrap().len(), 1);
    assert!(listed["tracking"][0].get("owner_id").is_none());
    shutdown.send(()).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, task)
        .await
        .expect("service shutdown must be bounded")
        .expect("service task must not panic")
        .expect("service must stop cleanly");
    database.shutdown().await;
}

#[tokio::test]
async fn graceful_shutdown_deadline_terminates_a_never_finishing_connection() {
    let (database, config) = migrated_service().await;
    let prepared = prepare_service(&config).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receiver) = oneshot::channel();
    let task = tokio::spawn(prepared.serve_with_shutdown_timeout(
        listener,
        async move {
            let _ = receiver.await;
        },
        Duration::from_millis(20),
    ));

    let mut connection = TcpStream::connect(address).await.unwrap();
    connection
        .write_all(b"GET /v1/health HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\n\r\n")
        .await
        .unwrap();
    assert!(
        reqwest::get(format!("http://{address}/v1/health"))
            .await
            .unwrap()
            .status()
            .is_success()
    );

    shutdown.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_millis(250), task)
        .await
        .expect("the hard shutdown deadline must bound the serving future")
        .expect("the service task must not panic");

    assert_eq!(result, Err(ServiceError::ShutdownTimeout));
    drop(connection);
    database.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn serve_process_emits_json_tracing_and_gracefully_handles_int_and_term() {
    let (database, _) = migrated_service().await;
    let database_url = SecretFile::new(&database.url);
    let primary = SecretFile::new("json-primary-token");
    let secondary = SecretFile::new("json-secondary-token");
    let runner = SecretFile::new("json-runner-token");
    let lifecycle = SecretFile::new("json-lifecycle-token");

    for signal in ["INT", "TERM"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut child = Command::new(assert_cmd::cargo::cargo_bin!("media"))
            .arg("serve")
            .env_clear()
            .env("RUST_LOG", "media=info")
            .env("MEDIA_DATABASE_URL_FILE", &database_url.0)
            .env("MEDIA_PRIMARY_TOKEN_FILE", &primary.0)
            .env("MEDIA_SECONDARY_TOKEN_FILE", &secondary.0)
            .env("MEDIA_RUNNER_TOKEN_FILE", &runner.0)
            .env("MEDIA_LIFECYCLE_TOKEN_FILE", &lifecycle.0)
            .env("MEDIA_LISTEN_ADDR", address.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_until_healthy(&mut child, address).await;
        let pid = child.id().to_string();
        assert!(
            Command::new("kill")
                .args([format!("-{signal}"), pid])
                .status()
                .unwrap()
                .success()
        );
        let output =
            tokio::task::spawn_blocking(move || wait_for_output(child, Duration::from_secs(2)))
                .await
                .unwrap()
                .unwrap();

        assert!(
            output.status.success(),
            "{signal} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rendered = format!(
            "{}{}",
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap()
        );
        let events: Vec<serde_json::Value> = rendered
            .lines()
            .map(|line| serde_json::from_str(line).expect("every tracing event must be JSON"))
            .collect();
        assert!(
            events
                .iter()
                .any(|event| { event["fields"]["message"] == "media service listening" })
        );
        for secret in [
            database.url.as_str(),
            "json-primary-token",
            "json-secondary-token",
            "json-runner-token",
        ] {
            assert!(!rendered.contains(secret), "tracing exposed {secret}");
        }
    }
    database.shutdown().await;
}
