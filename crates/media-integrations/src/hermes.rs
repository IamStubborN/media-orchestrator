use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use media_contract::HermesDeliverOnlyWebhook;
use media_core::{
    NotificationDelivery, NotificationDeliveryFailure, NotificationRecipient, NotificationSink,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;

const REQUEST_ID_HEADER: &str = "x-request-id";
const TIMESTAMP_HEADER: &str = "x-webhook-timestamp";
const SIGNATURE_HEADER: &str = "x-webhook-signature-v2";

pub struct HermesWebhookConfig {
    primary_endpoint: url::Url,
    secondary_endpoint: url::Url,
    primary_secret: SecretString,
    secondary_secret: SecretString,
}

impl HermesWebhookConfig {
    #[must_use]
    pub fn new(
        primary_endpoint: url::Url,
        secondary_endpoint: url::Url,
        primary_secret: SecretString,
        secondary_secret: SecretString,
    ) -> Self {
        Self {
            primary_endpoint,
            secondary_endpoint,
            primary_secret,
            secondary_secret,
        }
    }
}

impl std::fmt::Debug for HermesWebhookConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HermesWebhookConfig { endpoints: [REDACTED], secrets: [REDACTED] }")
    }
}

pub struct HermesWebhookClient {
    client: reqwest::Client,
    config: HermesWebhookConfig,
}

impl HermesWebhookClient {
    pub fn new(config: HermesWebhookConfig) -> Result<Self, WebhookError> {
        validate_endpoint(&config.primary_endpoint)?;
        validate_endpoint(&config.secondary_endpoint)?;
        if config.primary_secret.expose_secret().is_empty()
            || config.secondary_secret.expose_secret().is_empty()
        {
            return Err(WebhookError::Configuration);
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| WebhookError::Configuration)?;
        Ok(Self { client, config })
    }

    pub async fn deliver(&self, delivery: &NotificationDelivery) -> Result<(), WebhookError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| WebhookError::Clock)?
            .as_secs();
        self.deliver_at(delivery, timestamp).await
    }

    pub async fn deliver_at(
        &self,
        delivery: &NotificationDelivery,
        timestamp: u64,
    ) -> Result<(), WebhookError> {
        let body = serde_json::to_vec(&HermesDeliverOnlyWebhook {
            event_type: "media.notification".to_owned(),
            message: delivery.message().to_owned(),
        })
        .map_err(|_| WebhookError::Serialization)?;
        let (endpoint, secret) = self.route(delivery.recipient());
        let timestamp = timestamp.to_string();
        let signature = signature(secret.expose_secret().as_bytes(), &timestamp, &body)?;
        let response = self
            .client
            .post(endpoint.clone())
            .header(TIMESTAMP_HEADER, &timestamp)
            .header(SIGNATURE_HEADER, signature)
            .header(REQUEST_ID_HEADER, delivery.id().to_string())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(classify_request_error)?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(classify_status_error(status))
        }
    }

    fn route(&self, recipient: NotificationRecipient) -> (&url::Url, &SecretString) {
        match recipient {
            NotificationRecipient::Primary => {
                (&self.config.primary_endpoint, &self.config.primary_secret)
            }
            NotificationRecipient::Secondary => (
                &self.config.secondary_endpoint,
                &self.config.secondary_secret,
            ),
        }
    }
}

impl std::fmt::Debug for HermesWebhookClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HermesWebhookClient { client: [REDACTED], config: [REDACTED] }")
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum WebhookError {
    #[error("webhook configuration is invalid")]
    Configuration,
    #[error("system clock is invalid")]
    Clock,
    #[error("webhook payload serialization failed")]
    Serialization,
    #[error("webhook request timed out")]
    Timeout,
    #[error("webhook endpoint is unavailable")]
    Connect,
    #[error("webhook request failed")]
    Request,
    #[error("webhook endpoint rejected delivery")]
    RetryableHttp,
    #[error("webhook endpoint permanently rejected delivery")]
    TerminalHttp,
}

fn validate_endpoint(endpoint: &url::Url) -> Result<(), WebhookError> {
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/webhooks/media-notify"
    {
        return Err(WebhookError::Configuration);
    }
    Ok(())
}

fn signature(secret: &[u8], timestamp: &str, body: &[u8]) -> Result<String, WebhookError> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(|_| WebhookError::Configuration)?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn classify_request_error(error: reqwest::Error) -> WebhookError {
    if error.is_timeout() {
        WebhookError::Timeout
    } else if error.is_connect() {
        WebhookError::Connect
    } else {
        WebhookError::Request
    }
}

/// Classifies a non-success HTTP response. A 4xx other than 408 and 429 signals
/// a request the endpoint will never accept on replay (a signature, auth, or
/// payload rejection), so it is terminal; everything else stays retryable.
fn classify_status_error(status: reqwest::StatusCode) -> WebhookError {
    if status.is_client_error()
        && status != reqwest::StatusCode::REQUEST_TIMEOUT
        && status != reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        WebhookError::TerminalHttp
    } else {
        WebhookError::RetryableHttp
    }
}

#[async_trait::async_trait]
impl NotificationSink for HermesWebhookClient {
    async fn deliver(
        &self,
        delivery: &NotificationDelivery,
    ) -> Result<(), NotificationDeliveryFailure> {
        HermesWebhookClient::deliver(self, delivery)
            .await
            .map_err(|error| match error {
                WebhookError::Timeout => NotificationDeliveryFailure::retryable("webhook_timeout"),
                WebhookError::Connect => NotificationDeliveryFailure::retryable("webhook_connect"),
                WebhookError::RetryableHttp => {
                    NotificationDeliveryFailure::retryable("webhook_http")
                }
                WebhookError::Request => NotificationDeliveryFailure::retryable("webhook_request"),
                WebhookError::Clock => NotificationDeliveryFailure::retryable("webhook_clock"),
                WebhookError::TerminalHttp => {
                    NotificationDeliveryFailure::terminal("webhook_rejected")
                }
                WebhookError::Configuration | WebhookError::Serialization => {
                    NotificationDeliveryFailure::terminal("webhook_internal")
                }
            })
    }
}
