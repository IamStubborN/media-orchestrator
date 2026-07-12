use std::{
    future::IntoFuture,
    path::PathBuf,
    process::Output,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
    response::Response,
    routing::any,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

static SECRET_FILE_ID: AtomicU64 = AtomicU64::new(0);
const CLI_TIMEOUT: Duration = Duration::from_secs(5);

struct SecretFile(PathBuf);

impl SecretFile {
    fn new(contents: &str) -> Self {
        let id = SECRET_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("media-http-cli-{}-{id}.secret", std::process::id()));
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

async fn command_output(command: assert_cmd::Command) -> Result<Output, String> {
    command_output_with_timeout(command, CLI_TIMEOUT).await
}

async fn command_output_with_timeout(
    mut command: assert_cmd::Command,
    timeout: Duration,
) -> Result<Output, String> {
    command.timeout(timeout);
    match tokio::task::spawn_blocking(move || command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("CLI subprocess could not start: {error}")),
        Err(error) => Err(format!("CLI subprocess task failed: {error}")),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn command_timeout_kills_and_reaps_the_child_process() {
    let mut command = assert_cmd::Command::new("sh");
    command.args(["-c", "echo $$; exec sleep 30"]);
    let started = Instant::now();

    let output = command_output_with_timeout(command, Duration::from_millis(100))
        .await
        .expect("timed out child must still produce a reaped output status");

    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_secs(2));
    let pid = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    let status = std::process::Command::new("sh")
        .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
        .status()
        .unwrap();
    assert!(!status.success(), "timed out child {pid} was not reaped");
}

fn json_response(status: StatusCode, body: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn jobs_create_uses_auth_generated_headers_and_stable_json() {
    let router = Router::new().route(
        "/v1/jobs",
        any(|request: Request| async move {
            let valid_method = request.method() == Method::POST;
            let bearer = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                == Some("Bearer cli-secret");
            let request_id = request
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok());
            let idempotency_key = request
                .headers()
                .get("idempotency-key")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok());
            let body = to_bytes(request.into_body(), 64 * 1024).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let valid_body = body
                == serde_json::json!({
                    "provider": "rezka",
                    "result_ref": "rezka:series:42",
                    "notify_scope": "initiator"
                });
            if valid_method && bearer && request_id && idempotency_key && valid_body {
                json_response(
                    StatusCode::CREATED,
                    r#"{ "state": "queued", "result_ref": "rezka:series:42", "provider": "rezka", "notify_scope": "initiator", "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111" }"#,
                )
            } else {
                json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#)
            }
        }),
    );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret\n");

    let output = command_output(command(
        &server,
        &token_file,
        [
            "jobs",
            "create",
            "--provider",
            "rezka",
            "--result-ref",
            "rezka:series:42",
            "--json",
        ],
    ))
    .await;
    server.stop().await;
    let output = output.expect("CLI subprocess must finish before the harness timeout");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"id\":\"018f3f86-7b4c-7b4f-9b6a-6d62f45bb111\",\"notify_scope\":\"initiator\",\"provider\":\"rezka\",\"result_ref\":\"rezka:series:42\",\"state\":\"queued\"}\n",
    );
    assert!(output.stderr.is_empty());
}

