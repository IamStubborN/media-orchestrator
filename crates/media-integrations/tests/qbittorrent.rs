use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use media_integrations::qbittorrent::{
    EpisodeFileSelection, ExplicitTorrentSelection, QbittorrentClient, QbittorrentConfig,
    QbittorrentError, TorrentState,
};
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

const TORRENT_INFO_HASH: &str = "d2939e5af6d595ecdfd4d11563f16986535b6b98";
const TORRENT_BYTES: &[u8] = b"d8:announce14:http://tracker4:infod6:lengthi5e4:name8:file.txt12:piece lengthi16384e6:pieces20:12345678901234567890ee";

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
async fn single_episode_submission_selects_only_the_episode_and_its_subtitles() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "media-tv": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(body_string_contains("stopped"))
        .and(body_string_contains("true"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/files"))
        .and(query_param("hash", TORRENT_INFO_HASH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "index": 0,
                "name": "Example Show/Example.Show.S02E06.mkv",
                "size": 1_000,
                "progress": 0.0,
                "priority": 1
            },
            {
                "index": 1,
                "name": "Example Show/Example.Show.S02E07.mkv",
                "size": 1_000,
                "progress": 0.0,
                "priority": 1
            },
            {
                "index": 2,
                "name": "Example Show/Example.Show.S02E07.ru.srt",
                "size": 10,
                "progress": 0.0,
                "priority": 1
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/filePrio"))
        .and(body_string_contains("priority=0"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/filePrio"))
        .and(body_string_contains("priority=1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/start"))
        .and(body_string_contains(TORRENT_INFO_HASH))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:indexer-guid-a",
        TORRENT_INFO_HASH,
        format!("magnet:?xt=urn:btih:{TORRENT_INFO_HASH}"),
    )
    .unwrap();

    client
        .submit_episode_to_category(
            selection,
            "media-tv",
            EpisodeFileSelection::new(2, 7).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn rejected_new_episode_torrent_is_deleted_with_partial_files() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "media-tv": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(body_string_contains("stopped"))
        .and(body_string_contains("true"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/files"))
        .and(query_param("hash", TORRENT_INFO_HASH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "index": 0,
                "name": "Unrelated.Movie.2026.1080p.mkv",
                "priority": 1
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/delete"))
        .and(body_string_contains(TORRENT_INFO_HASH))
        .and(body_string_contains("deleteFiles=true"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:indexer-guid-a",
        TORRENT_INFO_HASH,
        format!("magnet:?xt=urn:btih:{TORRENT_INFO_HASH}"),
    )
    .unwrap();

    let error = client
        .submit_episode_to_category(
            selection,
            "media-tv",
            EpisodeFileSelection::new(2, 7).unwrap(),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, QbittorrentError::InvalidSelection { .. }));
}

#[tokio::test]
async fn magnet_metadata_is_fetched_before_selecting_episode_files() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "media-tv": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(body_string_contains("stopped"))
        .and(body_string_contains("true"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    let metadata_requests = Arc::new(AtomicUsize::new(0));
    let metadata_requests_for_responder = Arc::clone(&metadata_requests);
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/files"))
        .and(query_param("hash", TORRENT_INFO_HASH))
        .respond_with(move |_: &wiremock::Request| {
            if metadata_requests_for_responder.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(409)
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {
                        "index": 0,
                        "name": "Example Show/Example.Show.S02E07.mkv",
                        "priority": 1
                    }
                ]))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/start"))
        .and(body_string_contains(TORRENT_INFO_HASH))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/stop"))
        .and(body_string_contains(TORRENT_INFO_HASH))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/filePrio"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:indexer-guid-a",
        TORRENT_INFO_HASH,
        format!("magnet:?xt=urn:btih:{TORRENT_INFO_HASH}"),
    )
    .unwrap();

    client
        .submit_episode_to_category(
            selection,
            "media-tv",
            EpisodeFileSelection::new(2, 7).unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(metadata_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn completed_season_pack_discovers_only_the_requested_episode() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "hash": TORRENT_INFO_HASH,
                "name": "Example Show S02",
                "category": "media-tv",
                "state": "uploading",
                "progress": 1.0,
                "amount_left": 0,
                "content_path": "/downloads/media-tv/Example Show S02",
                "save_path": "/downloads/media-tv/"
            }])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/files"))
        .and(query_param("hash", TORRENT_INFO_HASH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "index": 0,
                "name": "Example Show S02/Example.Show.S02E06.mkv",
                "priority": 1
            },
            {
                "index": 1,
                "name": "Example Show S02/Example.Show.S02E07.mkv",
                "priority": 1
            },
            {
                "index": 2,
                "name": "Example Show S02/Example.Show.S02E07.en.srt",
                "priority": 1
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let handle = media_integrations::qbittorrent::TorrentHandle {
        source_identity: "prowlarr:3:indexer-guid-a".into(),
        hash: TORRENT_INFO_HASH.into(),
        category: "media-tv".into(),
    };

    let content = client
        .discover_episode_content(&handle, EpisodeFileSelection::new(2, 7).unwrap())
        .await
        .unwrap();

    assert_eq!(
        content.files,
        vec![
            Path::new("/downloads/media-tv/Example Show S02/Example.Show.S02E07.mkv"),
            Path::new("/downloads/media-tv/Example Show S02/Example.Show.S02E07.en.srt"),
        ]
    );
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
async fn qbittorrent_5_2_pending_add_response_is_accepted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
            "added_torrent_ids": [],
            "failure_count": 0,
            "pending_count": 1,
            "success_count": 0
        })))
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
}

#[tokio::test]
async fn http_torrent_is_fetched_verified_and_uploaded_as_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/selected.torrent"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(TORRENT_BYTES))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(body_string_contains("selected.torrent"))
        .and(body_string_contains("media-tv"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:indexer-guid-a",
        TORRENT_INFO_HASH,
        format!("{}/selected.torrent", server.uri()),
    )
    .unwrap();

    client.submit_selected(selection).await.unwrap();
}

#[tokio::test]
async fn prowlarr_redirect_to_matching_magnet_is_submitted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/prowlarr/download"))
        .respond_with(ResponseTemplate::new(301).insert_header(
            "Location",
            format!("magnet:?xt=urn:btih:{TORRENT_INFO_HASH}"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(body_string_contains(TORRENT_INFO_HASH))
        .and(body_string_contains("media-tv"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:indexer-guid-a",
        TORRENT_INFO_HASH,
        format!("{}/prowlarr/download", server.uri()),
    )
    .unwrap();

    client.submit_selected(selection).await.unwrap();
}

#[tokio::test]
async fn already_pending_exact_magnet_conflict_is_monitored_idempotently() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(body_string_contains(TORRENT_INFO_HASH))
        .respond_with(ResponseTemplate::new(409).set_body_string("Conflict"))
        .expect(1)
        .mount(&server)
        .await;

    let client = QbittorrentClient::connect(config(&server)).await.unwrap();
    let selection = ExplicitTorrentSelection::new(
        "prowlarr:3:indexer-guid-a",
        TORRENT_INFO_HASH,
        format!("magnet:?xt=urn:btih:{TORRENT_INFO_HASH}"),
    )
    .unwrap();

    let handle = client.submit_selected(selection).await.unwrap();
    assert_eq!(handle.hash, TORRENT_INFO_HASH);
}

#[tokio::test]
async fn an_existing_exact_torrent_is_reused_without_duplicate_submission() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .and(query_param(
            "hashes",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ))
        .and(query_param("category", "media-tv"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "name": "Selected release",
                "category": "media-tv",
                "state": "downloading",
                "progress": 0.25,
                "amount_left": 300,
                "downloaded": 100,
                "completed": 100,
                "size": 400,
                "dlspeed": 50,
                "eta": 6,
                "num_seeds": 12,
                "num_leechs": 4,
                "content_path": "/downloads/media-tv/release",
                "save_path": "/downloads/media-tv",
                "completion_on": -1
            }])),
        )
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
    assert_eq!(handle.category, "media-tv");
    let snapshot = client.monitor(&handle).await.unwrap();
    assert_eq!(snapshot.downloaded_bytes, Some(100));
    assert_eq!(snapshot.total_bytes, Some(400));
    assert_eq!(snapshot.download_speed_bps, Some(50));
    assert_eq!(snapshot.eta_seconds, Some(6));
    assert_eq!(snapshot.seeds, Some(12));
    assert_eq!(snapshot.peers, Some(4));
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
                "downloaded": -1,
                "size": -1,
                "dlspeed": -1,
                "eta": 8640000,
                "num_seeds": -1,
                "num_leechs": -1,
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
    assert_eq!(snapshot.downloaded_bytes, None);
    assert_eq!(snapshot.total_bytes, None);
    assert_eq!(snapshot.download_speed_bps, None);
    assert_eq!(snapshot.eta_seconds, None);
    assert_eq!(snapshot.seeds, None);
    assert_eq!(snapshot.peers, None);
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

#[test]
fn only_bounded_startup_failures_are_transient() {
    assert!(QbittorrentError::Transport.is_transient());
    assert!(
        QbittorrentError::ProviderResponse {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        }
        .is_transient()
    );
    assert!(
        !QbittorrentError::ProviderResponse {
            status: reqwest::StatusCode::BAD_REQUEST,
        }
        .is_transient()
    );
    assert!(!QbittorrentError::Unauthorized.is_transient());
}
