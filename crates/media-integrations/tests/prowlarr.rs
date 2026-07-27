use std::time::Duration;

use media_integrations::prowlarr::{
    EpisodeAvailabilityQuery, MediaQuery, ProwlarrClient, ProwlarrConfig, ProwlarrErrorCode,
    SearchPageRequest, SearchSession,
};
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

const TORRENT_INFO_HASH: &str = "d2939e5af6d595ecdfd4d11563f16986535b6b98";
const TORRENT_BYTES: &[u8] = b"d8:announce14:http://tracker4:infod6:lengthi5e4:name8:file.txt12:piece lengthi16384e6:pieces20:12345678901234567890ee";

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

fn indexers(ids: &[i32]) -> serde_json::Value {
    serde_json::Value::Array(
        ids.iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "enable": true,
                    "protocol": "torrent"
                })
            })
            .collect(),
    )
}

const EMPTY_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss><channel><title>Prowlarr</title></channel></rss>"#;

const AVAILABLE_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss><channel><title>Prowlarr</title><item><title>Example Show S03E05</title>
<link>https://prowlarr.invalid/api/v1/indexer/7/download?id=one</link>
<enclosure url="https://prowlarr.invalid/api/v1/indexer/7/download?id=one" type="application/x-bittorrent" />
</item></channel></rss>"#;

const NON_MATCHING_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss><channel><title>Prowlarr</title>
<item><title>Example Show S03 Complete</title>
<link>https://prowlarr.invalid/api/v1/indexer/7/download?id=pack</link></item>
<item><title>Example Show S03E04</title>
<enclosure url="https://prowlarr.invalid/api/v1/indexer/7/download?id=wrong" type="application/x-bittorrent" />
</item></channel></rss>"#;

