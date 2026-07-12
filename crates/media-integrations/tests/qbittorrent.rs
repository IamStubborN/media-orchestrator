use std::{path::Path, time::Duration};

use media_integrations::qbittorrent::{
    ExplicitTorrentSelection, QbittorrentClient, QbittorrentConfig, TorrentState,
};
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

fn config(server: &MockServer) -> QbittorrentConfig {
    QbittorrentConfig::new(
        Url::parse(&server.uri()).unwrap(),
        "media-tv",
        "media-user",
        SecretString::from("qbittorrent-secret"),
        Duration::from_secs(2),
    )
    .unwrap()
}

async fn mount_login(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .and(body_string_contains("username=media-user"))
        .and(body_string_contains("password=qbittorrent-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Set-Cookie", "SID=session-cookie; path=/; HttpOnly")
                .set_body_string("Ok."),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn subnet_whitelist_does_not_require_a_login_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.0.4"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(body_string_contains("media-tv"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:indexer-guid-a",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    client.submit_selected(selection).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() != "/api/v2/auth/login")
    );
    assert!(
        requests
            .iter()
            .all(|request| !request.headers.contains_key("cookie"))
    );
}

#[tokio::test]
async fn only_explicit_selection_is_submitted_to_the_configured_category() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(header("cookie", "SID=session-cookie"))
        .and(body_string_contains("name=\"urls\""))
        .and(body_string_contains(
            "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ))
        .and(body_string_contains("name=\"category\""))
        .and(body_string_contains("media-tv"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:indexer-guid-a",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let handle = client.submit_selected(selection).await.unwrap();

    assert_eq!(handle.source_identity, "prowlarr:3:indexer-guid-a");
    assert_eq!(handle.hash, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(handle.category, "media-tv");
    let requests = server.received_requests().await.unwrap();
    let add_request = requests
        .iter()
        .find(|request| request.url.path() == "/api/v2/torrents/add")
        .unwrap();
    let add_body = String::from_utf8_lossy(&add_request.body);
    for forbidden in [
        "savepath",
        "rename",
        "paused",
        "autoTMM",
        "ratioLimit",
        "seedingTimeLimit",
        "sequentialDownload",
        "firstLastPiecePrio",
        "skip_checking",
    ] {
        assert!(
            !add_body.contains(forbidden),
            "forbidden add field: {forbidden}"
        );
    }
}

#[tokio::test]
async fn explicit_selection_is_submitted_to_the_requested_existing_category() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/categories"))
        .and(header("cookie", "SID=session-cookie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "media-tv": {"name": "media-tv", "savePath": "/downloads/media-tv"},
            "media-movies": {"name": "media-movies", "savePath": "/downloads/media-movies"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(header("cookie", "SID=session-cookie"))
        .and(body_string_contains("name=\"category\""))
        .and(body_string_contains("media-movies"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:movie-guid",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let handle = client
        .submit_selected_to_category(selection, "media-movies")
        .await
        .unwrap();

    assert_eq!(handle.category, "media-movies");
}

#[tokio::test]
async fn selection_is_not_submitted_to_an_unknown_category() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "media-tv": {"name": "media-tv", "savePath": "/downloads/media-tv"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:movie-guid",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let error = client
        .submit_selected_to_category(selection, "media-movies")
        .await
        .unwrap_err();

    assert_eq!(
        error.code(),
        media_integrations::qbittorrent::QbittorrentErrorCode::InvalidSelection
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.url.path() != "/api/v2/torrents/add")
    );
}

#[tokio::test]
async fn monitoring_and_path_discovery_are_read_only() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .and(header("cookie", "SID=session-cookie"))
        .and(query_param(
            "hashes",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ))
        .and(query_param("category", "media-tv"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "name": "Example Show",
                "category": "media-tv",
                "state": "uploading",
                "progress": 1.0,
                "amount_left": 0,
                "content_path": "/downloads/media-tv/Example Show",
                "save_path": "/downloads/media-tv/",
                "completion_on": 1770000000
            }])),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/files"))
        .and(header("cookie", "SID=session-cookie"))
        .and(query_param("hash", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"index": 0, "name": "Example Show/episode.mkv", "size": 1234, "progress": 1.0, "priority": 1},
            {"index": 1, "name": "Example Show/episode.en.srt", "size": 45, "progress": 1.0, "priority": 1}
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let handle = media_integrations::qbittorrent::TorrentHandle {
        source_identity: "prowlarr:3:indexer-guid-a".into(),
        hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        category: "media-tv".into(),
    };
    let snapshot = client.monitor(&handle).await.unwrap();
    assert_eq!(snapshot.state, TorrentState::Complete);
    assert_eq!(snapshot.progress, 1.0);
    assert_eq!(
        snapshot.content_path,
        Path::new("/downloads/media-tv/Example Show")
    );
    assert_eq!(snapshot.save_path, Path::new("/downloads/media-tv/"));

    let content = client.discover_content(&handle).await.unwrap();
    assert_eq!(content.root, Path::new("/downloads/media-tv/Example Show"));
    assert_eq!(
        content.files[0],
        Path::new("/downloads/media-tv/Example Show/episode.mkv")
    );
    assert_eq!(
        content.files[1],
        Path::new("/downloads/media-tv/Example Show/episode.en.srt")
    );

    let requests = server.received_requests().await.unwrap();
    let allowed = [
        "/api/v2/app/version",
        "/api/v2/auth/login",
        "/api/v2/torrents/info",
        "/api/v2/torrents/files",
    ];
    assert!(
        requests
            .iter()
            .all(|request| allowed.contains(&request.url.path()))
    );
    assert!(
        requests
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .all(|request| { request.url.path() == "/api/v2/auth/login" })
    );
}

#[tokio::test]
async fn monitoring_uses_the_category_preserved_in_the_job_handle() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .and(query_param(
            "hashes",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ))
        .and(query_param("category", "media-movies"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "name": "Example Movie",
                "category": "media-movies",
                "state": "uploading",
                "progress": 1.0,
                "amount_left": 0,
                "content_path": "/downloads/media-movies/Example Movie.mkv",
                "save_path": "/downloads/media-movies/",
                "completion_on": 1770000000
            }])),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let handle = media_integrations::qbittorrent::TorrentHandle {
        source_identity: "prowlarr:3:movie-guid".into(),
        hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        category: "media-movies".into(),
    };

    let snapshot = client.monitor(&handle).await.unwrap();

    assert_eq!(snapshot.name, "Example Movie");
}

#[test]
fn secrets_and_selected_uri_are_redacted_from_debug() {
    let config = QbittorrentConfig::new(
        Url::parse("http://localhost:8080").unwrap(),
        "media-tv",
        "user",
        SecretString::from("never-print-password"),
        Duration::from_secs(1),
    )
    .unwrap();
    let selection = ExplicitTorrentSelection::new(
        "source-id",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "https://tracker.invalid/download?apikey=never-print-api-key",
    )
    .unwrap();
    let debug = format!("{config:?} {selection:?}");
    assert!(!debug.contains("never-print-password"));
    assert!(!debug.contains("never-print-api-key"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn magnet_identity_must_match_the_preserved_info_hash() {
    let error = ExplicitTorrentSelection::new(
        "source-id",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "magnet:?xt=urn:btih:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
    )
    .unwrap_err();
    assert_eq!(
        error.code(),
        media_integrations::qbittorrent::QbittorrentErrorCode::IdentityMismatch
    );
}
