mod support;

use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Extension, State},
    http::{HeaderName, HeaderValue, Request, StatusCode, header},
    routing::post,
};
use futures_util::stream;
use media_api::{ApiState, IdempotencyRequest, RequestId, build_router, router};
use media_contract::{ApiError as ErrorBody, ApiErrorCode};
use media_core::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole};
use tower::ServiceExt;

use support::{
    FakeClientStore, FakeReadiness, RecordingIdempotencyStore, VALID_TOKEN, state,
    state_with_idempotency,
};

async fn echo_request_id(Extension(request_id): Extension<RequestId>) -> String {
    request_id.as_str().to_owned()
}

async fn body_length(body: Bytes) -> String {
    body.len().to_string()
}

async fn raw_recording_handler(
    State(state): State<ApiState>,
    Extension(handler_calls): Extension<Arc<AtomicUsize>>,
    request: Request<Body>,
) -> StatusCode {
    handler_calls.fetch_add(1, Ordering::SeqCst);
    state
        .idempotency()
        .reserve(IdempotencyRequest::new(
            PRIMARY_CLIENT_ID,
            "test-key".to_owned(),
            [1; 32],
        ))
        .await
        .unwrap();
    axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap();
    StatusCode::NO_CONTENT
}

fn recording_app(
    handler_calls: Arc<AtomicUsize>,
    idempotency: RecordingIdempotencyStore,
) -> Router {
    let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let protected = Router::new()
        .route("/test/raw", post(raw_recording_handler))
        .layer(Extension(handler_calls));
    build_router(
        state_with_idempotency(
            FakeClientStore::new([(VALID_TOKEN, actor)]),
            FakeReadiness::ready(),
            Arc::new(idempotency),
        ),
        protected,
    )
}

async fn assert_limit_error(
    response: axum::response::Response,
    expected_status: StatusCode,
    expected_message: &str,
) {
    assert_eq!(response.status(), expected_status);
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.code, ApiErrorCode::InvalidRequest);
    assert_eq!(body.message, expected_message);
    assert_eq!(body.request_id, request_id);
}

fn public_app(readiness: FakeReadiness) -> Router {
    router(state(FakeClientStore::new([]), readiness))
}

