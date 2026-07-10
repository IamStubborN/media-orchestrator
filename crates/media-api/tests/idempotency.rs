mod support;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::router;
use media_contract::{ApiError, ApiErrorCode};
use media_core::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole};
use tower::ServiceExt;

use support::{
    FakeClientStore, FakeJobStore, FakeLeaseStore, MemoryIdempotencyStore, VALID_TOKEN,
    state_with_stores,
};

const BODY: &str = r#"{"provider":"rezka","result_ref":"selection-1","notify_scope":"initiator"}"#;

fn app(jobs: FakeJobStore) -> axum::Router {
    let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    router(state_with_stores(
        FakeClientStore::new([(VALID_TOKEN, actor)]),
        Arc::new(jobs),
        Arc::new(FakeLeaseStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
    ))
}

fn request(key: &str, body: &'static str, request_id: &str) -> Request<Body> {
    Request::post("/v1/jobs")
        .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
        .header("idempotency-key", key)
        .header("content-type", "application/json")
        .header("x-request-id", request_id)
        .body(Body::from(body))
        .unwrap()
}

async fn body_bytes(response: axum::response::Response) -> axum::body::Bytes {
    to_bytes(response.into_body(), usize::MAX).await.unwrap()
}

async fn error(response: axum::response::Response) -> ApiError {
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

#[tokio::test]
async fn duplicate_same_body_replays_exact_response_and_creates_once() {
    let jobs = FakeJobStore::default();
    let app = app(jobs.clone());
    let first = app
        .clone()
        .oneshot(request("same-request", BODY, "first-request"))
        .await
        .unwrap();
    let first_status = first.status();
    let first_content_type = first.headers()[header::CONTENT_TYPE].clone();
    let first_body = body_bytes(first).await;

    let replay = app
        .oneshot(request("same-request", BODY, "second-request"))
        .await
        .unwrap();
    assert_eq!(replay.status(), first_status);
    assert_eq!(replay.headers()[header::CONTENT_TYPE], first_content_type);
    assert_eq!(body_bytes(replay).await, first_body);
    assert_eq!(jobs.create_calls(), 1);
}

#[tokio::test]
async fn changed_body_reuse_conflicts() {
    let app = app(FakeJobStore::default());
    let first = app
        .clone()
        .oneshot(request("changed-body", BODY, "changed-first"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);

    let changed = r#"{"provider":"rezka","result_ref":"selection-2","notify_scope":"initiator"}"#;
    let response = app
        .oneshot(request("changed-body", changed, "changed-second"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = error(response).await;
    assert_eq!(body.code, ApiErrorCode::IdempotencyConflict);
    assert_eq!(body.request_id, "changed-second");
}

#[tokio::test]
async fn concurrent_duplicate_returns_stable_in_progress_error() {
    let jobs = FakeJobStore::default();
    jobs.block_creates();
    let app = app(jobs.clone());
    let first_app = app.clone();
    let first = tokio::spawn(async move {
        first_app
            .oneshot(request("concurrent", BODY, "concurrent-first"))
            .await
            .unwrap()
    });
    for _ in 0..10_000 {
        if jobs.create_calls() != 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        jobs.create_calls(),
        1,
        "first request did not reach job storage"
    );

    let response = app
        .oneshot(request("concurrent", BODY, "concurrent-second"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = error(response).await;
    assert_eq!(body.code, ApiErrorCode::IdempotencyInProgress);
    assert_eq!(body.request_id, "concurrent-second");

    jobs.unblock_creates();
    assert_eq!(first.await.unwrap().status(), StatusCode::CREATED);
    assert_eq!(jobs.create_calls(), 1);
}

#[tokio::test]
async fn server_error_aborts_reservation_so_same_request_can_retry() {
    let jobs = FakeJobStore::default();
    jobs.fail_creates(1);
    let app = app(jobs.clone());
    let failed = app
        .clone()
        .oneshot(request("retry-500", BODY, "failed-request"))
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let failed_body = error(failed).await;
    assert_eq!(failed_body.code, ApiErrorCode::Internal);
    assert_eq!(failed_body.request_id, "failed-request");

    let retried = app
        .oneshot(request("retry-500", BODY, "retried-request"))
        .await
        .unwrap();
    assert_eq!(retried.status(), StatusCode::CREATED);
    assert_eq!(jobs.create_calls(), 2);
}

#[tokio::test]
async fn every_post_requires_a_valid_visible_ascii_idempotency_key() {
    let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let app = router(state_with_stores(
        FakeClientStore::new([(VALID_TOKEN, actor)]),
        Arc::new(FakeJobStore::default()),
        Arc::new(FakeLeaseStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
    ));
    let missing = app
        .clone()
        .oneshot(
            Request::post("/v1/jobs")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("x-request-id", "missing-key")
                .body(Body::from(BODY))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        error(missing).await.code,
        ApiErrorCode::MissingIdempotencyKey
    );

    for (index, invalid) in ["", "has space", &"a".repeat(129)].into_iter().enumerate() {
        let response = app
            .clone()
            .oneshot(request(invalid, BODY, &format!("invalid-key-{index}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error(response).await.code, ApiErrorCode::InvalidRequest);
    }
}

#[tokio::test]
async fn deterministic_client_error_is_persisted_and_replayed_exactly() {
    let jobs = FakeJobStore::default();
    let app = app(jobs.clone());
    let invalid = r#"{"provider":"rezka","result_ref":"selection","notify_scope":"initiator","owner_id":"spoof"}"#;
    let first = app
        .clone()
        .oneshot(request("stored-400", invalid, "stored-error"))
        .await
        .unwrap();
    let first_status = first.status();
    let first_content_type = first.headers()[header::CONTENT_TYPE].clone();
    let first_body = body_bytes(first).await;

    let replay = app
        .oneshot(request("stored-400", invalid, "different-request-id"))
        .await
        .unwrap();
    assert_eq!(replay.status(), first_status);
    assert_eq!(replay.headers()[header::CONTENT_TYPE], first_content_type);
    assert_eq!(replay.headers()["x-request-id"], "stored-error");
    assert_eq!(body_bytes(replay).await, first_body);
    assert_eq!(jobs.create_calls(), 0);
}
