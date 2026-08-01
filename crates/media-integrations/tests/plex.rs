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
async fn library_summary_returns_only_configured_sections_with_bounded_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/sections"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaContainer": {"Directory": [
                {"key": "2", "title": "Films", "type": "movie"},
                {"key": "7", "title": "Series", "type": "show"},
                {"key": "9", "title": "Private", "type": "movie"}
            ]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/library/sections/2/all"))
        .and(query_param("X-Plex-Container-Start", "0"))
        .and(query_param("X-Plex-Container-Size", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaContainer": {"size": 1, "totalSize": 42, "viewGroup": "movie"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/library/sections/7/all"))
        .and(query_param("X-Plex-Container-Start", "0"))
        .and(query_param("X-Plex-Container-Size", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaContainer": {"size": 1, "totalSize": 17, "viewGroup": "show"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let summary = PlexClient::new(config(&server))
        .unwrap()
        .admin_library_summary(&[2, 7])
        .await
        .unwrap();

    assert_eq!(summary["sections"][0]["title"], "Films");
    assert_eq!(summary["sections"][0]["item_count"], 42);
    assert_eq!(summary["sections"][1]["title"], "Series");
    assert_eq!(summary["sections"][1]["item_count"], 17);
    assert_eq!(summary["sections"].as_array().unwrap().len(), 2);
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

#[tokio::test]
async fn specials_episode_accepts_zero_season_and_verifies_parent_index() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/metadata/45"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaContainer": {
                "size": 1,
                "Metadata": [{
                    "ratingKey": "45",
                    "guid": "plex://episode/special",
                    "type": "episode",
                    "parentIndex": 0,
                    "index": 1,
                    "Guid": [{"id": "tvdb://special-1"}],
                    "Media": [{"Part": [{"file": "/tv/Show/Specials/Show - S00E01.mkv"}]}]
                }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = PlexClient::new(config(&server)).unwrap();
    let expected = ExpectedPlexItem::episode(
        45,
        "/tv/Show/Specials/Show - S00E01.mkv",
        "tvdb://special-1",
        0,
        1,
    )
    .unwrap();

    assert!(matches!(
        client.verify(&expected).await.unwrap(),
        PlexVerification::Matched { rating_key: 45, .. }
    ));
}

#[tokio::test]
async fn path_verification_finds_the_exact_scanned_episode_without_a_rating_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/sections/7/all"))
        .and(query_param("includeGuids", "1"))
        .and(query_param("type", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MediaContainer": {"Metadata": [{
                "ratingKey": "321", "guid": "plex://episode/abcdef", "type": "episode",
                "parentIndex": 2, "index": 4, "Guid": [{"id": "tvdb://456"}],
                "Media": [{"Part": [{"file": "/plex/tv/Show/Season 02/Show - S02E04.mkv"}]}]
            }]}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let verification = PlexClient::new(config(&server))
        .unwrap()
        .verify_path(
            7,
            std::path::Path::new("/plex/tv/Show/Season 02/Show - S02E04.mkv"),
            "rezka://42",
            Some(2),
            Some(4),
        )
        .await
        .unwrap();

    assert_eq!(
        verification,
        PlexVerification::Matched {
            rating_key: 321,
            plex_guid: "plex://episode/abcdef".to_owned(),
        }
    );
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
