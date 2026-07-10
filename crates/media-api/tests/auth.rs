mod support;

use axum::{
    Json, Router,
    extract::Extension,
    http::{Request, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use media_api::{ApiError, build_router};
use media_contract::{ApiError as ErrorBody, ApiErrorCode};
use media_core::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole, RUNNER_CLIENT_ID};
use tower::ServiceExt;

use support::{DISABLED_TOKEN, FakeClientStore, FakeReadiness, RUNNER_TOKEN, VALID_TOKEN, state};

async fn actor_identity(Extension(actor): Extension<Actor>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "client_id": actor.client_id().to_string(),
        "user_id": actor.user_id().map(|id| id.to_string()),
        "role": match actor.role() {
            ClientRole::Hermes => "hermes",
            ClientRole::Runner => "runner",
        },
    }))
}

async fn users_only(
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<media_api::RequestId>,
) -> Result<StatusCode, ApiError> {
    actor
        .require_user()
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|_| ApiError::forbidden(&request_id, "this operation requires a user client"))
}

fn app() -> Router {
    let hermes = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let runner = Actor::new(RUNNER_CLIENT_ID, None, ClientRole::Runner).unwrap();
    let clients = FakeClientStore::new([(VALID_TOKEN, hermes), (RUNNER_TOKEN, runner)]);
    let protected = Router::new()
        .route("/test/actor", get(actor_identity))
        .route("/test/users-only", get(users_only));

    build_router(state(clients, FakeReadiness::ready()), protected)
}

async fn error_body(response: axum::response::Response) -> ErrorBody {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn missing_bearer_token_returns_sanitized_unauthorized_error() {
    let response = app()
        .oneshot(
            Request::get("/test/actor")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let header_request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body = error_body(response).await;
    assert_eq!(body.code, ApiErrorCode::InvalidToken);
    assert_eq!(body.message, "authentication failed");
    assert_eq!(body.request_id, header_request_id);
}

#[tokio::test]
async fn invalid_and_disabled_tokens_are_indistinguishable() {
    for token in ["not-valid", DISABLED_TOKEN] {
        let response = app()
            .oneshot(
                Request::get("/test/actor")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = error_body(response).await;
        assert_eq!(body.code, ApiErrorCode::InvalidToken);
        assert_eq!(body.message, "authentication failed");
    }
}

#[tokio::test]
async fn oversized_bearer_token_is_rejected_before_hashing() {
    let response = app()
        .oneshot(
            Request::get("/test/actor")
                .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(513)))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = error_body(response).await;
    assert_eq!(body.code, ApiErrorCode::InvalidRequest);
    assert_eq!(body.message, "authorization header is invalid");
}

#[tokio::test]
async fn storage_failure_returns_sanitized_internal_error() {
    let protected = Router::new().route("/test/actor", get(actor_identity));
    let app = build_router(
        state(FakeClientStore::failing(), FakeReadiness::ready()),
        protected,
    );
    let response = app
        .oneshot(
            Request::get("/test/actor")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = error_body(response).await;
    assert_eq!(body.code, ApiErrorCode::Internal);
    assert_eq!(body.message, "internal server error");
}

#[tokio::test]
async fn hermes_identity_is_inserted_into_request_extensions() {
    let response = app()
        .oneshot(
            Request::get("/test/actor")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["client_id"], PRIMARY_CLIENT_ID.to_string());
    assert_eq!(body["user_id"], PRIMARY_USER_ID.to_string());
    assert_eq!(body["role"], "hermes");
}

#[tokio::test]
async fn runner_identity_is_inserted_and_user_route_forbids_it() {
    let actor_response = app()
        .clone()
        .oneshot(
            Request::get("/test/actor")
                .header(header::AUTHORIZATION, format!("Bearer {RUNNER_TOKEN}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(actor_response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(actor_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let actor: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(actor["client_id"], RUNNER_CLIENT_ID.to_string());
    assert_eq!(actor["user_id"], serde_json::Value::Null);
    assert_eq!(actor["role"], "runner");

    let forbidden = app()
        .oneshot(
            Request::get("/test/users-only")
                .header(header::AUTHORIZATION, format!("Bearer {RUNNER_TOKEN}"))
                .header("x-request-id", "runner-forbidden")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(forbidden.headers()["x-request-id"], "runner-forbidden");
    let body = error_body(forbidden).await;
    assert_eq!(body.code, ApiErrorCode::Forbidden);
    assert_eq!(body.request_id, "runner-forbidden");
}

#[test]
fn successful_handler_response_is_an_http_response() {
    let response = Json(serde_json::json!({ "ok": true })).into_response();
    assert_eq!(response.status(), StatusCode::OK);
}