#[tokio::test]
async fn exact_episode_probe_uses_enabled_indexers_and_accepts_any_usable_result() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/indexer"))
        .and(header("x-api-key", "prowlarr-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(indexers(&[3, 7])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/indexer/3/newznab"))
        .and(query_param("t", "tvsearch"))
        .and(query_param("q", "Example Show"))
        .and(query_param("season", "3"))
        .and(query_param("ep", "5"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/indexer/7/newznab"))
        .and(query_param("t", "tvsearch"))
        .and(query_param("q", "Example Show"))
        .and(query_param("season", "3"))
        .and(query_param("ep", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_string(AVAILABLE_FEED))
        .expect(1)
        .mount(&server)
        .await;

    let query = EpisodeAvailabilityQuery::new(vec!["Example Show".to_owned()], 3, 5).unwrap();
    assert!(
        ProwlarrClient::new(config(&server))
            .unwrap()
            .episode_available(&query)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn exact_episode_probe_distinguishes_empty_results_from_provider_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/indexer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(indexers(&[3])))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/indexer/3/newznab"))
        .and(query_param("q", "Unavailable Show"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EMPTY_FEED))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/indexer/3/newznab"))
        .and(query_param("q", "Broken Show"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .mount(&server)
        .await;

    let client = ProwlarrClient::new(config(&server)).unwrap();
    let unavailable =
        EpisodeAvailabilityQuery::new(vec!["Unavailable Show".to_owned()], 3, 5).unwrap();
    assert!(!client.episode_available(&unavailable).await.unwrap());

    let broken = EpisodeAvailabilityQuery::new(vec!["Broken Show".to_owned()], 3, 5).unwrap();
    let error = client.episode_available(&broken).await.unwrap_err();
    assert_eq!(error.code(), ProwlarrErrorCode::ProviderResponse);
}

#[tokio::test]
async fn exact_episode_probe_rejects_season_packs_and_other_episode_coordinates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/indexer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(indexers(&[7])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/indexer/7/newznab"))
        .and(query_param("q", "Example Show"))
        .and(query_param("season", "3"))
        .and(query_param("ep", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_string(NON_MATCHING_FEED))
        .expect(1)
        .mount(&server)
        .await;

    let query = EpisodeAvailabilityQuery::new(vec!["Example Show".to_owned()], 3, 5).unwrap();
    assert!(
        !ProwlarrClient::new(config(&server))
            .unwrap()
            .episode_available(&query)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn series_search_sends_exact_five_item_page_and_ranks_valid_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(header("x-api-key", "prowlarr-secret"))
        .and(query_param("query", "Example Show"))
        .and(query_param("type", "tvsearch"))
        .and(query_param("indexerIds", "-2"))
        .and(query_param("limit", "100"))
        .and(query_param("offset", "0"))
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
        .search(SearchPageRequest::new(session.clone(), 0).unwrap())
        .await
        .unwrap();

    assert_eq!(page.session, session);
    assert_eq!(page.offset, 0);
    assert_eq!(page.results.len(), 4, "non-torrent results are excluded");
    assert_eq!(page.results[0].identity.indexer_id, 3);
    assert_eq!(page.results[0].identity.guid, "indexer-guid-a");
    assert_eq!(
        page.results[0].source.info_hash.as_deref(),
        Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
    );
    assert_eq!(page.results[1].identity.guid, "indexer-guid-b");
    assert_eq!(page.results[3].identity.guid, "wrong-season");
    let debug = format!("{page:?}");
    assert!(!debug.contains("never-print-provider-key"));
    assert!(debug.contains("[REDACTED]"));
    assert!(page.continuation.is_none());
}

#[tokio::test]
async fn movie_search_uses_movie_type_and_never_submits_a_result() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("query", "Example Movie"))
        .and(query_param("type", "movie"))
        .and(query_param("indexerIds", "-2"))
        .and(query_param("limit", "100"))
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

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .url
            .query_pairs()
            .all(|(name, _)| name != "categories")
    );
}

#[tokio::test]
async fn search_accepts_null_release_ids_from_prowlarr() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": null,
                "guid": "https://tracker.example/topic/42",
                "indexerId": 7,
                "indexer": "tracker",
                "title": "Example Movie 1080p",
                "size": 2000,
                "seeders": 10,
                "protocol": "torrent",
                "infoHash": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "magnetUrl": "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            }])),
        )
        .expect(1)
        .mount(&server)
        .await;

    let request = SearchPageRequest::new(
        SearchSession::new("null-id", MediaQuery::movie("Example Movie")).unwrap(),
        0,
    )
    .unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap();

    assert_eq!(page.results.len(), 1);
    assert!(page.results[0].identity.result_id > 0);
}

#[tokio::test]
async fn malformed_release_does_not_hide_valid_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": 1,
                "guid": "valid-release",
                "indexerId": 7,
                "title": "Example Movie 1080p",
                "size": 2000,
                "seeders": 10,
                "protocol": "torrent",
                "infoHash": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            },
            {
                "id": 2,
                "guid": "malformed-release",
                "indexerId": "not-an-integer",
                "title": "Broken provider row",
                "protocol": "torrent"
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let request = SearchPageRequest::new(
        SearchSession::new("mixed-results", MediaQuery::movie("Example Movie")).unwrap(),
        0,
    )
    .unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap();

    assert_eq!(page.results.len(), 1);
    assert_eq!(page.results[0].identity.guid, "valid-release");
}

#[tokio::test]
async fn download_url_is_resolved_to_exact_v1_info_hash() {
    let server = MockServer::start().await;
    let download_url = format!("{}/download/one?apikey=provider-secret", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": 1,
                "guid": "download-only",
                "indexerId": 9,
                "title": "Example Movie 1080p",
                "size": 5,
                "seeders": 10,
                "protocol": "torrent",
                "downloadUrl": download_url
            }])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/one"))
        .and(header("x-api-key", "prowlarr-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(TORRENT_BYTES))
        .expect(1)
        .mount(&server)
        .await;

    let request = SearchPageRequest::new(
        SearchSession::new("download-only", MediaQuery::movie("Example Movie")).unwrap(),
        0,
    )
    .unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap();

    assert_eq!(page.results.len(), 1);
    assert_eq!(
        page.results[0].source.info_hash.as_deref(),
        Some(TORRENT_INFO_HASH)
    );
    assert_eq!(
        page.results[0].source.download_url.as_deref(),
        Some(download_url.as_str())
    );
}

#[tokio::test]
async fn malformed_torrent_is_rejected_without_leaking_download_url() {
    let server = MockServer::start().await;
    let secret = "never-print-provider-key";
    let download_url = format!("{}/download/bad?apikey={secret}", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": 2,
                "guid": "malformed",
                "indexerId": 9,
                "title": "Example Movie 1080p",
                "protocol": "torrent",
                "downloadUrl": download_url
            }])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/bad"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"not-bencoded"))
        .expect(1)
        .mount(&server)
        .await;

    let request = SearchPageRequest::new(
        SearchSession::new("malformed", MediaQuery::movie("Example Movie")).unwrap(),
        0,
    )
    .unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap();

    assert!(page.results.is_empty());
    let debug = format!("{page:?}");
    assert!(!debug.contains(secret));
}

