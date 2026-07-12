//! CLI-level tests for human-readable output (the default, without `--json`).
//!
//! These drive the real `media` binary against a mock service and assert that
//! the rendered text is produced, while `--json` still prints the raw
//! machine contract verbatim.

use std::{
    future::IntoFuture,
    path::PathBuf,
    process::Output,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use axum::body::Body;
use axum::{Router, http::StatusCode, response::Response, routing::any};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

static SECRET_FILE_ID: AtomicU64 = AtomicU64::new(0);
const CLI_TIMEOUT: Duration = Duration::from_secs(5);

struct SecretFile(PathBuf);

impl SecretFile {
    fn new(contents: &str) -> Self {
        let id = SECRET_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "media-render-cli-{}-{id}.secret",
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

struct TestServer {
    service_url: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), std::io::Error>>,
}

impl TestServer {
    async fn start(router: Router) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .into_future(),
        );
        Self {
            service_url: format!("http://{address}"),
            shutdown: Some(shutdown),
            task,
        }
    }

    async fn stop(mut self) {
        self.shutdown.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .expect("test server must stop within two seconds")
            .expect("test server task must not panic")
            .expect("test server must shut down cleanly");
    }
}

fn command(
    server: &TestServer,
    token_file: &SecretFile,
    args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
) -> assert_cmd::Command {
    let mut command = assert_cmd::cargo::cargo_bin_cmd!("media");
    command
        .env_clear()
        .env("MEDIA_SERVICE_URL", &server.service_url)
        .env("MEDIA_TOKEN_FILE", &token_file.0)
        .args(args);
    command
}

async fn command_output(mut command: assert_cmd::Command) -> Output {
    command.timeout(CLI_TIMEOUT);
    tokio::task::spawn_blocking(move || command.output())
        .await
        .expect("CLI subprocess task must finish")
        .expect("CLI subprocess must start")
}

fn json_response(status: StatusCode, body: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[tokio::test]
async fn queue_status_renders_human_block_by_default() {
    let router = Router::new().route(
        "/v1/queue/status",
        any(|| async { json_response(StatusCode::OK, r#"{"queued":3,"active":false}"#) }),
    );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    let human = command_output(command(&server, &token_file, ["queue", "status"])).await;
    let json = command_output(command(&server, &token_file, ["queue", "status", "--json"])).await;
    server.stop().await;

    assert_eq!(stdout(&human), "Queue\n  Queued: 3\n  Active: no\n");
    // `--json` remains the byte-for-byte machine contract.
    assert_eq!(stdout(&json), "{\"active\":false,\"queued\":3}\n");
}

#[tokio::test]
async fn jobs_list_renders_placeholder_and_table() {
    let empty = Router::new().route(
        "/v1/jobs",
        any(|| async { json_response(StatusCode::OK, r#"{"jobs":[]}"#) }),
    );
    let server = TestServer::start(empty).await;
    let token_file = SecretFile::new("cli-secret");
    let empty_output = command_output(command(&server, &token_file, ["jobs", "list"])).await;
    server.stop().await;
    assert_eq!(stdout(&empty_output), "No jobs.\n");

    let populated = Router::new().route(
        "/v1/jobs",
        any(|| async {
            json_response(
                StatusCode::OK,
                r#"{"jobs":[{"id":"018f3f86-7b4c-7b4f-9b6a-6d62f45bb111","provider":"rezka","result_ref":"item","state":"queued","notify_scope":"initiator"}]}"#,
            )
        }),
    );
    let server = TestServer::start(populated).await;
    let list = command_output(command(&server, &token_file, ["jobs", "list"])).await;
    server.stop().await;

    let rendered = stdout(&list);
    assert!(rendered.contains("STATE"));
    assert!(rendered.contains("018f3f86-7b4c-7b4f-9b6a-6d62f45bb111"));
    assert!(rendered.contains("queued"));
    assert!(rendered.contains("rezka"));
}

#[tokio::test]
async fn search_renders_human_table_by_default() {
    let router = Router::new().route(
        "/v1/searches",
        any(|| async {
            json_response(
                StatusCode::CREATED,
                r#"{"api_version":"v1","session_id":"session-1","source":"prowlarr","expires_at":"2026-07-13T12:00:00Z","continuation":"session-1:5","results":[{"source":"prowlarr","result_id":"result-1","title":"Movie","size_bytes":104857600,"seeders":7,"ranking":{"exact_title":true,"exact_season":true,"quality_preference":0,"language_preference":0,"seeders":7,"size_bytes":104857600,"codec_preference":0,"release_group_preference":0}}]}"#,
            )
        }),
    );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    let human = command_output(command(
        &server,
        &token_file,
        ["search", "prowlarr", "Movie", "--kind", "movie"],
    ))
    .await;
    server.stop().await;

    let rendered = stdout(&human);
    assert!(rendered.starts_with("Search session session-1 (prowlarr)"));
    assert!(rendered.contains("Movie"));
    assert!(rendered.contains("100.0 MiB"));
    assert!(rendered.contains("SEEDERS"));
    assert!(rendered.contains("More results: search prowlarr --continue session-1:5"));
}

#[tokio::test]
async fn release_query_renders_next_episode_and_source() {
    let router = Router::new().route(
        "/v1/releases/query",
        any(|| async {
            json_response(
                StatusCode::OK,
                r#"{"status":"matched","source":"tvmaze","fetched_at":"2026-07-13T12:00:00Z","show":{"source_id":10,"title":"Severance","original_title":null,"year":2022,"lifecycle":"ongoing"},"precision":"date_time","lifecycle":"ongoing","released_episodes":19,"expected_episodes":20,"next_episode":{"source_id":200,"season":3,"episode":1,"title":"Future","air_at":"2027-01-01T14:00:00Z","precision":"date_time"},"schedule":[]}"#,
            )
        }),
    );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    let output = command_output(command(
        &server,
        &token_file,
        ["release", "--title", "Severance", "--year", "2022"],
    ))
    .await;
    server.stop().await;

    let rendered = stdout(&output);
    assert!(rendered.contains("Severance"));
    assert!(rendered.contains("S03E01"));
    assert!(rendered.contains("tvmaze"));
    assert!(rendered.contains("schedule metadata, not Rezka availability"));
}
