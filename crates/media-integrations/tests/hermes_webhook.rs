use media_core::{
    NotificationDelivery, NotificationEventType, NotificationId, NotificationRecipient,
};
use media_integrations::hermes::{HermesWebhookClient, HermesWebhookConfig, WebhookError};
use secrecy::SecretString;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

fn delivery() -> NotificationDelivery {
    NotificationDelivery::rehydrate(
        NotificationId::from_uuid(
            uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000123").unwrap(),
        ),
        NotificationRecipient::Primary,
        NotificationEventType::Started,
        "Media job 00000000-0000-0000-0000-000000000123 started.".to_owned(),
        0,
    )
    .unwrap()
}

fn config(server: &MockServer) -> HermesWebhookConfig {
    HermesWebhookConfig::new(
        format!("{}/webhooks/media-notify", server.uri())
            .parse()
            .unwrap(),
        format!("{}/webhooks/media-notify", server.uri())
            .parse()
            .unwrap(),
        SecretString::from("test-webhook-secret"),
        SecretString::from("other-webhook-secret"),
    )
}

#[tokio::test]
async fn posts_exact_deliver_only_payload_with_generic_hmac_v2_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/webhooks/media-notify"))
        .and(header("content-type", "application/json"))
        .and(header("x-webhook-timestamp", "1720785600"))
        .and(header(
            "x-webhook-signature-v2",
            "49731e3f577c64593519baf26df584814ec45092839957081c1377349a97a677",
        ))
        .and(header(
            "x-request-id",
            "00000000-0000-0000-0000-000000000123",
        ))
        .and(body_json(serde_json::json!({
            "event_type": "media.notification",
            "message": "Media job 00000000-0000-0000-0000-000000000123 started."
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "delivered"
        })))
        .expect(1)
        .mount(&server)
        .await;

    HermesWebhookClient::new(config(&server))
        .unwrap()
        .deliver_at(&delivery(), 1_720_785_600)
        .await
        .unwrap();
}

#[tokio::test]
async fn non_success_is_retryable_and_error_output_contains_no_endpoint_or_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/webhooks/media-notify"))
        .respond_with(ResponseTemplate::new(502).set_body_string("sensitive upstream detail"))
        .mount(&server)
        .await;
    let client = HermesWebhookClient::new(config(&server)).unwrap();

    let error = client
        .deliver_at(&delivery(), 1_720_785_600)
        .await
        .unwrap_err();

    assert_eq!(error, WebhookError::RetryableHttp);
    let rendered = format!("{client:?} {error:?}");
    assert!(!rendered.contains(&server.uri()));
    assert!(!rendered.contains("test-webhook-secret"));
    assert!(!rendered.contains("sensitive upstream detail"));
}

async fn delivery_error_for_status(status: u16) -> WebhookError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/webhooks/media-notify"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server)
        .await;
    HermesWebhookClient::new(config(&server))
        .unwrap()
        .deliver_at(&delivery(), 1_720_785_600)
        .await
        .unwrap_err()
}

#[tokio::test]
async fn client_errors_are_terminal_except_throttling_and_request_timeout() {
    for status in [400, 401, 403, 404, 422] {
        assert_eq!(
            delivery_error_for_status(status).await,
            WebhookError::TerminalHttp,
            "{status} must not be retried forever"
        );
    }
    for status in [408, 429, 500, 502, 503] {
        assert_eq!(
            delivery_error_for_status(status).await,
            WebhookError::RetryableHttp,
            "{status} must stay retryable"
        );
    }
}
