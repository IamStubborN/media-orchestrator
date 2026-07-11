use std::time::Duration;

use media_integrations::gluetun::{GluetunClient, GluetunConfig, GluetunErrorCode};
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

fn config(server: &MockServer) -> GluetunConfig {
    GluetunConfig::new(
        Url::parse(&server.uri()).unwrap(),
        SecretString::from("gluetun-api-key"),
        Duration::from_secs(2),
    )
    .unwrap()
}

#[tokio::test]
async fn rotation_is_rejected_without_http_while_a_sticky_job_is_active() {
    let server = MockServer::start().await;
    let client = GluetunClient::new(config(&server)).unwrap();
    let lease = client.begin_job("job-1").await.unwrap();

    let error = client.rotate_between_jobs().await.unwrap_err();
    assert_eq!(error.code(), GluetunErrorCode::StickyJobActive);
    assert!(server.received_requests().await.unwrap().is_empty());

    client.end_job(lease).await.unwrap();
}

#[tokio::test]
async fn rotation_between_jobs_uses_only_vpn_control_and_observation_routes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/publicip/ip"))
        .and(header("x-api-key", "gluetun-api-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "public_ip": "203.0.113.10"
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/vpn/status"))
        .and(header("x-api-key", "gluetun-api-key"))
        .and(body_json(serde_json::json!({"status": "stopped"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "stopped"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/vpn/status"))
        .and(header("x-api-key", "gluetun-api-key"))
        .and(body_json(serde_json::json!({"status": "running"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "running"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/vpn/status"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "running"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/publicip/ip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "public_ip": "203.0.113.11"
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let client = GluetunClient::new(config(&server)).unwrap();
    let rotation = client.rotate_between_jobs().await.unwrap();
    assert_eq!(rotation.previous_public_ip.as_deref(), Some("203.0.113.10"));
    assert_eq!(rotation.current_public_ip.as_deref(), Some("203.0.113.11"));

    let requests = server.received_requests().await.unwrap();
    let sequence = requests
        .iter()
        .map(|request| (request.method.as_str(), request.url.path()))
        .collect::<Vec<_>>();
    assert_eq!(
        sequence,
        [
            ("GET", "/v1/publicip/ip"),
            ("PUT", "/v1/vpn/status"),
            ("PUT", "/v1/vpn/status"),
            ("GET", "/v1/vpn/status"),
            ("GET", "/v1/publicip/ip"),
        ]
    );
}

#[test]
fn api_key_is_redacted_from_config_debug() {
    let config = GluetunConfig::new(
        Url::parse("http://localhost:8000").unwrap(),
        SecretString::from("never-print-key"),
        Duration::from_secs(1),
    )
    .unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("never-print-key"));
    assert!(debug.contains("[REDACTED]"));
}
