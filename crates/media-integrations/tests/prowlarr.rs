use std::time::Duration;

use media_integrations::prowlarr::{
    MediaQuery, ProwlarrClient, ProwlarrConfig, SearchPageRequest, SearchSession,
};
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn config(server: &MockServer) -> ProwlarrConfig {
    ProwlarrConfig::new(
        Url::parse(&server.uri()).unwrap(),
        SecretString::from("prowlarr-secret"),
        Duration::from_secs(2),
    )
    .unwrap()
}

fn releases() -> serde_json::Value {
    serde_json::json!([
        {
            "id": 99,
            "guid": "indexer-guid-b",
            "indexerId": 7,
            "indexer": "tracker-b",
            "title": "Example Show S02 1080p DUB HEVC-GROUP",
            "size": 2000,
            "seeders": 10,
            "protocol": "torrent",
            "infoHash": "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
            "magnetUrl": "magnet:?xt=urn:btih:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
            "subGroup": "GROUP"
        },
        {
            "id": 11,
            "guid": "indexer-guid-a",
            "indexerId": 3,
            "indexer": "tracker-a",
            "title": "Example Show S02 1080p DUB HEVC-GROUP",
            "size": 2000,
            "seeders": 10,
            "protocol": "torrent",
            "infoHash": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "downloadUrl": "https://prowlarr.invalid/download/a?apikey=never-print-provider-key",
            "subGroup": "GROUP"
        },
        {
            "id": 12,
            "guid": "wrong-season",
            "indexerId": 3,
            "indexer": "tracker-a",
            "title": "Example Show S01 2160p DUB HEVC-GROUP",
            "size": 9000,
            "seeders": 1000,
            "protocol": "torrent",
            "infoHash": "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
            "magnetUrl": "magnet:?xt=urn:btih:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"
        },
        {
            "id": 13,
            "guid": "wrong-language",
            "indexerId": 3,
            "indexer": "tracker-a",
            "title": "Example Show S02 2160p ENG AVC-OTHER",
            "size": 8000,
            "seeders": 100,
            "protocol": "torrent",
            "infoHash": "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD",
            "magnetUrl": "magnet:?xt=urn:btih:DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD"
        },
        {
            "id": 14,
            "guid": "usenet-result",
            "indexerId": 4,
            "title": "Example Show S02 1080p DUB HEVC-GROUP",
            "size": 2500,
            "seeders": 500,
            "protocol": "usenet"
        }
    ])
}

#[tokio::test]
async fn series_search_sends_exact_five_item_page_and_returns_stable_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(header("x-api-key", "prowlarr-secret"))
        .and(query_param("query", "Example Show"))
        .and(query_param("type", "tvsearch"))
        .and(query_param("indexerIds", "-2"))
        .and(query_param("categories", "5000"))
        .and(query_param("limit", "5"))
        .and(query_param("offset", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(releases()))
        .expect(1)
        .mount(&server)
        .await;

    let session = SearchSession::new(
        "session-42",
        MediaQuery::series("Example Show", 2)
            .prefer_quality(["1080p", "2160p"])
            .prefer_languages(["DUB", "ENG"])
            .prefer_codecs(["HEVC", "AVC"])
            .prefer_release_groups(["GROUP", "OTHER"]),
    )
    .unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(SearchPageRequest::new(session.clone(), 5).unwrap())
        .await
        .unwrap();

    assert_eq!(page.session, session);
    assert_eq!(page.offset, 5);
    assert_eq!(page.results.len(), 4, "non-torrent results are excluded");
    assert_eq!(page.results[0].identity.indexer_id, 3);
    assert_eq!(page.results[0].identity.guid, "indexer-guid-a");
    assert_eq!(page.results[1].identity.guid, "indexer-guid-b");
    assert_eq!(page.results[3].identity.guid, "wrong-season");
    let debug = format!("{page:?}");
    assert!(!debug.contains("never-print-provider-key"));
    assert!(debug.contains("[REDACTED]"));
    assert_eq!(
        page.continuation.unwrap(),
        SearchPageRequest::new(session, 10).unwrap()
    );
}

