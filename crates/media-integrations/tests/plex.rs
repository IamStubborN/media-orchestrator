use std::{path::PathBuf, time::Duration};

use media_integrations::plex::{
    ExpectedPlexItem, PlexClient, PlexConfig, PlexMismatch, PlexVerification, ScanRequest,
};
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn config(server: &MockServer) -> PlexConfig {
    PlexConfig::new(
        Url::parse(&server.uri()).unwrap(),
        SecretString::from("plex-secret"),
        Duration::from_secs(2),
    )
    .unwrap()
}

#[tokio::test]
async fn targeted_scan_and_exact_episode_verification_use_read_only_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/sections/7/refresh"))
        .and(header("x-plex-token", "plex-secret"))
        .and(query_param("path", "/downloads/media-tv/Example Show"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/library/metadata/321"))
        .and(header("x-plex-token", "plex-secret"))
        .and(header("accept", "application/json"))
        .and(query_param("includeGuids", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaContainer": {
                "size": 1,
                "Metadata": [{
                    "ratingKey": "321",
                    "guid": "plex://episode/abcdef",
                    "type": "episode",
                    "parentIndex": 2,
                    "index": 4,
                    "Guid": [{"id": "tmdb://98765"}, {"id": "tvdb://456"}],
                    "Media": [{"Part": [{"file": "/downloads/media-tv/Example Show/episode.mkv"}]}]
                }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = PlexClient::new(config(&server)).unwrap();
    client
        .trigger_scan(&ScanRequest::new(7, "/downloads/media-tv/Example Show").unwrap())
        .await
        .unwrap();
    let expected = ExpectedPlexItem::episode(
        321,
        PathBuf::from("/downloads/media-tv/Example Show/episode.mkv"),
        "tmdb://98765",
        2,
        4,
    )
    .unwrap();
    let verification = client.verify(&expected).await.unwrap();

    assert_eq!(
        verification,
        PlexVerification::Matched {
            rating_key: 321,
            plex_guid: "plex://episode/abcdef".into(),
        }
    );
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|request| request.method.as_str() == "GET")
    );
}

#[tokio::test]
async fn mismatch_is_reported_without_any_mutating_followup() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/metadata/44"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaContainer": {
                "size": 1,
                "Metadata": [{
                    "ratingKey": "44",
                    "guid": "plex://episode/wrong",
                    "type": "episode",
                    "parentIndex": 1,
                    "index": 8,
                    "Guid": [{"id": "tmdb://111"}],
                    "Media": [{"Part": [{"file": "/wrong/path.mkv"}]}]
                }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = PlexClient::new(config(&server)).unwrap();
    let expected = ExpectedPlexItem::episode(44, "/expected/path.mkv", "tmdb://222", 2, 3).unwrap();
    let verification = client.verify(&expected).await.unwrap();
    assert_eq!(
        verification,
        PlexVerification::Mismatch(vec![
            PlexMismatch::Path,
            PlexMismatch::CanonicalIdentity,
            PlexMismatch::Season,
            PlexMismatch::Episode,
        ])
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
}

#[test]
fn token_is_redacted_from_config_debug() {
    let config = PlexConfig::new(
        Url::parse("http://localhost:32400").unwrap(),
        SecretString::from("never-print-token"),
        Duration::from_secs(1),
    )
    .unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("never-print-token"));
    assert!(debug.contains("[REDACTED]"));
}
