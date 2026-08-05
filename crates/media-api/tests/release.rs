mod support;

use std::sync::{Arc, Mutex};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::router;
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole, ReleaseCandidate, ReleaseLifecycle,
    ReleaseMetadataPort, ReleaseMetadataResult, ReleaseQuery, ReleaseQueryError,
};
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, VALID_TOKEN, state};

struct FakeReleaseProvider {
    query: Mutex<Option<ReleaseQuery>>,
}

#[async_trait::async_trait]
impl ReleaseMetadataPort for FakeReleaseProvider {
    async fn query(
        &self,
        query: &ReleaseQuery,
    ) -> Result<ReleaseMetadataResult, ReleaseQueryError> {
        *self.query.lock().unwrap() = Some(query.clone());
        Ok(ReleaseMetadataResult::ChoiceNeeded {
            source: "tvmaze".to_owned(),
            fetched_at: "2026-07-13T12:00:00Z".to_owned(),
            candidates: vec![ReleaseCandidate {
                source_id: 42,
                title: "The Office".to_owned(),
                original_title: None,
                year: Some(2005),
                lifecycle: ReleaseLifecycle::Ended,
            }],
        })
    }
}

#[tokio::test]
async fn query_returns_explicit_choice_without_idempotency_or_job_creation() {
    let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let provider = Arc::new(FakeReleaseProvider {
        query: Mutex::new(None),
    });
    let app = router(
        state(
            FakeClientStore::new([(VALID_TOKEN, actor)]),
            FakeReadiness::ready(),
        )
        .with_release_metadata(Arc::new(media_core::ReleaseMetadataService::new(
            provider.clone(),
        ))),
    );

    let response = app
        .oneshot(
            Request::post("/v1/releases/query")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"title":"Офис","original_title":"The Office","year":2005,"source_id":42}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["status"], "choice_needed");
    assert_eq!(body["source"], "tvmaze");
    assert_eq!(
        provider.query.lock().unwrap().as_ref().unwrap().year,
        Some(2005)
    );
    assert_eq!(
        provider.query.lock().unwrap().as_ref().unwrap().source_id,
        Some(42)
    );
}
