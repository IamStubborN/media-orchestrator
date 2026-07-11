mod support;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::router;
use media_contract::{ApiError, ApiErrorCode, LeaseDto};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole, Job, JobId, JobLease, JobState, LeaseId,
    LeaseStore, NotifyScope, OperationKey, Provider, RUNNER_CLIENT_ID,
};
use tower::ServiceExt;

use support::{
    CommitThenErrorIdempotencyStore, ControlledIdempotencyStore, ControlledReservation,
    FakeClientStore, FakeJobStore, FakeLeaseStore, FakeOperationCompletionStore,
    MemoryIdempotencyStore, RUNNER_TOKEN, VALID_TOKEN, state_with_stores,
    state_with_stores_and_completion,
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

fn request(key: &str, body: impl Into<Body>, request_id: &str) -> Request<Body> {
    Request::post("/v1/jobs")
        .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
        .header("idempotency-key", key)
        .header("content-type", "application/json")
        .header("x-request-id", request_id)
        .body(body.into())
        .unwrap()
}

fn app_with_idempotency(
    jobs: FakeJobStore,
    idempotency: Arc<dyn media_api::IdempotencyStore>,
) -> axum::Router {
    let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    router(state_with_stores(
        FakeClientStore::new([(VALID_TOKEN, actor)]),
        Arc::new(jobs),
        Arc::new(FakeLeaseStore::default()),
        idempotency,
    ))
}

fn app_with_reconciliation(
    jobs: FakeJobStore,
    idempotency: Arc<dyn media_api::IdempotencyStore>,
    operations: Arc<dyn media_api::OperationCompletionStore>,
) -> axum::Router {
    let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    router(state_with_stores_and_completion(
        FakeClientStore::new([(VALID_TOKEN, actor)]),
        Arc::new(jobs),
        Arc::new(FakeLeaseStore::default()),
        idempotency,
        operations,
    ))
}

fn runner_app_with_idempotency(
    leases: FakeLeaseStore,
    idempotency: Arc<dyn media_api::IdempotencyStore>,
) -> axum::Router {
    let runner = Actor::new(RUNNER_CLIENT_ID, None, ClientRole::Runner).unwrap();
    router(state_with_stores(
        FakeClientStore::new([(RUNNER_TOKEN, runner)]),
        Arc::new(FakeJobStore::default()),
        Arc::new(leases),
        idempotency,
    ))
}

fn runner_request(path: &str, key: &str, request_id: &str) -> Request<Body> {
    Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {RUNNER_TOKEN}"))
        .header("idempotency-key", key)
        .header("x-request-id", request_id)
        .body(Body::empty())
        .unwrap()
}

fn lease_at(lease_id: LeaseId, expires_at: i64) -> JobLease {
    let job = Job::rehydrate(
        JobId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "durable-result".to_owned(),
        JobState::Leased,
        None,
        NotifyScope::Initiator,
    )
    .unwrap();
    JobLease::new(
        lease_id,
        job,
        RUNNER_CLIENT_ID,
        time::OffsetDateTime::from_unix_timestamp(expires_at).unwrap(),
    )
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
async fn completed_operation_reenters_an_in_progress_reservation_and_repairs_http_replay() {
    let jobs = FakeJobStore::default();
    let idempotency = ControlledIdempotencyStore::new([ControlledReservation::InProgress]);
    let operations = FakeOperationCompletionStore::completed();
    let response = app_with_reconciliation(
        jobs.clone(),
        Arc::new(idempotency.clone()),
        Arc::new(operations.clone()),
    )
    .oneshot(request(
        "completed-operation",
        BODY,
        "completed-operation-retry",
    ))
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(operations.calls(), 1);
    assert_eq!(idempotency.complete_calls(), 1);
    assert_eq!(idempotency.abort_calls(), 0);
    assert_eq!(jobs.create_calls(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fake_lease_store_records_each_concurrent_operation_once() {
    let lease = lease_at(LeaseId::new(), 1_783_707_660);
    let store = FakeLeaseStore::with_lease(lease.clone());

    for marker in 1..=32_u8 {
        let key = OperationKey::from_bytes([marker; 32]);
        let barrier = Arc::new(tokio::sync::Barrier::new(16));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let store = store.clone();
            let barrier = barrier.clone();
            tasks.spawn(async move {
                barrier.wait().await;
                store
                    .lease_next(key, RUNNER_CLIENT_ID, time::Duration::seconds(60))
                    .await
                    .unwrap()
            });
        }
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap(), Some(lease.clone()));
        }
        assert_eq!(store.lease_calls(), usize::from(marker));
    }
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

#[tokio::test]
async fn maximum_sized_create_response_is_persisted_and_replayed_before_mutating_twice() {
    let prefix = r#"{"provider":"rezka","result_ref":""#;
    let suffix = r#"","notify_scope":"initiator"}"#;
    let body = format!(
        "{prefix}{}{suffix}",
        "x".repeat(64 * 1024 - prefix.len() - suffix.len()),
    );
    assert_eq!(body.len(), 64 * 1024);
    let jobs = FakeJobStore::default();
    let app = app(jobs.clone());

    let first = app
        .clone()
        .oneshot(request("max-request", body.clone(), "max-first"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_body = body_bytes(first).await;
    assert!(first_body.len() > 64 * 1024);

    let replay = app
        .oneshot(request("max-request", body, "max-replay"))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(body_bytes(replay).await, first_body);
    assert_eq!(jobs.create_calls(), 1);
}

#[tokio::test]
async fn malformed_completed_replay_fails_closed_on_every_retry_without_execution() {
    let jobs = FakeJobStore::default();
    let malformed = media_api::StoredHttpResponse::new(
        200,
        "invalid\ncontent-type".to_owned(),
        b"malformed".to_vec(),
    );
    let idempotency = ControlledIdempotencyStore::new([
        ControlledReservation::Replay(malformed.clone()),
        ControlledReservation::Replay(malformed),
    ]);
    let app = app_with_idempotency(jobs.clone(), Arc::new(idempotency.clone()));

    let malformed = app
        .clone()
        .oneshot(request("malformed", BODY, "malformed-first"))
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let first_error = error(malformed).await;
    assert_eq!(first_error.code, ApiErrorCode::Internal);
    assert_eq!(first_error.message, "internal server error");
    assert_eq!(first_error.request_id, "malformed-first");
    assert_eq!(idempotency.abort_calls(), 0);
    assert_eq!(jobs.create_calls(), 0);

    let retry = app
        .oneshot(request("malformed", BODY, "malformed-retry"))
        .await
        .unwrap();
    assert_eq!(retry.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let retry_error = error(retry).await;
    assert_eq!(retry_error.code, first_error.code);
    assert_eq!(retry_error.message, first_error.message);
    assert_eq!(retry_error.request_id, "malformed-retry");
    assert_eq!(idempotency.abort_calls(), 0);
    assert_eq!(jobs.create_calls(), 0);
}

#[tokio::test]
async fn conflict_never_discards_a_different_fingerprint() {
    let idempotency = ControlledIdempotencyStore::new([ControlledReservation::Conflict]);
    let response = app_with_idempotency(FakeJobStore::default(), Arc::new(idempotency.clone()))
        .oneshot(request("conflict", BODY, "conflict-request"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(idempotency.abort_calls(), 0);
}

#[tokio::test]
async fn complete_and_abort_storage_failures_return_sanitized_500() {
    let complete_failure =
        ControlledIdempotencyStore::new([ControlledReservation::Reserved]).failing_complete();
    let response =
        app_with_idempotency(FakeJobStore::default(), Arc::new(complete_failure.clone()))
            .oneshot(request("complete-failure", BODY, "complete-failure"))
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error(response).await.request_id, "complete-failure");
    assert_eq!(complete_failure.complete_calls(), 1);
    assert_eq!(complete_failure.abort_calls(), 1);

    let jobs = FakeJobStore::default();
    jobs.fail_creates(1);
    let abort_failure =
        ControlledIdempotencyStore::new([ControlledReservation::Reserved]).failing_abort();
    let response = app_with_idempotency(jobs, Arc::new(abort_failure.clone()))
        .oneshot(request("abort-failure", BODY, "abort-failure"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error(response).await.request_id, "abort-failure");
    assert_eq!(abort_failure.abort_calls(), 1);
}

#[tokio::test]
async fn committed_complete_error_cannot_be_aborted_and_retries_as_replay() {
    let jobs = FakeJobStore::default();
    let idempotency = CommitThenErrorIdempotencyStore::default();
    let app = app_with_idempotency(jobs.clone(), Arc::new(idempotency.clone()));

    let uncertain = app
        .clone()
        .oneshot(request("commit-uncertain", BODY, "commit-uncertain"))
        .await
        .unwrap();
    assert_eq!(uncertain.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(idempotency.abort_calls(), 1);
    assert_eq!(jobs.create_calls(), 1);

    let replay = app
        .oneshot(request("commit-uncertain", BODY, "commit-retry"))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(jobs.create_calls(), 1);
}

#[tokio::test]
async fn create_reentry_after_replay_completion_failure_mutates_once() {
    let jobs = FakeJobStore::default();
    let failing =
        ControlledIdempotencyStore::new([ControlledReservation::Reserved]).failing_complete();
    let first = app_with_idempotency(jobs.clone(), Arc::new(failing))
        .oneshot(request("durable-create", BODY, "durable-create-first"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let retried = app_with_idempotency(jobs.clone(), Arc::new(MemoryIdempotencyStore::default()))
        .oneshot(request("durable-create", BODY, "durable-create-retry"))
        .await
        .unwrap();

    assert_eq!(retried.status(), StatusCode::CREATED);
    assert_eq!(jobs.create_calls(), 1);
}

#[tokio::test]
async fn lease_reentry_after_replay_completion_failure_returns_the_original_result() {
    let original = lease_at(LeaseId::new(), 1_783_707_660);
    let leases = FakeLeaseStore::with_lease(original.clone());
    let failing =
        ControlledIdempotencyStore::new([ControlledReservation::Reserved]).failing_complete();
    let first = runner_app_with_idempotency(leases.clone(), Arc::new(failing))
        .oneshot(runner_request(
            "/v1/runner/leases",
            "durable-lease",
            "durable-lease-first",
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::INTERNAL_SERVER_ERROR);
    leases.set_lease(Some(lease_at(LeaseId::new(), 1_783_708_000)));

    let retried =
        runner_app_with_idempotency(leases.clone(), Arc::new(MemoryIdempotencyStore::default()))
            .oneshot(runner_request(
                "/v1/runner/leases",
                "durable-lease",
                "durable-lease-retry",
            ))
            .await
            .unwrap();

    assert_eq!(retried.status(), StatusCode::OK);
    let dto: LeaseDto = serde_json::from_slice(&body_bytes(retried).await).unwrap();
    assert_eq!(dto.lease_id.to_string(), original.lease_id().to_string());
    assert_eq!(dto.expires_at, "2026-07-10T18:21:00Z");
    assert_eq!(leases.lease_calls(), 1);
}

#[tokio::test]
async fn empty_lease_reentry_after_replay_completion_failure_stays_no_content() {
    let leases = FakeLeaseStore::default();
    let failing =
        ControlledIdempotencyStore::new([ControlledReservation::Reserved]).failing_complete();
    let first = runner_app_with_idempotency(leases.clone(), Arc::new(failing))
        .oneshot(runner_request(
            "/v1/runner/leases",
            "durable-empty-lease",
            "durable-empty-first",
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::INTERNAL_SERVER_ERROR);
    leases.set_lease(Some(lease_at(LeaseId::new(), 1_783_708_000)));

    let retried =
        runner_app_with_idempotency(leases.clone(), Arc::new(MemoryIdempotencyStore::default()))
            .oneshot(runner_request(
                "/v1/runner/leases",
                "durable-empty-lease",
                "durable-empty-retry",
            ))
            .await
            .unwrap();

    assert_eq!(retried.status(), StatusCode::NO_CONTENT);
    assert_eq!(leases.lease_calls(), 1);
}

#[tokio::test]
async fn heartbeat_reentry_after_replay_completion_failure_keeps_the_original_expiry() {
    let lease_id = LeaseId::new();
    let original = lease_at(lease_id, 1_783_707_660);
    let leases = FakeLeaseStore::with_lease(original);
    let path = format!("/v1/runner/leases/{lease_id}/heartbeat");
    let failing =
        ControlledIdempotencyStore::new([ControlledReservation::Reserved]).failing_complete();
    let first = runner_app_with_idempotency(leases.clone(), Arc::new(failing))
        .oneshot(runner_request(
            &path,
            "durable-heartbeat",
            "durable-heartbeat-first",
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::INTERNAL_SERVER_ERROR);
    leases.set_lease(Some(lease_at(lease_id, 1_783_708_000)));

    let retried =
        runner_app_with_idempotency(leases.clone(), Arc::new(MemoryIdempotencyStore::default()))
            .oneshot(runner_request(
                &path,
                "durable-heartbeat",
                "durable-heartbeat-retry",
            ))
            .await
            .unwrap();

    assert_eq!(retried.status(), StatusCode::OK);
    let dto: LeaseDto = serde_json::from_slice(&body_bytes(retried).await).unwrap();
    assert_eq!(dto.lease_id.to_string(), lease_id.to_string());
    assert_eq!(dto.expires_at, "2026-07-10T18:21:00Z");
    assert_eq!(leases.heartbeat_calls(), 1);
}

#[tokio::test]
async fn key_boundaries_and_duplicate_headers_apply_to_every_post_family() {
    let user = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let runner = Actor::new(RUNNER_CLIENT_ID, None, ClientRole::Runner).unwrap();
    let app = router(state_with_stores(
        FakeClientStore::new([(VALID_TOKEN, user), (RUNNER_TOKEN, runner)]),
        Arc::new(FakeJobStore::default()),
        Arc::new(FakeLeaseStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
    ));
    let heartbeat_path = format!("/v1/runner/leases/{}/heartbeat", LeaseId::new());
    let families = [
        ("/v1/jobs".to_owned(), VALID_TOKEN, BODY),
        ("/v1/runner/leases".to_owned(), RUNNER_TOKEN, ""),
        (heartbeat_path, RUNNER_TOKEN, ""),
    ];

    for (family_index, (path, token, body)) in families.iter().enumerate() {
        let missing = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(*body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            error(missing).await.code,
            ApiErrorCode::MissingIdempotencyKey
        );

        let marker = char::from(b'a' + u8::try_from(family_index).unwrap());
        for (boundary_index, key) in [marker.to_string(), marker.to_string().repeat(128)]
            .into_iter()
            .enumerate()
        {
            let response = app
                .clone()
                .oneshot(
                    Request::post(path)
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .header("idempotency-key", key)
                        .body(Body::from(*body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "family {family_index} rejected key boundary {boundary_index}",
            );
        }

        let mut duplicate = Request::post(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("idempotency-key", format!("duplicate-{family_index}"))
            .body(Body::from(*body))
            .unwrap();
        duplicate
            .headers_mut()
            .append("idempotency-key", "second".parse().unwrap());
        let response = app.clone().oneshot(duplicate).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error(response).await.code, ApiErrorCode::InvalidRequest);
    }
}
