use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use media::composition::PlexReconcileAdapter;
use media_api::PlexReconcileService;
use media_contract::{PlexReconcileRequest, PlexReconcileStatus};
use media_integrations::plex::{PlexClient, PlexConfig};
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path},
};

#[derive(Clone)]
struct DelayedPlexItem {
    calls: Arc<AtomicUsize>,
}

impl Respond for DelayedPlexItem {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"MediaContainer": {"Metadata": []}}));
        }
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaContainer": {"Metadata": [{
                "ratingKey": "321",
                "guid": "plex://episode/abcdef",
                "type": "episode",
                "parentIndex": 2,
                "index": 4,
                "Guid": [],
                "Media": [{"Part": [{"file": "/plex/tv/Show/Season 02/Show - S02E04.mp4"}]}]
            }]}
        }))
    }
}

fn adapter(server: &MockServer, max_wait: Duration) -> PlexReconcileAdapter {
    let config = PlexConfig::new(
        Url::parse(&server.uri()).unwrap(),
        SecretString::from("plex-secret"),
        Duration::from_secs(1),
    )
    .unwrap();
    PlexReconcileAdapter::new(PlexClient::new(config).unwrap(), 7, 8)
        .with_polling(Duration::from_millis(1), max_wait)
}

fn request() -> PlexReconcileRequest {
    PlexReconcileRequest {
        path: "/plex/tv/Show/Season 02/Show - S02E04.mp4".to_owned(),
        canonical_id: "rezka://42".to_owned(),
        season: Some(2),
        episode: Some(4),
    }
}

#[tokio::test]
async fn reconcile_polls_until_the_scanned_item_appears() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/sections/7/refresh"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/library/sections/7/all"))
        .and(wiremock::matchers::query_param("type", "4"))
        .respond_with(DelayedPlexItem {
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let response = adapter(&server, Duration::from_millis(50))
        .reconcile(request())
        .await
        .unwrap();

    assert_eq!(response.status, PlexReconcileStatus::Matched);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reconcile_returns_pending_after_the_polling_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/sections/7/refresh"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/library/sections/7/all"))
        .and(wiremock::matchers::query_param("type", "4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"MediaContainer": {"Metadata": []}})),
        )
        .mount(&server)
        .await;

    let response = adapter(&server, Duration::from_millis(3))
        .reconcile(request())
        .await
        .unwrap();

    assert_eq!(response.status, PlexReconcileStatus::Pending);
    assert!(response.observation.is_none());
}

#[tokio::test]
async fn transient_scan_failure_returns_pending_for_later_reconciliation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/sections/7/refresh"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    let response = adapter(&server, Duration::from_millis(10))
        .reconcile(request())
        .await
        .unwrap();

    assert_eq!(response.status, PlexReconcileStatus::Pending);
    assert!(response.observation.is_none());
}