#[tokio::test]
async fn movie_search_uses_movie_type_and_never_submits_a_result() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("query", "Example Movie"))
        .and(query_param("type", "movie"))
        .and(query_param("indexerIds", "-2"))
        .and(query_param("categories", "2000"))
        .and(query_param("limit", "5"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let request = SearchPageRequest::new(
        SearchSession::new("movie-session", MediaQuery::movie("Example Movie")).unwrap(),
        0,
    )
    .unwrap();
    ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap();
}

#[tokio::test]
async fn ranking_exposes_every_required_factor_for_a_season_word_release() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": 1,
                "guid": "all-factors",
                "indexerId": 9,
                "title": "Example Show Season 2 1080p DUB HEVC-GROUP",
                "size": 4567,
                "seeders": 123,
                "protocol": "torrent",
                "infoHash": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "magnetUrl": "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "subGroup": "GROUP"
            },
            {
                "id": 2,
                "guid": "title-prefix-only",
                "indexerId": 9,
                "title": "Example Showdown Season 2 1080p DUB HEVC-GROUP",
                "size": 9999,
                "seeders": 999,
                "protocol": "torrent",
                "infoHash": "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
                "magnetUrl": "magnet:?xt=urn:btih:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
                "subGroup": "GROUP"
            }
        ])))
        .mount(&server)
        .await;
    let query = MediaQuery::series("Example Show", 2)
        .prefer_quality(["1080p", "720p"])
        .prefer_languages(["DUB", "ENG"])
        .prefer_codecs(["HEVC", "AVC"])
        .prefer_release_groups(["GROUP", "OTHER"]);
    let request = SearchPageRequest::new(SearchSession::new("factors", query).unwrap(), 0).unwrap();

    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap();
    let score = &page.results[0].ranking;
    assert!(score.exact_title);
    assert!(score.exact_season);
    assert_eq!(score.quality_preference, 2);
    assert_eq!(score.language_preference, 2);
    assert_eq!(score.seeders, 123);
    assert_eq!(score.size_bytes, 4567);
    assert_eq!(score.codec_preference, 2);
    assert_eq!(score.release_group_preference, 2);
    assert!(!page.results[1].ranking.exact_title);
    assert!(page.continuation.is_none());
}

#[test]
fn config_debug_redacts_api_key() {
    let config = ProwlarrConfig::new(
        Url::parse("http://localhost:9696").unwrap(),
        SecretString::from("never-print-this"),
        Duration::from_secs(1),
    )
    .unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("never-print-this"));
    assert!(debug.contains("[REDACTED]"));
}

#[tokio::test]
async fn provider_over_return_is_truncated_to_five_ranked_results() {
    let server = MockServer::start().await;
    let releases = (0..6)
        .map(|id| {
            serde_json::json!({
                "id": id,
                "guid": format!("guid-{id}"),
                "indexerId": 1,
                "title": format!("Movie 1080p release-{id}"),
                "size": 1000 + id,
                "seeders": id,
                "protocol": "torrent",
                "infoHash": format!("{id:040x}"),
                "magnetUrl": format!("magnet:?xt=urn:btih:{id:040x}")
            })
        })
        .collect::<Vec<_>>();
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(releases))
        .mount(&server)
        .await;
    let session = SearchSession::new("bounded", MediaQuery::movie("Movie")).unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(SearchPageRequest::new(session.clone(), 0).unwrap())
        .await
        .unwrap();

    assert_eq!(page.results.len(), 5);
    assert_eq!(page.results[0].identity.guid, "guid-5");
    assert_eq!(
        page.continuation,
        Some(SearchPageRequest::new(session, 5).unwrap())
    );
}
