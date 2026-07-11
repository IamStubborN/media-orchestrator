mod support;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::router;
use media_contract::{ApiError, ApiErrorCode};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole, Job, JobId, JobState, NotifyScope,
    Provider, QueueStatus, RUNNER_CLIENT_ID, SECONDARY_USER_ID,
};
use tower::ServiceExt;

use support::{
    FakeClientStore, FakeJobStore, FakeLeaseStore, MemoryIdempotencyStore, RUNNER_TOKEN,
    VALID_TOKEN, state_with_stores,
};

fn primary() -> Actor {
    Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap()
}

fn runner_actor() -> Actor {
    Actor::new(RUNNER_CLIENT_ID, None, ClientRole::Runner).unwrap()
}

fn app(jobs: FakeJobStore) -> axum::Router {
    router(state_with_stores(
        FakeClientStore::new([(VALID_TOKEN, primary()), (RUNNER_TOKEN, runner_actor())]),
        Arc::new(jobs),
        Arc::new(FakeLeaseStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
    ))
}

async fn error(response: axum::response::Response) -> ApiError {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn create_body(extra: &str) -> String {
    format!(
        r#"{{"provider":"rezka","result_ref":"selection-1","notify_scope":"initiator"{extra}}}"#,
    )
}

#[tokio::test]
async fn owner_cannot_read_another_users_job() {
    let job_id = JobId::new();
    let job = Job::rehydrate(
        job_id,
        SECONDARY_USER_ID,
        Provider::Rezka,
        "private-secondary-selection".to_owned(),
        JobState::Queued,
        None,
        NotifyScope::Family,
    )
    .unwrap();

    let response = app(FakeJobStore::with_job(job))
        .oneshot(
            Request::get(format!("/v1/jobs/{job_id}"))
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("x-request-id", "owner-scope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.headers()["x-request-id"], "owner-scope");
    let body = error(response).await;
    assert_eq!(body.code, ApiErrorCode::NotFound);
    assert_eq!(body.request_id, "owner-scope");
}

#[tokio::test]
async fn runner_cannot_create_or_read_user_jobs() {
    let create = app(FakeJobStore::default())
        .clone()
        .oneshot(
            Request::post("/v1/jobs")
                .header(header::AUTHORIZATION, format!("Bearer {RUNNER_TOKEN}"))
                .header("idempotency-key", "runner-create")
                .header("content-type", "application/json")
                .body(Body::from(create_body("")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::FORBIDDEN);
    assert_eq!(error(create).await.code, ApiErrorCode::Forbidden);

    let read = app(FakeJobStore::default())
        .oneshot(
            Request::get(format!("/v1/jobs/{}", JobId::new()))
                .header(header::AUTHORIZATION, format!("Bearer {RUNNER_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::FORBIDDEN);
    assert_eq!(error(read).await.code, ApiErrorCode::Forbidden);
}

#[tokio::test]
async fn create_rejects_owner_spoofing_as_unknown_json() {
    let jobs = FakeJobStore::default();
    let response = app(jobs.clone())
        .oneshot(
            Request::post("/v1/jobs")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("idempotency-key", "owner-spoof")
                .header("content-type", "application/json")
                .header("x-request-id", "owner-spoof-request")
                .body(Body::from(create_body(
                    r#", "owner_id":"00000000-0000-0000-0000-000000000002""#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = error(response).await;
    assert_eq!(body.code, ApiErrorCode::InvalidRequest);
    assert_eq!(body.request_id, "owner-spoof-request");
    assert_eq!(jobs.create_calls(), 0);
}

#[tokio::test]
async fn queue_status_exposes_only_counts_and_activity() {
    let response = app(FakeJobStore::with_status(QueueStatus {
        queued: 2,
        active: true,
    }))
    .oneshot(
        Request::get("/v1/queue/status")
            .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value, serde_json::json!({"queued": 2, "active": true}));
    assert!(
        bytes
            .windows("owner".len())
            .all(|window| window != b"owner")
    );
    assert!(
        bytes
            .windows("result_ref".len())
            .all(|window| window != b"result_ref")
    );
}

#[tokio::test]
async fn list_returns_only_the_authenticated_owners_jobs() {
    let own = Job::rehydrate(
        JobId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "own-selection".to_owned(),
        JobState::Queued,
        None,
        NotifyScope::Initiator,
    )
    .unwrap();
    let other = Job::rehydrate(
        JobId::new(),
        SECONDARY_USER_ID,
        Provider::Prowlarr,
        "private-selection".to_owned(),
        JobState::Queued,
        None,
        NotifyScope::Family,
    )
    .unwrap();

    let response = app(FakeJobStore::with_jobs([own.clone(), other]))
        .oneshot(
            Request::get("/v1/jobs")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(value["jobs"][0]["id"], own.id().to_string());
}

#[tokio::test]
async fn owner_can_cancel_a_queued_job_immediately() {
    let job = Job::rehydrate(
        JobId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "cancel-selection".to_owned(),
        JobState::Queued,
        None,
        NotifyScope::Initiator,
    )
    .unwrap();
    let response = app(FakeJobStore::with_job(job.clone()))
        .oneshot(
            Request::post(format!("/v1/jobs/{}/cancel", job.id()))
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("idempotency-key", "cancel-job")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["state"], "cancelled");
}