#[tokio::test]
async fn generated_request_id_is_added_to_the_response_header() {
    let response = public_app(FakeReadiness::ready())
        .oneshot(Request::get("/missing").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let header_id = response
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(!header_id.is_empty());
    assert!(uuid::Uuid::parse_str(&header_id).is_ok());
}

#[tokio::test]
async fn valid_request_id_is_propagated_to_extension_and_response_header() {
    let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let protected = Router::new().route("/test/request-id", post(echo_request_id));
    let app = build_router(
        state(
            FakeClientStore::new([(VALID_TOKEN, actor)]),
            FakeReadiness::ready(),
        ),
        protected,
    );
    let response = app
        .oneshot(
            Request::post("/test/request-id")
                .header("x-request-id", "request-123")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-request-id"], "request-123");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), b"request-123");
}

#[tokio::test]
async fn invalid_request_ids_are_replaced() {
    for invalid in ["", "contains space", &"a".repeat(129)] {
        let response = public_app(FakeReadiness::ready())
            .oneshot(
                Request::get("/v1/health")
                    .header("x-request-id", invalid)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let actual = response.headers()["x-request-id"].to_str().unwrap();
        assert_ne!(actual, invalid);
        assert!(uuid::Uuid::parse_str(actual).is_ok());
    }
}

#[tokio::test]
async fn repeated_request_id_headers_are_replaced() {
    let mut request = Request::get("/v1/health").body(Body::empty()).unwrap();
    request
        .headers_mut()
        .append("x-request-id", "first".parse().unwrap());
    request
        .headers_mut()
        .append("x-request-id", "second".parse().unwrap());

    let response = public_app(FakeReadiness::ready())
        .oneshot(request)
        .await
        .unwrap();

    let actual = response.headers()["x-request-id"].to_str().unwrap();
    assert_ne!(actual, "first");
    assert_ne!(actual, "second");
    assert!(uuid::Uuid::parse_str(actual).is_ok());
}

#[tokio::test]
async fn oversized_body_returns_stable_json_without_authentication() {
    let protected = Router::new().route("/test/body", post(body_length));
    let app = build_router(
        state(FakeClientStore::new([]), FakeReadiness::ready()),
        protected,
    );
    let response = app
        .oneshot(
            Request::post("/test/body")
                .header(header::CONTENT_LENGTH, 65_537)
                .body(Body::from(vec![b'x'; 65_537]))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.code, ApiErrorCode::InvalidRequest);
    assert_eq!(body.message, "request body is too large");
    assert_eq!(body.request_id, request_id);
}

#[tokio::test]
async fn default_body_limit_rejects_unannounced_oversized_body_after_authentication() {
    let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let protected = Router::new().route("/test/body", post(body_length));
    let app = build_router(
        state(
            FakeClientStore::new([(VALID_TOKEN, actor)]),
            FakeReadiness::ready(),
        ),
        protected,
    );

    let accepted = app
        .clone()
        .oneshot(
            Request::post("/test/body")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .body(Body::from(vec![b'x'; 65_536]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    let rejected = app
        .oneshot(
            Request::post("/test/body")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .body(Body::from(vec![b'x'; 65_537]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let request_id = rejected.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = axum::body::to_bytes(rejected.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.code, ApiErrorCode::InvalidRequest);
    assert_eq!(body.request_id, request_id);
}

#[tokio::test]
async fn raw_streaming_body_is_globally_limited_before_handler_or_idempotency() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let idempotency = RecordingIdempotencyStore::default();
    let app = recording_app(handler_calls.clone(), idempotency.clone());
    let chunks = [32_768, 32_768, 1].map(|size| Ok::<_, Infallible>(Bytes::from(vec![b'x'; size])));
    let request = Request::post("/test/raw")
        .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
        .header("x-request-id", "raw-overflow")
        .body(Body::from_stream(stream::iter(chunks)))
        .unwrap();
    assert!(request.headers().get(header::CONTENT_LENGTH).is_none());

    let response = app.oneshot(request).await.unwrap();

    assert_limit_error(
        response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "request body is too large",
    )
    .await;
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    assert_eq!(idempotency.calls(), 0);
}

#[tokio::test]
async fn raw_streaming_body_at_the_limit_reaches_handler_and_idempotency() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let idempotency = RecordingIdempotencyStore::default();
    let app = recording_app(handler_calls.clone(), idempotency.clone());
    let chunks = [32_768, 32_768].map(|size| Ok::<_, Infallible>(Bytes::from(vec![b'x'; size])));
    let request = Request::post("/test/raw")
        .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
        .body(Body::from_stream(stream::iter(chunks)))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(handler_calls.load(Ordering::SeqCst), 1);
    assert_eq!(idempotency.calls(), 1);
}

#[tokio::test]
async fn aggregate_header_count_is_limited_before_handler_or_idempotency() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let idempotency = RecordingIdempotencyStore::default();
    let app = recording_app(handler_calls.clone(), idempotency.clone());
    let mut request = Request::post("/test/raw")
        .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
        .header("x-request-id", "header-count-overflow")
        .body(Body::empty())
        .unwrap();
    for index in 0..65 {
        request.headers_mut().append(
            HeaderName::from_bytes(format!("x-extra-{index}").as_bytes()).unwrap(),
            HeaderValue::from_static("x"),
        );
    }

    let response = app.oneshot(request).await.unwrap();

    assert_limit_error(
        response,
        StatusCode::BAD_REQUEST,
        "request headers are too large",
    )
    .await;
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    assert_eq!(idempotency.calls(), 0);
}

#[tokio::test]
async fn aggregate_header_bytes_are_limited_before_handler_or_idempotency() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let idempotency = RecordingIdempotencyStore::default();
    let app = recording_app(handler_calls.clone(), idempotency.clone());
    let request = Request::post("/test/raw")
        .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
        .header("x-request-id", "header-bytes-overflow")
        .header("x-unrelated", "x".repeat(16_385))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_limit_error(
        response,
        StatusCode::BAD_REQUEST,
        "request headers are too large",
    )
    .await;
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    assert_eq!(idempotency.calls(), 0);
}

#[tokio::test]
async fn invalid_content_length_returns_stable_bad_request() {
    let response = public_app(FakeReadiness::ready())
        .oneshot(
            Request::get("/v1/health")
                .header(header::CONTENT_LENGTH, "not-a-number")
                .header("x-request-id", "content-length-test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()["x-request-id"], "content-length-test");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.code, ApiErrorCode::InvalidRequest);
    assert_eq!(body.message, "content-length header is invalid");
    assert_eq!(body.request_id, "content-length-test");
}

#[tokio::test]
async fn health_is_live_without_authentication() {
    let response = public_app(FakeReadiness::failing())
        .oneshot(Request::get("/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        serde_json::json!({ "status": "ok" }),
    );
}

#[tokio::test]
async fn readiness_requires_a_ready_dependency() {
    for readiness in [FakeReadiness::not_ready(), FakeReadiness::failing()] {
        let response = public_app(readiness)
            .oneshot(
                Request::get("/v1/ready")
                    .header("x-request-id", "readiness-failure")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["x-request-id"], "readiness-failure");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.code, ApiErrorCode::Internal);
        assert_eq!(body.message, "service is not ready");
        assert_eq!(body.request_id, "readiness-failure");
    }

    let response = public_app(FakeReadiness::ready())
        .oneshot(Request::get("/v1/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
