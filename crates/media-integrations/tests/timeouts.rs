use std::time::Duration;

use media_integrations::{
    gluetun::{GluetunClient, GluetunConfig, GluetunErrorCode},
    plex::{PlexClient, PlexConfig, PlexErrorCode, ScanRequest},
    prowlarr::{
        MediaQuery, ProwlarrClient, ProwlarrConfig, ProwlarrErrorCode, SearchPageRequest,
        SearchSession,
    },
    qbittorrent::{QbittorrentClient, QbittorrentConfig, QbittorrentErrorCode},
};
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const TIMEOUT: Duration = Duration::from_millis(20);
const DELAY: Duration = Duration::from_millis(200);

#[tokio::test]
async fn prowlarr_requests_honor_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(DELAY)
                .set_body_json(serde_json::json!([])),
        )
        .mount(&server)
        .await;
    let config = ProwlarrConfig::new(
        Url::parse(&server.uri()).unwrap(),
        SecretString::from("key"),
        TIMEOUT,
    )
    .unwrap();
    let request = SearchPageRequest::new(
        SearchSession::new("session", MediaQuery::movie("Movie")).unwrap(),
        0,
    )
    .unwrap();
    let error = ProwlarrClient::new(config)
        .unwrap()
        .search(request)
        .await
        .unwrap_err();
    assert_eq!(error.code(), ProwlarrErrorCode::Transport);
}

#[tokio::test]
async fn qbittorrent_requests_honor_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_delay(DELAY))
        .mount(&server)
        .await;
    let config = QbittorrentConfig::new(
        Url::parse(&server.uri()).unwrap(),
        "media",
        "user",
        SecretString::from("password"),
        TIMEOUT,
    )
    .unwrap();
    let error = QbittorrentClient::connect(config).await.err().unwrap();
    assert_eq!(error.code(), QbittorrentErrorCode::Transport);
}

#[tokio::test]
async fn plex_requests_honor_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/sections/1/refresh"))
        .respond_with(ResponseTemplate::new(200).set_delay(DELAY))
        .mount(&server)
        .await;
    let config = PlexConfig::new(
        Url::parse(&server.uri()).unwrap(),
        SecretString::from("token"),
        TIMEOUT,
    )
    .unwrap();
    let client = PlexClient::new(config).unwrap();
    let error = client
        .trigger_scan(&ScanRequest::new(1, "/media").unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.code(), PlexErrorCode::Transport);
}

#[tokio::test]
async fn gluetun_requests_honor_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/publicip/ip"))
        .respond_with(ResponseTemplate::new(200).set_delay(DELAY))
        .mount(&server)
        .await;
    let config = GluetunConfig::new(
        Url::parse(&server.uri()).unwrap(),
        SecretString::from("key"),
        TIMEOUT,
    )
    .unwrap();
    let error = GluetunClient::new(config)
        .unwrap()
        .rotate_between_jobs()
        .await
        .unwrap_err();
    assert_eq!(error.code(), GluetunErrorCode::Transport);
}
