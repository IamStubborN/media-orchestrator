use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use media_contract::{
    HermesDeliverOnlyWebhook, HermesMediaNotificationWebhook, HermesSourceChoiceWebhook,
    MediaNotificationActionDto, MediaNotificationAudioDto, MediaNotificationDeliveryKindDto,
    MediaNotificationDto, MediaNotificationEpisodeDto, MediaNotificationIssueDto,
    MediaNotificationKindDto, MediaNotificationLibraryDto, MediaNotificationNextStepDto,
    MediaNotificationOriginDto, MediaNotificationProcessingDto, MediaNotificationProcessingModeDto,
    MediaNotificationProgressDto, MediaNotificationPublicationDto, MediaNotificationResultDto,
    MediaNotificationStageDto, MediaNotificationStateDto, MediaNotificationSubtitlesDto,
    MediaNotificationVideoDto, PublicId, SourceChoiceActionDto,
};
use media_core::{
    MediaNotification, MediaNotificationAction, MediaNotificationDeliveryKind,
    MediaNotificationKind, MediaNotificationLibrary, MediaNotificationNextStep,
    MediaNotificationOrigin, MediaNotificationProcessingMode, MediaNotificationResult,
    MediaNotificationStage, MediaNotificationState, NotificationContent, NotificationDelivery,
    NotificationDeliveryFailure, NotificationRecipient, NotificationSink, SourceChoiceAction,
    SourceChoiceNotification,
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
        let body = match delivery.content() {
            NotificationContent::LegacyMessage(_) => {
                serde_json::to_vec(&HermesDeliverOnlyWebhook {
                    event_type: "media.notification".to_owned(),
                    status_key: delivery.status_key().map(ToOwned::to_owned),
                    message: delivery.message().to_owned(),
                })
            }
            NotificationContent::Media(notification) => {
                serde_json::to_vec(&media_webhook(notification))
            }
            NotificationContent::SourceChoice(notification) => {
                serde_json::to_vec(&source_choice_webhook(notification))
            }
        }
        .map_err(|_| WebhookError::Serialization)?;
        let (endpoint, secret) = self.route(delivery.recipient());
        let timestamp = timestamp.to_string();
        let signature = signature(secret.expose_secret().as_bytes(), &timestamp, &body)?;
        let response = self
            .client
            .post(endpoint.clone())
            .header(TIMESTAMP_HEADER, &timestamp)
            .header(SIGNATURE_HEADER, signature)
            .header(
                REQUEST_ID_HEADER,
                format!("{}-{}", delivery.id(), delivery.generation()),
            )
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

fn source_choice_webhook(notification: &SourceChoiceNotification) -> HermesSourceChoiceWebhook {
    HermesSourceChoiceWebhook {
        event_type: "media.source-choice".to_owned(),
        schema_version: 1,
        card_key: notification.card_key().to_owned(),
        tracking_id: PublicId::parse(&notification.tracking_id().to_string())
            .expect("domain tracking IDs are valid UUIDs"),
        title: notification.title().to_owned(),
        season: notification.season(),
        episode: notification.episode(),
        actions: notification
            .actions()
            .iter()
            .map(|action| match action {
                SourceChoiceAction::All => SourceChoiceActionDto::All,
                SourceChoiceAction::Rezka => SourceChoiceActionDto::Rezka,
                SourceChoiceAction::Prowlarr => SourceChoiceActionDto::Prowlarr,
            })
            .collect(),
        poster_url: notification.poster_url().map(str::to_owned),
    }
}

fn media_webhook(notification: &MediaNotification) -> HermesMediaNotificationWebhook {
    let media = notification.media();
    HermesMediaNotificationWebhook {
        event_type: "media.notification".to_owned(),
        schema_version: 2,
        delivery_kind: match notification.delivery_kind() {
            MediaNotificationDeliveryKind::Card => MediaNotificationDeliveryKindDto::Card,
            MediaNotificationDeliveryKind::FinalPush => MediaNotificationDeliveryKindDto::FinalPush,
        },
        card_key: notification.card_key().to_owned(),
        revision: notification.revision(),
        lifecycle_cycle: notification.lifecycle_cycle(),
        terminal: notification.terminal(),
        state: match notification.state() {
            MediaNotificationState::Queued => MediaNotificationStateDto::Queued,
            MediaNotificationState::Downloading => MediaNotificationStateDto::Downloading,
            MediaNotificationState::Processing => MediaNotificationStateDto::Processing,
            MediaNotificationState::Publishing => MediaNotificationStateDto::Publishing,
            MediaNotificationState::Completed => MediaNotificationStateDto::Completed,
            MediaNotificationState::Partial => MediaNotificationStateDto::Partial,
            MediaNotificationState::Failed => MediaNotificationStateDto::Failed,
            MediaNotificationState::Cancelled => MediaNotificationStateDto::Cancelled,
            MediaNotificationState::NeedsAction => MediaNotificationStateDto::NeedsAction,
        },
        media: MediaNotificationDto {
            job_id: PublicId::parse(&media.job_id().to_string())
                .expect("domain job IDs are valid UUIDs"),
            title: media.title().to_owned(),
            kind: match media.kind() {
                MediaNotificationKind::Movie => MediaNotificationKindDto::Movie,
                MediaNotificationKind::Series => MediaNotificationKindDto::Series,
            },
            provider: media.provider().to_owned(),
            season: media.season(),
            translation: media.translation().map(ToOwned::to_owned),
            origin: media.origin().map(|origin| match origin {
                MediaNotificationOrigin::TrackedEpisode => {
                    MediaNotificationOriginDto::TrackedEpisode
                }
            }),
        },
        progress: notification
            .progress()
            .map(|progress| MediaNotificationProgressDto {
                completed_episodes: progress.completed_episodes(),
                total_episodes: progress.total_episodes(),
                current_episode: progress.current_episode(),
                missing_episodes: progress
                    .missing_episodes()
                    .iter()
                    .map(|episode| MediaNotificationEpisodeDto {
                        season: episode.season(),
                        episode: episode.episode(),
                    })
                    .collect(),
                downloaded_bytes: progress.downloaded_bytes(),
                total_bytes: progress.total_bytes(),
                download_speed_bps: progress.download_speed_bps(),
                percentage: progress.percentage(),
                eta_seconds: progress.eta_seconds(),
                seeds: progress.seeds(),
                peers: progress.peers(),
                source_state: progress.source_state().map(ToOwned::to_owned),
                connection_attempt: progress.connection_attempt(),
                connection_attempt_limit: progress.connection_attempt_limit(),
                vpn_rotation_pending: progress.vpn_rotation_pending(),
                storage_available_bytes: progress.storage_available_bytes(),
                storage_required_bytes: progress.storage_required_bytes(),
            }),
        stage: notification.stage().map(|stage| match stage {
            MediaNotificationStage::Download => MediaNotificationStageDto::Download,
            MediaNotificationStage::Process => MediaNotificationStageDto::Process,
            MediaNotificationStage::Publish => MediaNotificationStageDto::Publish,
        }),
        next_step: notification.next_step().map(|step| match step {
            MediaNotificationNextStep::Download => MediaNotificationNextStepDto::Download,
            MediaNotificationNextStep::Process => MediaNotificationNextStepDto::Process,
            MediaNotificationNextStep::Publish => MediaNotificationNextStepDto::Publish,
            MediaNotificationNextStep::None => MediaNotificationNextStepDto::None,
        }),
        issue: notification.issue().map(|issue| MediaNotificationIssueDto {
            code: issue.code().to_owned(),
            message: issue.message().to_owned(),
        }),
        result: notification.result().map(media_result_dto),
        actions: notification
            .actions()
            .iter()
            .map(|action| match action {
                MediaNotificationAction::Cancel => MediaNotificationActionDto::Cancel,
                MediaNotificationAction::Details => MediaNotificationActionDto::Details,
                MediaNotificationAction::Retry => MediaNotificationActionDto::Retry,
                MediaNotificationAction::RetryMissing => MediaNotificationActionDto::RetryMissing,
                MediaNotificationAction::ResumeStorage => MediaNotificationActionDto::ResumeStorage,
                MediaNotificationAction::SearchAlternative => {
                    MediaNotificationActionDto::SearchAlternative
                }
            })
            .collect(),
    }
}

fn media_result_dto(result: &MediaNotificationResult) -> MediaNotificationResultDto {
    MediaNotificationResultDto {
        video: result.video().map(|video| MediaNotificationVideoDto {
            codec: video.codec().to_owned(),
            profile: video.profile().map(ToOwned::to_owned),
            width: video.width(),
            height: video.height(),
        }),
        audio: result.audio().map(|audio| MediaNotificationAudioDto {
            language: audio.language().map(ToOwned::to_owned),
            codec: audio.codec().to_owned(),
            channels: audio.channels(),
            channel_layout: audio.channel_layout().map(ToOwned::to_owned),
            title: audio.title().map(ToOwned::to_owned),
        }),
        subtitles: result
            .subtitles()
            .map(|subtitles| MediaNotificationSubtitlesDto {
                downloaded: subtitles.downloaded(),
                missing: subtitles.missing(),
            }),
        file_size_bytes: result.file_size_bytes(),
        duration_seconds: result.duration_seconds(),
        processing: result
            .processing()
            .map(|processing| MediaNotificationProcessingDto {
                mode: match processing.mode() {
                    MediaNotificationProcessingMode::VaapiUpscale => {
                        MediaNotificationProcessingModeDto::VaapiUpscale
                    }
                    MediaNotificationProcessingMode::Original => {
                        MediaNotificationProcessingModeDto::Original
                    }
                },
                elapsed_seconds: processing.elapsed_seconds(),
            }),
        publication: result
            .publication()
            .map(|publication| MediaNotificationPublicationDto {
                library: match publication.library() {
                    MediaNotificationLibrary::Movies => MediaNotificationLibraryDto::Movies,
                    MediaNotificationLibrary::TvShows => MediaNotificationLibraryDto::TvShows,
                },
                title: publication.title().to_owned(),
                season: publication.season(),
                episode: publication.episode(),
            }),
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

/// Maps a webhook failure onto the outbox retry taxonomy. Only deterministic
/// failures that a replay would repeat identically are terminal: a rejected
/// signature/auth/payload (`TerminalHttp`) and a payload `Serialization`
/// failure. Everything else — including a boot-time `Configuration` gap that a
/// redeploy can fix — stays retryable so a transient condition does not
/// permanently dead-letter an otherwise valid notification.
fn classify_delivery_failure(error: WebhookError) -> NotificationDeliveryFailure {
    match error {
        WebhookError::Timeout => NotificationDeliveryFailure::retryable("webhook_timeout"),
        WebhookError::Connect => NotificationDeliveryFailure::retryable("webhook_connect"),
        WebhookError::RetryableHttp => NotificationDeliveryFailure::retryable("webhook_http"),
        WebhookError::Request => NotificationDeliveryFailure::retryable("webhook_request"),
        WebhookError::Clock => NotificationDeliveryFailure::retryable("webhook_clock"),
        WebhookError::Configuration => {
            NotificationDeliveryFailure::retryable("webhook_configuration")
        }
        WebhookError::TerminalHttp => NotificationDeliveryFailure::terminal("webhook_rejected"),
        WebhookError::Serialization => {
            NotificationDeliveryFailure::terminal("webhook_serialization")
        }
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
            .map_err(classify_delivery_failure)
    }
}

#[cfg(test)]
mod tests {
    use super::{WebhookError, classify_delivery_failure};

    #[test]
    fn configuration_gap_is_retryable_but_serialization_is_terminal() {
        // A missing secret/endpoint at boot can be fixed by a redeploy, so it
        // must not permanently dead-letter the notification.
        let configuration = classify_delivery_failure(WebhookError::Configuration);
        assert!(configuration.is_retryable());
        assert_eq!(configuration.code(), "webhook_configuration");

        // A payload serialization failure is deterministic and never recovers.
        let serialization = classify_delivery_failure(WebhookError::Serialization);
        assert!(!serialization.is_retryable());
        assert_eq!(serialization.code(), "webhook_serialization");
    }

    #[test]
    fn rejected_delivery_stays_terminal_and_transient_failures_stay_retryable() {
        assert!(!classify_delivery_failure(WebhookError::TerminalHttp).is_retryable());
        for transient in [
            WebhookError::Timeout,
            WebhookError::Connect,
            WebhookError::RetryableHttp,
            WebhookError::Request,
            WebhookError::Clock,
        ] {
            assert!(
                classify_delivery_failure(transient).is_retryable(),
                "{transient:?} must stay retryable"
            );
        }
    }
}
