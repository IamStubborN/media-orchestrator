mod support;

use std::sync::{Arc, Mutex};

use axum::{body::Body, http::Request};
use http_body_util::BodyExt as _;
use media_api::{SearchError, SearchService, router};
use media_contract::{
    ContinueSearchRequest, ExecutionSelectionDto, JobDto, JobStateDto, NotifyScopeDto, ProviderDto,
    SearchPageDto, SearchResultDto, SelectResultRequest, StartSearchRequest,
};
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole, OperationKey, RUNNER_CLIENT_ID, UserId,
};
use tower::ServiceExt as _;

use support::{FakeClientStore, FakeReadiness, RUNNER_TOKEN, VALID_TOKEN, state};

#[derive(Default)]
struct FakeSearchService {
    owners: Mutex<Vec<UserId>>,
}

#[async_trait::async_trait]
impl SearchService for FakeSearchService {
    async fn start(
        &self,
        owner: UserId,
        request: StartSearchRequest,
    ) -> Result<SearchPageDto, SearchError> {
        self.owners.lock().unwrap().push(owner);
        Ok(page(request.source, Some("session:5")))
    }

    async fn continue_search(
        &self,
        owner: UserId,
        _: ContinueSearchRequest,
    ) -> Result<SearchPageDto, SearchError> {
        self.owners.lock().unwrap().push(owner);
        Ok(page(ProviderDto::Rezka, None))
    }

    async fn select(
        &self,
        owner: UserId,
        _: OperationKey,
        _: SelectResultRequest,
    ) -> Result<JobDto, SearchError> {
        self.owners.lock().unwrap().push(owner);
        Ok(JobDto {
            id: media_contract::PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb111").unwrap(),
            provider: ProviderDto::Rezka,
            result_ref: "selection:018f3f86".to_owned(),
            state: JobStateDto::Queued,
            needs_action_reason: None,
            notify_scope: NotifyScopeDto::Initiator,
        })
    }

    async fn execution_for(&self, _: &str) -> Result<ExecutionSelectionDto, SearchError> {
        Err(SearchError::NotFound)
    }
}

fn page(source: ProviderDto, continuation: Option<&str>) -> SearchPageDto {
    SearchPageDto {
        api_version: "v1".to_owned(),
        session_id: "018f3f86-7b4c-7b4f-9b6a-6d62f45bb112".to_owned(),
        source,
        expires_at: "2026-07-13T12:00:00Z".to_owned(),
        results: vec![SearchResultDto::Prowlarr {
            result_id: "result-1".to_owned(),
            title: "Movie".to_owned(),
            indexer: None,
            size_bytes: 100,
            seeders: 3,
            release_group: None,
            ranking: media_contract::ProwlarrRankingDto {
                exact_title: true,
                exact_season: true,
                quality_preference: 0,
                language_preference: 0,
                seeders: 3,
                size_bytes: 100,
                codec_preference: 0,
                release_group_preference: 0,
            },
        }],
        continuation: continuation.map(str::to_owned),
    }
}

fn app(service: Arc<FakeSearchService>) -> axum::Router {
    let user = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let runner = Actor::new(RUNNER_CLIENT_ID, None, ClientRole::Runner).unwrap();
    router(
        state(
            FakeClientStore::new([(VALID_TOKEN, user), (RUNNER_TOKEN, runner)]),
            FakeReadiness::ready(),
        )
        .with_search(service),
    )
}

fn post(path: &str, token: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("authorization", format!("Bearer {token}"))
        .header("x-request-id", "search-test")
        .header("idempotency-key", format!("key-{path}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn authenticated_user_searches_continues_and_selects_without_identity_flags() {
    let service = Arc::new(FakeSearchService::default());
    let app = app(service.clone());

    let response = app
        .clone()
        .oneshot(post(
            "/v1/searches",
            VALID_TOKEN,
            serde_json::json!({
                "scope":{"platform":"telegram","chat_id":"42"},
                "source":"prowlarr","query":"Movie"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["continuation"], "session:5");
    assert!(!body.to_string().contains("magnet"));

    let response = app
        .clone()
        .oneshot(post(
            "/v1/searches/continue",
            VALID_TOKEN,
            serde_json::json!({
                "continuation":"session:5",
                "scope":{"platform":"telegram","chat_id":"42"}
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let response = app
        .oneshot(post(
            "/v1/selections",
            VALID_TOKEN,
            serde_json::json!({
                "session_id":"018f3f86-7b4c-7b4f-9b6a-6d62f45bb112",
                "result_id":"result-1",
                "translation_id":37,
                "season":1,
                "episode":2
                ,"scope":{"platform":"telegram","chat_id":"42"}
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(
        service.owners.lock().unwrap().as_slice(),
        &[PRIMARY_USER_ID; 3]
    );
}

#[tokio::test]
async fn runner_cannot_search_and_requested_by_is_rejected() {
    let app = app(Arc::new(FakeSearchService::default()));
    let response = app
        .clone()
        .oneshot(post(
            "/v1/searches",
            RUNNER_TOKEN,
            serde_json::json!({"source":"rezka","query":"Movie"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 403);

    let response = app
        .oneshot(post(
            "/v1/searches",
            VALID_TOKEN,
            serde_json::json!({"source":"rezka","query":"Movie","requested_by":"other"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}