#[tokio::test]
async fn valid_bencode_without_torrent_info_is_rejected() {
    let server = MockServer::start().await;
    let download_url = format!("{}/download/not-torrent", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": 4,
                "guid": "not-torrent",
                "indexerId": 9,
                "title": "Example Movie 1080p",
                "protocol": "torrent",
                "downloadUrl": download_url
            }])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/not-torrent"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"d4:name8:not-filee"))
        .expect(1)
        .mount(&server)
        .await;

    let request = SearchPageRequest::new(
        SearchSession::new("not-torrent", MediaQuery::movie("Example Movie")).unwrap(),
        0,
    )
    .unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap();

    assert!(page.results.is_empty());
}

#[tokio::test]
async fn oversized_torrent_is_rejected() {
    let server = MockServer::start().await;
    let download_url = format!("{}/download/large", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": 3,
                "guid": "oversized",
                "indexerId": 9,
                "title": "Example Movie 1080p",
                "protocol": "torrent",
                "downloadUrl": download_url
            }])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/large"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 8 * 1024 * 1024 + 1]))
        .expect(1)
        .mount(&server)
        .await;

    let request = SearchPageRequest::new(
        SearchSession::new("oversized", MediaQuery::movie("Example Movie")).unwrap(),
        0,
    )
    .unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap();

    assert!(page.results.is_empty());
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

#[tokio::test]
async fn oversized_search_body_is_rejected_before_deserialization() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 8 * 1024 * 1024 + 1]))
        .expect(1)
        .mount(&server)
        .await;

    let request = SearchPageRequest::new(
        SearchSession::new("oversized-body", MediaQuery::movie("Example Movie")).unwrap(),
        0,
    )
    .unwrap();
    let error = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(request)
        .await
        .unwrap_err();

    assert_eq!(error.code(), ProwlarrErrorCode::ProviderResponse);
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

#[tokio::test]
async fn later_pages_are_sliced_locally_from_the_full_ranked_result_set() {
    let server = MockServer::start().await;
    let releases = (0..6)
        .map(|id| {
            serde_json::json!({
                "id": id + 1,
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
        .and(query_param("limit", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(releases))
        .expect(1)
        .mount(&server)
        .await;

    let session = SearchSession::new("second-page", MediaQuery::movie("Movie")).unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(SearchPageRequest::new(session, 5).unwrap())
        .await
        .unwrap();

    assert_eq!(page.results.len(), 1);
    assert_eq!(page.results[0].identity.guid, "guid-0");
    assert!(page.continuation.is_none());
}

#[tokio::test]
async fn later_pages_never_request_a_shifted_upstream_offset() {
    let server = MockServer::start().await;
    let releases = (0..12)
        .map(|id| {
            serde_json::json!({
                "id": id + 1,
                "guid": format!("guid-{id}"),
                "indexerId": 1,
                "title": format!("Movie 1080p release-{id}"),
                "size": 1000 + id,
                "seeders": 100 - id,
                "protocol": "torrent",
                "infoHash": format!("{id:040x}"),
                "magnetUrl": format!("magnet:?xt=urn:btih:{id:040x}")
            })
        })
        .collect::<Vec<_>>();
    // A Prowlarr that honors the window returns the full candidate set only for
    // offset=0; any shifted upstream offset would come back empty.
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(releases))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("offset", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(0)
        .mount(&server)
        .await;

    let session = SearchSession::new("deep-page", MediaQuery::movie("Movie")).unwrap();
    let page = ProwlarrClient::new(config(&server))
        .unwrap()
        .search(SearchPageRequest::new(session.clone(), 5).unwrap())
        .await
        .unwrap();

    assert_eq!(page.results.len(), 5);
    assert_eq!(page.results[0].identity.guid, "guid-5");
    assert_eq!(
        page.continuation,
        Some(SearchPageRequest::new(session, 10).unwrap())
    );
}
