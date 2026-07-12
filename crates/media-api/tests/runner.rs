mod support;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::{PlexReconcileService, PlexServiceError, router};
use media_contract::{
    ApiError, ApiErrorCode, LeaseDto, PlexObservationDto, PlexReconcileRequest,
    PlexReconcileResponse, PlexReconcileStatus,
};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientId, ClientRole, Job, JobId, JobLease, JobState,
    LeaseId, NotifyScope, Provider, RUNNER_CLIENT_ID,
};
use tower::ServiceExt;

use support::{
    FakeClientStore, FakeJobStore, FakeLeaseStore, MemoryIdempotencyStore, OTHER_RUNNER_TOKEN,
    RUNNER_TOKEN, VALID_TOKEN, state_with_stores,
};

fn runner() -> Actor {
    Actor::new(RUNNER_CLIENT_ID, None, ClientRole::Runner).unwrap()
}

fn app(leases: FakeLeaseStore, other_runner: ClientId) -> axum::Router {
    let user = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let other = Actor::new(other_runner, None, ClientRole::Runner).unwrap();
    router(state_with_stores(
        FakeClientStore::new([
            (VALID_TOKEN, user),
            (RUNNER_TOKEN, runner()),
            (OTHER_RUNNER_TOKEN, other),
        ]),
        Arc::new(FakeJobStore::default()),
        Arc::new(leases),
        Arc::new(MemoryIdempotencyStore::default()),
    ))
}

struct MatchedPlex;

#[async_trait::async_trait]
impl PlexReconcileService for MatchedPlex {
    async fn reconcile(
        &self,
        request: PlexReconcileRequest,
    ) -> Result<PlexReconcileResponse, PlexServiceError> {
        Ok(PlexReconcileResponse {
            status: PlexReconcileStatus::Matched,
            observation: Some(PlexObservationDto {
                path: request.path,
                canonical_id: request.canonical_id,
                season: request.season,
                episode: request.episode,
            }),
        })
    }
}

fn app_with_plex() -> axum::Router {
    let user = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    router(
        state_with_stores(
            FakeClientStore::new([(VALID_TOKEN, user), (RUNNER_TOKEN, runner())]),
            Arc::new(FakeJobStore::default()),
            Arc::new(FakeLeaseStore::default()),
            Arc::new(MemoryIdempotencyStore::default()),
        )
        .with_plex(Arc::new(MatchedPlex)),
    )
}

