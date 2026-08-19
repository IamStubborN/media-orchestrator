use std::time::Duration;

use media_integrations::credential_broker::{CredentialBrokerClient, CredentialBrokerConfig};
use secrecy::{ExposeSecret as _, SecretString};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

#[tokio::test]
async fn resolves_credentials_with_the_fixed_one_time_command_contract() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/command"))
        .and(header("authorization", "Bearer broker-token"))
        .and(body_json(serde_json::json!({
            "command":"credential_resolve", "argument":"request-42"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "username":"user@example.test","password":"secret","url":"https://rezka.test/"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let config = CredentialBrokerConfig::new(
        server.uri().parse().unwrap(),
        SecretString::from("broker-token"),
        Duration::from_secs(2),
        &["127.0.0.1".to_owned()],
    )
    .unwrap();
    let credentials = CredentialBrokerClient::new(config)
        .unwrap()
        .resolve("request-42")
        .await
        .unwrap();
    assert_eq!(credentials.username, "user@example.test");
    assert_eq!(credentials.password.expose_secret(), "secret");
}

#[test]
fn rejects_public_plain_http_brokers() {
    assert!(
        CredentialBrokerConfig::new(
            "http://example.com/".parse().unwrap(),
            SecretString::from("token"),
            Duration::from_secs(2),
            &[],
        )
        .is_err()
    );
}
