use media_core::{
    JobId, MediaNotification, MediaNotificationAction, MediaNotificationDeliveryKind,
    MediaNotificationEpisode, MediaNotificationKind, MediaNotificationMedia,
    MediaNotificationNextStep, MediaNotificationProgress, MediaNotificationStage,
    MediaNotificationState, NotificationDelivery, NotificationEventType, NotificationId,
    NotificationRecipient,
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
        Some("media-job:00000000-0000-0000-0000-000000000999".to_owned()),
        "Media job 00000000-0000-0000-0000-000000000123 started.".to_owned(),
        1,
        0,
    )
    .unwrap()
}

fn media_delivery() -> NotificationDelivery {
    let job_id =
        JobId::from_uuid(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000999").unwrap());
    let notification = MediaNotification::new(
        MediaNotificationDeliveryKind::Card,
        format!("media-job:{job_id}"),
        3,
        2,
        false,
        MediaNotificationState::Downloading,
        MediaNotificationMedia::new(
            job_id,
            "Example Show".to_owned(),
            MediaNotificationKind::Series,
            "rezka".to_owned(),
            Some(1),
            Some("AniLibria".to_owned()),
        )
        .unwrap(),
        Some(
            MediaNotificationProgress::new(
                Some(7),
                Some(12),
                Some(8),
                vec![MediaNotificationEpisode::new(1, 9).unwrap()],
                Some(195_035_136),
                Some(5_452_595),
                None,
            )
            .unwrap(),
        ),
        Some(MediaNotificationStage::Download),
        Some(MediaNotificationNextStep::Process),
        None,
        vec![
            MediaNotificationAction::Cancel,
            MediaNotificationAction::Details,
        ],
    )
    .unwrap();
    NotificationDelivery::rehydrate_media(
        NotificationId::from_uuid(
            uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000124").unwrap(),
        ),
        NotificationRecipient::Primary,
        NotificationEventType::DownloadProgress,
        notification,
        7,
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
            "d6ba9e250497a0a96d8cc101e2a9b6784db9db5cdd583dab0d896d0108e4a21a",
        ))
        .and(header(
            "x-request-id",
            "00000000-0000-0000-0000-000000000123-1",
        ))
        .and(body_json(serde_json::json!({
            "event_type": "media.notification",
            "status_key": "media-job:00000000-0000-0000-0000-000000000999",
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
async fn posts_exact_schema_v2_payload_with_generation_aware_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/webhooks/media-notify"))
        .and(header("content-type", "application/json"))
        .and(header("x-webhook-timestamp", "1720785600"))
        .and(header(
            "x-request-id",
            "00000000-0000-0000-0000-000000000124-7",
        ))
        .and(body_json(serde_json::json!({
            "event_type": "media.notification",
            "schema_version": 2,
            "delivery_kind": "card",
            "card_key": "media-job:00000000-0000-0000-0000-000000000999",
            "revision": 3,
            "lifecycle_cycle": 2,
            "terminal": false,
            "state": "downloading",
            "media": {
                "job_id": "00000000-0000-0000-0000-000000000999",
                "title": "Example Show",
                "kind": "series",
                "provider": "rezka",
                "season": 1,
                "translation": "AniLibria"
            },
            "progress": {
                "completed_episodes": 7,
                "total_episodes": 12,
                "current_episode": 8,
                "missing_episodes": [{"season": 1, "episode": 9}],
                "downloaded_bytes": 195035136,
                "download_speed_bps": 5452595
            },
            "stage": "download",
            "next_step": "process",
            "actions": ["cancel", "details"]
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    HermesWebhookClient::new(config(&server))
        .unwrap()
        .deliver_at(&media_delivery(), 1_720_785_600)
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