fn post(path: &str, token: &str, key: &str, request_id: &str, body: Body) -> Request<Body> {
    Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("idempotency-key", key)
        .header("x-request-id", request_id)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn error(response: axum::response::Response) -> ApiError {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn lease() -> JobLease {
    let job = Job::rehydrate(
        JobId::new(),
        PRIMARY_USER_ID,
        Provider::Prowlarr,
        "prowlarr:result:7".to_owned(),
        JobState::Leased,
        None,
        NotifyScope::Family,
    )
    .unwrap();
    JobLease::new(
        LeaseId::new(),
        job,
        RUNNER_CLIENT_ID,
        time::OffsetDateTime::from_unix_timestamp(1_783_707_660).unwrap(),
    )
}

#[tokio::test]
async fn runner_can_lease_and_heartbeat_with_exact_request_ids() {
    let expected = lease();
    let app = app(
        FakeLeaseStore::with_lease(expected.clone()),
        ClientId::new(),
    );
    let leased = app
        .clone()
        .oneshot(post(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "lease-next",
            "lease-request",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(leased.status(), StatusCode::OK);
    assert_eq!(leased.headers()["x-request-id"], "lease-request");
    let bytes = to_bytes(leased.into_body(), usize::MAX).await.unwrap();
    let dto: LeaseDto = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(dto.lease_id.to_string(), expected.lease_id().to_string());
    assert_eq!(dto.job.id.to_string(), expected.job().id().to_string());
    assert_eq!(dto.job.result_ref, "prowlarr:result:7");
    assert_eq!(dto.expires_at, "2026-07-10T18:21:00Z");

    let heartbeat = app
        .oneshot(post(
            &format!("/v1/runner/leases/{}/heartbeat", expected.lease_id()),
            RUNNER_TOKEN,
            "heartbeat",
            "heartbeat-request",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(heartbeat.status(), StatusCode::OK);
    assert_eq!(heartbeat.headers()["x-request-id"], "heartbeat-request");
    let bytes = to_bytes(heartbeat.into_body(), usize::MAX).await.unwrap();
    let dto: LeaseDto = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(dto.lease_id.to_string(), expected.lease_id().to_string());
}

#[tokio::test]
async fn empty_queue_returns_no_content() {
    let leases = FakeLeaseStore::default();
    let app = app(leases.clone(), ClientId::new());
    let first = app
        .clone()
        .oneshot(post(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "empty-lease",
            "empty-lease-request",
            Body::empty(),
        ))
        .await
        .unwrap();

    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    assert!(first.headers().get(header::CONTENT_TYPE).is_none());
    assert!(
        to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );

    let replay = app
        .oneshot(post(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "empty-lease",
            "empty-lease-replay",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::NO_CONTENT);
    assert!(replay.headers().get(header::CONTENT_TYPE).is_none());
    assert_eq!(leases.lease_calls(), 1);
}

#[tokio::test]
async fn wrong_runner_heartbeat_looks_not_found() {
    let expected = lease();
    let other_runner = ClientId::new();
    let response = app(FakeLeaseStore::with_lease(expected.clone()), other_runner)
        .oneshot(post(
            &format!("/v1/runner/leases/{}/heartbeat", expected.lease_id()),
            OTHER_RUNNER_TOKEN,
            "wrong-runner",
            "wrong-runner-request",
            Body::empty(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.headers()["x-request-id"], "wrong-runner-request");
    let body = error(response).await;
    assert_eq!(body.code, ApiErrorCode::LeaseNotFound);
    assert_eq!(body.request_id, "wrong-runner-request");
}

#[tokio::test]
async fn user_cannot_use_runner_routes_and_ttl_override_is_unknown_json() {
    let other_runner = ClientId::new();
    let user_response = app(FakeLeaseStore::default(), other_runner)
        .clone()
        .oneshot(post(
            "/v1/runner/leases",
            VALID_TOKEN,
            "user-lease",
            "user-lease-request",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(user_response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error(user_response).await.code, ApiErrorCode::Forbidden);

    let ttl_response = app(FakeLeaseStore::default(), other_runner)
        .oneshot(post(
            "/v1/runner/leases",
            RUNNER_TOKEN,
            "ttl-override",
            "ttl-override-request",
            Body::from(r#"{"ttl":300}"#),
        ))
        .await
        .unwrap();
    assert_eq!(ttl_response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error(ttl_response).await.code, ApiErrorCode::InvalidRequest);
}

#[tokio::test]
async fn heartbeat_rejects_ttl_override_and_missing_lease_is_distinct_not_found() {
    let expected = lease();
    let other_runner = ClientId::new();
    let app = app(FakeLeaseStore::with_lease(expected), other_runner);
    let missing_id = LeaseId::new();
    let missing = app
        .clone()
        .oneshot(post(
            &format!("/v1/runner/leases/{missing_id}/heartbeat"),
            RUNNER_TOKEN,
            "missing-heartbeat",
            "missing-heartbeat-request",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(error(missing).await.code, ApiErrorCode::LeaseNotFound);

    let ttl = app
        .oneshot(post(
            &format!("/v1/runner/leases/{missing_id}/heartbeat"),
            RUNNER_TOKEN,
            "heartbeat-ttl",
            "heartbeat-ttl-request",
            Body::from(r#"{"ttl":300}"#),
        ))
        .await
        .unwrap();
    assert_eq!(ttl.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error(ttl).await.code, ApiErrorCode::InvalidRequest);
}

#[tokio::test]
async fn runner_reports_a_started_event_through_the_owned_live_lease() {
    let expected = lease();
    let response = app(
        FakeLeaseStore::with_lease(expected.clone()),
        ClientId::new(),
    )
    .oneshot(post(
        &format!("/v1/runner/leases/{}/events", expected.lease_id()),
        RUNNER_TOKEN,
        "started-event",
        "started-event-request",
        Body::from(format!(
            r#"{{"event_id":"{}","event":{{"type":"started"}}}}"#,
            uuid::Uuid::new_v4(),
        )),
    ))
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["job"]["state"], "running");
}

#[tokio::test]
async fn runner_reconciles_plex_but_user_cannot_call_the_runner_port() {
    let body = r#"{"path":"/plex/tv/Show/Season 01/Show - S01E02.mkv","canonical_id":"rezka://42","season":1,"episode":2}"#;
    let response = app_with_plex()
        .clone()
        .oneshot(post(
            "/v1/runner/plex/reconcile",
            RUNNER_TOKEN,
            "plex-runner",
            "plex-runner-request",
            Body::from(body),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["status"], "matched");
    assert_eq!(value["observation"]["episode"], 2);

    let forbidden = app_with_plex()
        .oneshot(post(
            "/v1/runner/plex/reconcile",
            VALID_TOKEN,
            "plex-user",
            "plex-user-request",
            Body::from(body),
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
}
