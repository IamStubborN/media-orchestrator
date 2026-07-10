mod support;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::Extension,
    http::{Request, StatusCode, header},
    routing::post,
};
use media_api::{RequestId, build_router, router};
use media_contract::{ApiError as ErrorBody, ApiErrorCode};
use media_core::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole};
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, VALID_TOKEN, state};

async fn echo_request_id(Extension(request_id): Extension<RequestId>) -> String {
    request_id.as_str().to_owned()
}

async fn body_length(body: Bytes) -> String {
    body.len().to_string()
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
            .oneshot(Request::get("/v1/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.code, ApiErrorCode::Internal);
        assert_eq!(body.message, "service is not ready");
    }

    let response = public_app(FakeReadiness::ready())
        .oneshot(Request::get("/v1/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