#[tokio::test]
async fn tracking_add_list_and_remove_use_strict_json_contracts() {
    let router = Router::new()
        .route(
            "/v1/tracking",
            any(|request: Request| async move {
                let method = request.method().clone();
                let headers = request.headers().clone();
                let body = to_bytes(request.into_body(), 64 * 1024).await.unwrap();
                if method == Method::POST {
                    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let valid = headers.contains_key("authorization")
                        && headers.contains_key("x-request-id")
                        && headers.contains_key("idempotency-key")
                        && value
                            == serde_json::json!({
                                "provider": "rezka",
                                "title": "Ongoing Show",
                                "translation": "Studio Dub",
                                "known_episodes": [{"season": 1, "episode": 4}],
                                "scope": "family",
                                "series_ongoing": true
                            });
                    if valid {
                        json_response(StatusCode::CREATED, r#"{"id":"018f3f86-7b4c-7b4f-9b6a-6d62f45bb111","provider":"rezka","title":"Ongoing Show","translation":"Studio Dub","known_episodes":[{"season":1,"episode":4}],"scope":"family","state":"active"}"#)
                    } else {
                        json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#)
                    }
                } else if method == Method::GET
                    && headers.contains_key("authorization")
                    && !headers.contains_key("idempotency-key")
                {
                    json_response(StatusCode::OK, r#"{"tracking":[]}"#)
                } else {
                    json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#)
                }
            }),
        )
        .route(
            "/v1/tracking/{tracking_id}",
            any(|request: Request| async move {
                let valid = request.method() == Method::DELETE
                    && request.headers().contains_key("authorization")
                    && request.headers().contains_key("x-request-id")
                    && request.headers().contains_key("idempotency-key");
                if valid {
                    json_response(StatusCode::OK, r#"{"id":"018f3f86-7b4c-7b4f-9b6a-6d62f45bb111","provider":"rezka","title":"Ongoing Show","translation":"Studio Dub","known_episodes":[{"season":1,"episode":4}],"scope":"family","state":"active"}"#)
                } else {
                    json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#)
                }
            }),
        );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    let add = command_output(command(
        &server,
        &token_file,
        [
            "tracking",
            "add",
            "--provider",
            "rezka",
            "--title",
            "Ongoing Show",
            "--translation",
            "Studio Dub",
            "--known-episode",
            "1:4",
            "--scope",
            "family",
            "--json",
        ],
    ))
    .await
    .unwrap();
    let list = command_output(command(
        &server,
        &token_file,
        ["tracking", "list", "--json"],
    ))
    .await
    .unwrap();
    let remove = command_output(command(
        &server,
        &token_file,
        [
            "tracking",
            "remove",
            "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
            "--json",
        ],
    ))
    .await
    .unwrap();
    server.stop().await;

    for output in [&add, &list, &remove] {
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        String::from_utf8(list.stdout).unwrap(),
        "{\"tracking\":[]}\n"
    );
    assert!(
        String::from_utf8(add.stdout)
            .unwrap()
            .contains("\"scope\":\"family\"")
    );
    assert!(
        String::from_utf8(remove.stdout)
            .unwrap()
            .contains("\"state\":\"active\"")
    );
}

#[tokio::test]
async fn jobs_get_and_queue_status_use_the_expected_paths() {
    let router = Router::new()
        .route(
            "/v1/jobs/{job_id}",
            any(|request: Request| async move {
                if request.method() == Method::GET
                    && request.headers().contains_key("authorization")
                    && request.headers().contains_key("x-request-id")
                    && !request.headers().contains_key("idempotency-key")
                {
                    json_response(StatusCode::OK, r#"{ "state": "queued", "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111", "provider": "rezka", "result_ref": "item", "notify_scope": "initiator" }"#)
                } else {
                    json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#)
                }
            }),
        )
        .route(
            "/v1/queue/status",
            any(|request: Request| async move {
                if request.method() == Method::GET
                    && request.headers().contains_key("authorization")
                    && request.headers().contains_key("x-request-id")
                    && !request.headers().contains_key("idempotency-key")
                {
                    json_response(StatusCode::OK, r#"{ "queued": 3, "active": false }"#)
                } else {
                    json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#)
                }
            }),
        );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    let get = command_output(command(
        &server,
        &token_file,
        [
            "jobs",
            "get",
            "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
            "--json",
        ],
    ))
    .await;
    let queue = command_output(command(&server, &token_file, ["queue", "status", "--json"])).await;
    server.stop().await;
    let get = get.expect("CLI subprocess must finish before the harness timeout");
    let queue = queue.expect("CLI subprocess must finish before the harness timeout");

    assert!(
        get.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&get.stderr)
    );
    assert_eq!(
        String::from_utf8(get.stdout).unwrap(),
        "{\"id\":\"018f3f86-7b4c-7b4f-9b6a-6d62f45bb111\",\"notify_scope\":\"initiator\",\"provider\":\"rezka\",\"result_ref\":\"item\",\"state\":\"queued\"}\n",
    );
    assert!(
        queue.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&queue.stderr)
    );
    assert_eq!(
        String::from_utf8(queue.stdout).unwrap(),
        "{\"active\":false,\"queued\":3}\n"
    );
}

#[tokio::test]
async fn search_continue_and_download_use_stable_json_and_exact_selection() {
    let router = Router::new()
        .route(
            "/v1/searches",
            any(|request: Request| async move {
                let valid = request.method() == Method::POST
                    && request.headers().contains_key("authorization")
                    && request.headers().contains_key("x-request-id")
                    && request.headers().contains_key("idempotency-key");
                let body = to_bytes(request.into_body(), 64 * 1024).await.unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if valid
                    && body["source"] == "prowlarr"
                    && body["query"] == "Movie"
                    && body["media_kind"] == "movie"
                    && body["scope"] == serde_json::json!({"platform":"cli","chat_id":"local"})
                {
                    json_response(StatusCode::CREATED, r#"{
                        "api_version":"v1","session_id":"session-1","source":"prowlarr",
                        "expires_at":"2026-07-13T12:00:00Z","continuation":"session-1:5",
                        "results":[{"source":"prowlarr","result_id":"result-1","title":"Movie",
                        "size_bytes":100,"seeders":2,"ranking":{"exact_title":true,"exact_season":true,
                        "quality_preference":0,"language_preference":0,"seeders":2,"size_bytes":100,
                        "codec_preference":0,"release_group_preference":0}}]
                    }"#)
                } else { json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#) }
            }),
        )
        .route(
            "/v1/searches/continue",
            any(|request: Request| async move {
                let body = to_bytes(request.into_body(), 64 * 1024).await.unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if body == serde_json::json!({
                    "continuation":"session-1:5",
                    "scope":{"platform":"cli","chat_id":"local"}
                }) {
                    json_response(StatusCode::OK, r#"{"api_version":"v1","session_id":"session-1","source":"prowlarr","expires_at":"2026-07-13T12:00:00Z","results":[]}"#)
                } else { json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#) }
            }),
        )
        .route(
            "/v1/selections",
            any(|request: Request| async move {
                let body = to_bytes(request.into_body(), 64 * 1024).await.unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if body == serde_json::json!({
                    "session_id":"session-1","result_id":"result-1",
                    "scope":{"platform":"cli","chat_id":"local"}
                }) {
                    json_response(StatusCode::CREATED, r#"{"id":"018f3f86-7b4c-7b4f-9b6a-6d62f45bb111","provider":"prowlarr","result_ref":"selection:one","state":"queued","notify_scope":"initiator"}"#)
                } else { json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#) }
            }),
        );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    let first = command_output(command(
        &server,
        &token_file,
        ["search", "prowlarr", "Movie", "--kind", "movie", "--json"],
    ))
    .await
    .unwrap();
    let next = command_output(command(
        &server,
        &token_file,
        ["search", "prowlarr", "--continue", "session-1:5", "--json"],
    ))
    .await
    .unwrap();
    let selected = command_output(command(
        &server,
        &token_file,
        [
            "download",
            "--session",
            "session-1",
            "--result",
            "result-1",
            "--json",
        ],
    ))
    .await
    .unwrap();
    server.stop().await;

    for output in [&first, &next, &selected] {
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!value.to_string().contains("magnet:"));
    }
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&first.stdout).unwrap()["continuation"],
        "session-1:5"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&selected.stdout).unwrap()["result_ref"],
        "selection:one"
    );
}

#[tokio::test]
async fn jobs_list_show_and_cancel_use_json_http_contracts() {
    let router = Router::new()
        .route(
            "/v1/jobs",
            any(|request: Request| async move {
                if request.method() == Method::GET
                    && request.headers().contains_key("authorization")
                    && request.headers().contains_key("x-request-id")
                    && !request.headers().contains_key("idempotency-key")
                {
                    json_response(StatusCode::OK, r#"{"jobs":[]}"#)
                } else {
                    json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#)
                }
            }),
        )
        .route(
            "/v1/jobs/{job_id}",
            any(|request: Request| async move {
                if request.method() == Method::GET {
                    json_response(StatusCode::OK, r#"{"id":"018f3f86-7b4c-7b4f-9b6a-6d62f45bb111","provider":"rezka","result_ref":"item","state":"queued","notify_scope":"initiator"}"#)
                } else {
                    json_response(StatusCode::METHOD_NOT_ALLOWED, r#"{}"#)
                }
            }),
        )
        .route(
            "/v1/jobs/{job_id}/cancel",
            any(|request: Request| async move {
                let valid = request.method() == Method::POST
                    && request.headers().contains_key("authorization")
                    && request.headers().contains_key("x-request-id")
                    && request.headers().contains_key("idempotency-key");
                if valid {
                    json_response(StatusCode::OK, r#"{"id":"018f3f86-7b4c-7b4f-9b6a-6d62f45bb111","provider":"rezka","result_ref":"item","state":"cancelled","notify_scope":"initiator"}"#)
                } else {
                    json_response(StatusCode::BAD_REQUEST, r#"{"code":"bad_test_request"}"#)
                }
            }),
        );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    let list = command_output(command(&server, &token_file, ["jobs", "list", "--json"]))
        .await
        .unwrap();
    let show = command_output(command(
        &server,
        &token_file,
        [
            "jobs",
            "show",
            "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
            "--json",
        ],
    ))
    .await
    .unwrap();
    let cancel = command_output(command(
        &server,
        &token_file,
        [
            "jobs",
            "cancel",
            "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
            "--json",
        ],
    ))
    .await
    .unwrap();
    server.stop().await;

    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(String::from_utf8(list.stdout).unwrap(), "{\"jobs\":[]}\n");
    assert!(
        show.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    assert!(
        cancel.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cancel.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&cancel.stdout).unwrap()["state"],
        "cancelled",
    );
}

#[tokio::test]
async fn client_preserves_service_url_prefix_with_or_without_a_trailing_slash() {
    let router = Router::new().route(
        "/prefix/v1/queue/status",
        any(|| async { json_response(StatusCode::OK, r#"{"queued":1,"active":false}"#) }),
    );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    for suffix in ["/prefix", "/prefix/"] {
        let mut prefixed = command(&server, &token_file, ["queue", "status", "--json"]);
        prefixed.env(
            "MEDIA_SERVICE_URL",
            format!("{}{suffix}", server.service_url),
        );
        let output = command_output(prefixed)
            .await
            .expect("CLI subprocess must finish before the harness timeout");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "{\"active\":false,\"queued\":1}\n"
        );
    }

    server.stop().await;
}

#[tokio::test]
async fn cli_preserves_http_error_status_and_body_on_stderr() {
    let router = Router::new().route(
        "/v1/queue/status",
        any(|| async {
            json_response(
                StatusCode::CONFLICT,
                r#"{"code":"conflict","message":"queue unavailable","request_id":"req-server"}"#,
            )
        }),
    );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("cli-secret");

    let output = command_output(command(&server, &token_file, ["queue", "status", "--json"])).await;
    server.stop().await;
    let output = output.expect("CLI subprocess must finish before the harness timeout");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "HTTP 409 Conflict: {\"code\":\"conflict\",\"message\":\"queue unavailable\",\"request_id\":\"req-server\"}\n",
    );
}

#[tokio::test]
async fn healthcheck_uses_the_unauthenticated_health_endpoint() {
    let router = Router::new().route(
        "/v1/health",
        any(|request: Request| async move {
            if request.method() == Method::GET
                && !request.headers().contains_key("authorization")
                && !request.headers().contains_key("x-request-id")
            {
                json_response(StatusCode::OK, r#"{"status":"ok"}"#)
            } else {
                json_response(StatusCode::BAD_REQUEST, r#"{"status":"invalid"}"#)
            }
        }),
    );
    let server = TestServer::start(router).await;
    let token_file = SecretFile::new("unused-token");
    let url = format!("{}/v1/health", server.service_url);

    let output = command_output(command(
        &server,
        &token_file,
        ["healthcheck", "--url", &url],
    ))
    .await;
    server.stop().await;
    let output = output.expect("CLI subprocess must finish before the harness timeout");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
