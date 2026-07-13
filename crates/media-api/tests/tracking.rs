mod support;

use std::sync::{Arc, Mutex};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::router;
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole, NewTrackingSubscription, OperationKey,
    PortError, TrackingId, TrackingStore, TrackingSubscription, UserId, SECONDARY_CLIENT_ID,
    SECONDARY_USER_ID,
};
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, SECONDARY_TOKEN, VALID_TOKEN, state};

#[derive(Default)]
struct FakeTrackingStore {
    values: Mutex<Vec<TrackingSubscription>>,
}

#[async_trait::async_trait]
impl TrackingStore for FakeTrackingStore {
    async fn add(
        &self,
        _: OperationKey,
        value: NewTrackingSubscription,
    ) -> Result<TrackingSubscription, PortError> {
        let value = value.into_persisted();
        self.values.lock().unwrap().push(value.clone());
        Ok(value)
    }

    async fn list_visible(&self, user: UserId) -> Result<Vec<TrackingSubscription>, PortError> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .iter()
            .filter(|value| value.is_visible_to(user))
            .cloned()
            .collect())
    }

    async fn remove_visible(
        &self,
        _: OperationKey,
        id: TrackingId,
        user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let mut values = self.values.lock().unwrap();
        let index = values
            .iter()
            .position(|value| value.id() == id && value.is_visible_to(user));
        Ok(index.map(|index| values.remove(index)))
    }
}

fn app() -> axum::Router {
    let primary = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let secondary = Actor::new(
        SECONDARY_CLIENT_ID,
        Some(SECONDARY_USER_ID),
        ClientRole::Hermes,
    )
    .unwrap();
    router(
        state(
            FakeClientStore::new([(VALID_TOKEN, primary), (SECONDARY_TOKEN, secondary)]),
            FakeReadiness::ready(),
        )
        .with_tracking(Arc::new(media_core::TrackingApplication::new(Arc::new(
            FakeTrackingStore::default(),
        )))),
    )
}

fn create_body() -> &'static str {
    r#"{"provider":"rezka","title":"Ongoing Show","translation":"Studio Dub","known_episodes":[{"season":1,"episode":4}],"scope":"family","series_ongoing":true}"#
}

#[tokio::test]
async fn authenticated_owner_can_add_list_and_other_family_user_can_remove() {
    let app = app();
    let created = app
        .clone()
        .oneshot(
            Request::post("/v1/tracking")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("idempotency-key", "tracking-add")
                .header("content-type", "application/json")
                .body(Body::from(create_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(created.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["scope"], "family");
    assert_eq!(value["state"], "active");
    assert!(value.get("owner_id").is_none());
    assert!(value.get("auto_download").is_none());
    let id = value["id"].as_str().unwrap();

    let listed = app
        .clone()
        .oneshot(
            Request::get("/v1/tracking")
                .header(header::AUTHORIZATION, format!("Bearer {SECONDARY_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed: serde_json::Value =
        serde_json::from_slice(&to_bytes(listed.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(listed["tracking"].as_array().unwrap().len(), 1);

    let removed = app
        .oneshot(
            Request::delete(format!("/v1/tracking/{id}"))
                .header(header::AUTHORIZATION, format!("Bearer {SECONDARY_TOKEN}"))
                .header("idempotency-key", "tracking-remove")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    let removed: serde_json::Value =
        serde_json::from_slice(&to_bytes(removed.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(removed["state"], "removed");
}

#[tokio::test]
async fn create_rejects_owner_and_auto_download_fields() {
    for field in ["owner_id", "requested_by", "auto_download"] {
        let mut body: serde_json::Value = serde_json::from_str(create_body()).unwrap();
        body.as_object_mut()
            .unwrap()
            .insert(field.to_owned(), serde_json::json!(true));
        let response = app()
            .oneshot(
                Request::post("/v1/tracking")
                    .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                    .header("idempotency-key", format!("reject-{field}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
