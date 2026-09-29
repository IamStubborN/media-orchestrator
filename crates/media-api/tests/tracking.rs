mod support;

use std::sync::{Arc, Mutex};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::router;
use media_core::{
    Actor, ClientRole, EpisodeSnapshot, NewTrackingSubscription, OperationKey, PRIMARY_CLIENT_ID,
    PRIMARY_USER_ID, PortError, SECONDARY_CLIENT_ID, SECONDARY_USER_ID, TrackingDownloadPatch,
    TrackingId, TrackingStore, TrackingSubscription, UserId,
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

    async fn patch_download_visible(
        &self,
        id: TrackingId,
        user: UserId,
        patch: TrackingDownloadPatch,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let mut values = self.values.lock().unwrap();
        let Some(index) = values
            .iter()
            .position(|value| value.id() == id && value.is_visible_to(user))
        else {
            return Ok(None);
        };
        let value = &values[index];
        let updated = TrackingSubscription::rehydrate_with_poster(
            value.id(),
            value.owner_id(),
            value.provider(),
            value.title().to_owned(),
            patch.translation().to_owned(),
            value.known_episodes().to_vec(),
            value.scope(),
            Some(patch.download().clone()),
            value.poster_url().map(str::to_owned),
        )
        .map_err(|_| PortError::Conflict)?;
        values[index] = updated.clone();
        Ok(Some(updated))
    }

    async fn set_baseline_visible(
        &self,
        id: TrackingId,
        user: UserId,
        baseline: EpisodeSnapshot,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let mut values = self.values.lock().unwrap();
        let Some(index) = values
            .iter()
            .position(|value| value.id() == id && value.is_visible_to(user))
        else {
            return Ok(None);
        };
        let value = &values[index];
        let mut known = value
            .known_episodes()
            .iter()
            .copied()
            .filter(|episode| episode.season() != baseline.season())
            .collect::<Vec<_>>();
        known.extend(
            (1..=baseline.episode())
                .map(|episode| EpisodeSnapshot::new(baseline.season(), episode).unwrap()),
        );
        let updated = TrackingSubscription::rehydrate_with_poster(
            value.id(),
            value.owner_id(),
            value.provider(),
            value.title().to_owned(),
            value.translation().to_owned(),
            known,
            value.scope(),
            value.download().cloned(),
            value.poster_url().map(str::to_owned),
        )
        .map_err(|_| PortError::Conflict)?;
        values[index] = updated.clone();
        Ok(Some(updated))
    }

    async fn request_check_visible(
        &self,
        id: TrackingId,
        user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .iter()
            .find(|value| value.id() == id && value.is_visible_to(user))
            .cloned())
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
    r#"{"provider":"rezka","title":"Ongoing Show","translation":"Studio Dub","known_episodes":[{"season":1,"episode":4}],"scope":"family","series_ongoing":true,"poster_url":"https://image.tmdb.org/t/p/w780/show.jpg","release_identity":{"source":"tvmaze","source_id":77}}"#
}

fn download_body() -> &'static str {
    r#"{"provider":"rezka","title":"Blades of the Guardians S2","translation":"Studio Dub","known_episodes":[{"season":2,"episode":7}],"scope":"personal","series_ongoing":true,"download":{"provider_media_ref":"42513","translation_id":19,"season":2}}"#
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
    assert_eq!(value["release_identity"]["source"], "tvmaze");
    assert_eq!(value["release_identity"]["source_id"], 77);
    assert_eq!(
        value["poster_url"],
        "https://image.tmdb.org/t/p/w780/show.jpg"
    );
    assert!(value.get("owner_id").is_none());
    assert!(value.get("auto_download").is_none());
    let id = value["id"].as_str().unwrap();

    let patched = app
        .clone()
        .oneshot(
            Request::patch(format!("/v1/tracking/{id}"))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {SECONDARY_TOKEN}"),
                )
                .header("idempotency-key", "tracking-enable-download")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"translation":"DEEP","download":{"provider_media_ref":"88337","translation_id":509,"season":4}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched: serde_json::Value =
        serde_json::from_slice(&to_bytes(patched.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(patched["id"], id);
    assert_eq!(patched["translation"], "DEEP");
    assert_eq!(patched["known_episodes"][0]["episode"], 4);
    assert_eq!(patched["scope"], "family");
    assert_eq!(patched["download"]["provider_media_ref"], "88337");
    assert_eq!(patched["download"]["translation_id"], 509);
    assert_eq!(patched["download"]["season"], 4);

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
    assert_eq!(
        listed["tracking"][0]["poster_url"],
        "https://image.tmdb.org/t/p/w780/show.jpg"
    );

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
async fn owner_can_update_baseline_and_request_an_immediate_check() {
    let app = app();
    let created = app
        .clone()
        .oneshot(
            Request::post("/v1/tracking")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("idempotency-key", "tracking-controls-add")
                .header("content-type", "application/json")
                .body(Body::from(create_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(created.into_body(), usize::MAX).await.unwrap()).unwrap();
    let id = value["id"].as_str().unwrap();

    let baseline = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/tracking/{id}/baseline"))
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("idempotency-key", "tracking-controls-baseline")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"known_through":{"season":2,"episode":6}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(baseline.status(), StatusCode::OK);
    let baseline: serde_json::Value =
        serde_json::from_slice(&to_bytes(baseline.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(baseline["known_episodes"].as_array().unwrap().len(), 7);
    assert_eq!(baseline["known_episodes"][6]["season"], 2);
    assert_eq!(baseline["known_episodes"][6]["episode"], 6);

    let check = app
        .oneshot(
            Request::post(format!("/v1/tracking/{id}/check"))
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("idempotency-key", "tracking-controls-check")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(check.status(), StatusCode::OK);
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

#[tokio::test]
async fn authenticated_owner_can_create_an_exact_rezka_download_subscription() {
    let response = app()
        .oneshot(
            Request::post("/v1/tracking")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("idempotency-key", "tracking-download-add")
                .header("content-type", "application/json")
                .body(Body::from(download_body()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["download"]["provider_media_ref"], "42513");
    assert_eq!(value["download"]["translation_id"], 19);
    assert_eq!(value["download"]["season"], 2);
    assert!(value.get("owner_id").is_none());
}

#[tokio::test]
async fn automatic_download_rejects_a_prefixed_rezka_reference() {
    let body = download_body().replace("42513", "rezka:42513");
    let response = app()
        .oneshot(
            Request::post("/v1/tracking")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .header("idempotency-key", "tracking-download-prefixed-ref")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
