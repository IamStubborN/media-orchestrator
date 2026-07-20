use std::sync::Arc;

use crate::{NotificationId, PortError};

const MAX_NOTIFICATION_DISPLAY_BYTES: usize = 256;
const MAX_NOTIFICATION_CARD_KEY_BYTES: usize = 96;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum NotificationContent {
    /// Text payloads are retained only for outbox rows created before migration 24.
    LegacyMessage(String),
    Media(Box<MediaNotification>),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationDeliveryKind {
    Card,
    FinalPush,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationState {
    Queued,
    Downloading,
    Processing,
    Publishing,
    Completed,
    Partial,
    Failed,
    Cancelled,
    NeedsAction,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationKind {
    Movie,
    Series,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationStage {
    Download,
    Process,
    Publish,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationNextStep {
    Download,
    Process,
    Publish,
    None,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationAction {
    Cancel,
    Details,
    Retry,
    RetryMissing,
    ResumeStorage,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationMedia {
    job_id: crate::JobId,
    title: String,
    kind: MediaNotificationKind,
    provider: String,
    season: Option<u32>,
    translation: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationProgress {
    completed_episodes: Option<u32>,
    total_episodes: Option<u32>,
    current_episode: Option<u32>,
    missing_episodes: Vec<MediaNotificationEpisode>,
    downloaded_bytes: Option<u64>,
    download_speed_bps: Option<u64>,
    percentage: Option<u8>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MediaNotificationEpisode {
    season: u32,
    episode: u32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationIssue {
    code: String,
    message: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotification {
    delivery_kind: MediaNotificationDeliveryKind,
    card_key: String,
    revision: u64,
    lifecycle_cycle: u64,
    terminal: bool,
    state: MediaNotificationState,
    media: MediaNotificationMedia,
    progress: Option<MediaNotificationProgress>,
    stage: Option<MediaNotificationStage>,
    next_step: Option<MediaNotificationNextStep>,
    issue: Option<MediaNotificationIssue>,
    actions: Vec<MediaNotificationAction>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum NotificationRecipient {
    Primary,
    Secondary,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum NotificationEventType {
    Started,
    ChoiceNeeded,
    DownloadingStarted,
    DownloadProgress,
    Downloaded,
    TranscodingStarted,
    EncodingComplete,
    PlexAdded,
    Completed,
    SessionRefreshed,
    Partial,
    BlockedStorage,
    Failed,
    Cancelled,
    FutureEpisodeFound,
}

impl NotificationEventType {
    /// Parses the persisted wire tag for a notification event type, returning
    /// `None` for an unknown tag. The tags are the stable strings written to the
    /// notification outbox `event_type` column.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "started" => Self::Started,
            "choice-needed" => Self::ChoiceNeeded,
            "downloading-started" => Self::DownloadingStarted,
            "download-progress" => Self::DownloadProgress,
            "downloaded" => Self::Downloaded,
            "transcoding-started" => Self::TranscodingStarted,
            "encoding-complete" => Self::EncodingComplete,
            "plex-added" => Self::PlexAdded,
            "completed" => Self::Completed,
            "session-refreshed" => Self::SessionRefreshed,
            "partial" => Self::Partial,
            "blocked-storage" => Self::BlockedStorage,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "future-episode-found" => Self::FutureEpisodeFound,
            _ => return None,
        })
    }

    /// Whether this event is an intermediate progress milestone ("downloading
    /// started", "transcoding started"). Progress milestones are noise for a
    /// co-owner who did not initiate the job, so recipient selection routes them
    /// to the initiator only, even under a `Family` notify scope. Terminal and
    /// action-required events keep the job's configured scope routing.
    #[must_use]
    pub const fn is_progress_milestone(self) -> bool {
        matches!(
            self,
            Self::DownloadingStarted | Self::DownloadProgress | Self::TranscodingStarted
        )
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NotificationDelivery {
    id: NotificationId,
    recipient: NotificationRecipient,
    event_type: NotificationEventType,
    status_key: Option<String>,
    message: String,
    content: NotificationContent,
    generation: u64,
    attempt_count: u32,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum NotificationValidationError {
    #[error("notification message cannot be empty")]
    EmptyMessage,
    #[error("notification message cannot contain a URL")]
    UrlNotAllowed,
    #[error("notification status key is invalid")]
    InvalidStatusKey,
    #[error("notification generation is invalid")]
    InvalidGeneration,
    #[error("notification display field is invalid")]
    InvalidDisplayField,
    #[error("notification provider is invalid")]
    InvalidProvider,
    #[error("notification card key is invalid")]
    InvalidCardKey,
    #[error("notification revision or lifecycle cycle is invalid")]
    InvalidRevisionOrCycle,
    #[error("notification episode progress is invalid")]
    InvalidEpisodeProgress,
    #[error("notification percentage is invalid")]
    InvalidPercentage,
}

impl MediaNotificationMedia {
    pub fn new(
        job_id: crate::JobId,
        title: String,
        kind: MediaNotificationKind,
        provider: String,
        season: Option<u32>,
        translation: Option<String>,
    ) -> Result<Self, NotificationValidationError> {
        validate_display_field(&title)?;
        if !matches!(provider.as_str(), "rezka" | "prowlarr") {
            return Err(NotificationValidationError::InvalidProvider);
        }
        if season == Some(0) {
            return Err(NotificationValidationError::InvalidEpisodeProgress);
        }
        if let Some(translation) = &translation {
            validate_display_field(translation)?;
        }
        Ok(Self {
            job_id,
            title,
            kind,
            provider,
            season,
            translation,
        })
    }

    #[must_use]
    pub const fn job_id(&self) -> crate::JobId {
        self.job_id
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub const fn kind(&self) -> MediaNotificationKind {
        self.kind
    }
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }
    #[must_use]
    pub const fn season(&self) -> Option<u32> {
        self.season
    }
    #[must_use]
    pub fn translation(&self) -> Option<&str> {
        self.translation.as_deref()
    }
}

impl MediaNotificationProgress {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        completed_episodes: Option<u32>,
        total_episodes: Option<u32>,
        current_episode: Option<u32>,
        missing_episodes: Vec<MediaNotificationEpisode>,
        downloaded_bytes: Option<u64>,
        download_speed_bps: Option<u64>,
        percentage: Option<u8>,
    ) -> Result<Self, NotificationValidationError> {
        if total_episodes == Some(0)
            || completed_episodes
                .is_some_and(|completed| total_episodes.is_none_or(|total| completed > total))
            || current_episode.is_some_and(|current| {
                total_episodes.is_none_or(|total| current == 0 || current > total)
            })
            || (completed_episodes.is_some() && total_episodes.is_none())
        {
            return Err(NotificationValidationError::InvalidEpisodeProgress);
        }
        if percentage.is_some_and(|value| value > 100) {
            return Err(NotificationValidationError::InvalidPercentage);
        }
        Ok(Self {
            completed_episodes,
            total_episodes,
            current_episode,
            missing_episodes,
            downloaded_bytes,
            download_speed_bps,
            percentage,
        })
    }

    #[must_use]
    pub const fn completed_episodes(&self) -> Option<u32> {
        self.completed_episodes
    }
    #[must_use]
    pub const fn total_episodes(&self) -> Option<u32> {
        self.total_episodes
    }
    #[must_use]
    pub const fn current_episode(&self) -> Option<u32> {
        self.current_episode
    }
    #[must_use]
    pub fn missing_episodes(&self) -> &[MediaNotificationEpisode] {
        &self.missing_episodes
    }
    #[must_use]
    pub const fn downloaded_bytes(&self) -> Option<u64> {
        self.downloaded_bytes
    }
    #[must_use]
    pub const fn download_speed_bps(&self) -> Option<u64> {
        self.download_speed_bps
    }
    #[must_use]
    pub const fn percentage(&self) -> Option<u8> {
        self.percentage
    }
}

impl MediaNotificationEpisode {
    pub const fn new(season: u32, episode: u32) -> Result<Self, NotificationValidationError> {
        if season == 0 || episode == 0 {
            return Err(NotificationValidationError::InvalidEpisodeProgress);
        }
        Ok(Self { season, episode })
    }

    #[must_use]
    pub const fn season(self) -> u32 {
        self.season
    }

    #[must_use]
    pub const fn episode(self) -> u32 {
        self.episode
    }
}

impl MediaNotificationIssue {
    pub fn new(code: String, message: String) -> Result<Self, NotificationValidationError> {
        validate_display_field(&code)?;
        validate_display_field(&message)?;
        Ok(Self { code, message })
    }
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl MediaNotification {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        delivery_kind: MediaNotificationDeliveryKind,
        card_key: String,
        revision: u64,
        lifecycle_cycle: u64,
        terminal: bool,
        state: MediaNotificationState,
        media: MediaNotificationMedia,
        progress: Option<MediaNotificationProgress>,
        stage: Option<MediaNotificationStage>,
        next_step: Option<MediaNotificationNextStep>,
        issue: Option<MediaNotificationIssue>,
        actions: Vec<MediaNotificationAction>,
    ) -> Result<Self, NotificationValidationError> {
        if !valid_card_key(&card_key) {
            return Err(NotificationValidationError::InvalidCardKey);
        }
        if revision == 0
            || lifecycle_cycle == 0
            || revision > i64::MAX as u64
            || lifecycle_cycle > i64::MAX as u64
        {
            return Err(NotificationValidationError::InvalidRevisionOrCycle);
        }
        Ok(Self {
            delivery_kind,
            card_key,
            revision,
            lifecycle_cycle,
            terminal,
            state,
            media,
            progress,
            stage,
            next_step,
            issue,
            actions,
        })
    }
    #[must_use]
    pub const fn delivery_kind(&self) -> MediaNotificationDeliveryKind {
        self.delivery_kind
    }
    #[must_use]
    pub fn card_key(&self) -> &str {
        &self.card_key
    }
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    #[must_use]
    pub const fn lifecycle_cycle(&self) -> u64 {
        self.lifecycle_cycle
    }
    #[must_use]
    pub const fn terminal(&self) -> bool {
        self.terminal
    }
    #[must_use]
    pub const fn state(&self) -> MediaNotificationState {
        self.state
    }
    #[must_use]
    pub fn media(&self) -> &MediaNotificationMedia {
        &self.media
    }
    #[must_use]
    pub fn progress(&self) -> Option<&MediaNotificationProgress> {
        self.progress.as_ref()
    }
    #[must_use]
    pub const fn stage(&self) -> Option<MediaNotificationStage> {
        self.stage
    }
    #[must_use]
    pub const fn next_step(&self) -> Option<MediaNotificationNextStep> {
        self.next_step
    }
    #[must_use]
    pub fn issue(&self) -> Option<&MediaNotificationIssue> {
        self.issue.as_ref()
    }
    #[must_use]
    pub fn actions(&self) -> &[MediaNotificationAction] {
        &self.actions
    }
}

fn validate_display_field(value: &str) -> Result<(), NotificationValidationError> {
    if value.trim().is_empty()
        || value.len() > MAX_NOTIFICATION_DISPLAY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(NotificationValidationError::InvalidDisplayField);
    }
    Ok(())
}

fn valid_card_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NOTIFICATION_CARD_KEY_BYTES
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, ':' | '-'))
}

#[cfg(test)]
mod tests {
    use super::{
        MediaNotification, MediaNotificationDeliveryKind, MediaNotificationEpisode,
        MediaNotificationKind, MediaNotificationMedia, MediaNotificationState,
        NotificationValidationError,
    };
    use crate::JobId;

    fn media() -> MediaNotificationMedia {
        MediaNotificationMedia::new(
            JobId::new(),
            "Example Show".to_owned(),
            MediaNotificationKind::Series,
            "rezka".to_owned(),
            Some(1),
            None,
        )
        .unwrap()
    }

    #[test]
    fn missing_episode_coordinates_must_be_positive() {
        for coordinates in [(0, 1), (1, 0)] {
            assert_eq!(
                MediaNotificationEpisode::new(coordinates.0, coordinates.1),
                Err(NotificationValidationError::InvalidEpisodeProgress),
            );
        }
    }

    #[test]
    fn revision_and_lifecycle_cycle_must_fit_postgres_bigint() {
        for (revision, lifecycle_cycle) in [
            (0, 1),
            (1, 0),
            (i64::MAX as u64 + 1, 1),
            (1, i64::MAX as u64 + 1),
        ] {
            assert_eq!(
                MediaNotification::new(
                    MediaNotificationDeliveryKind::Card,
                    "media-job:example".to_owned(),
                    revision,
                    lifecycle_cycle,
                    false,
                    MediaNotificationState::Downloading,
                    media(),
                    None,
                    None,
                    None,
                    None,
                    vec![],
                ),
                Err(NotificationValidationError::InvalidRevisionOrCycle),
            );
        }
    }

    #[test]
    fn provider_is_limited_to_known_safe_identifiers() {
        for provider in [
            "https://rezka.example/video?token=secret",
            "rezka/path",
            "secret-token",
            "Rezka",
        ] {
            assert_eq!(
                MediaNotificationMedia::new(
                    JobId::new(),
                    "Example Show".to_owned(),
                    MediaNotificationKind::Series,
                    provider.to_owned(),
                    Some(1),
                    None,
                ),
                Err(NotificationValidationError::InvalidProvider),
            );
        }

        for provider in ["rezka", "prowlarr"] {
            assert!(
                MediaNotificationMedia::new(
                    JobId::new(),
                    "Example Show".to_owned(),
                    MediaNotificationKind::Series,
                    provider.to_owned(),
                    Some(1),
                    None,
                )
                .is_ok()
            );
        }
    }
}

impl NotificationDelivery {
    pub fn rehydrate(
        id: NotificationId,
        recipient: NotificationRecipient,
        event_type: NotificationEventType,
        status_key: Option<String>,
        message: String,
        generation: u64,
        attempt_count: u32,
    ) -> Result<Self, NotificationValidationError> {
        if message.trim().is_empty() {
            return Err(NotificationValidationError::EmptyMessage);
        }
        if message.contains("://") {
            return Err(NotificationValidationError::UrlNotAllowed);
        }
        if status_key.as_ref().is_some_and(|key| {
            key.is_empty()
                || key.len() > 96
                || !key.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, ':' | '-')
                })
        }) {
            return Err(NotificationValidationError::InvalidStatusKey);
        }
        if generation == 0 || generation > i64::MAX as u64 {
            return Err(NotificationValidationError::InvalidGeneration);
        }
        Ok(Self {
            id,
            recipient,
            event_type,
            status_key,
            content: NotificationContent::LegacyMessage(message.clone()),
            message,
            generation,
            attempt_count,
        })
    }

    #[must_use]
    pub const fn id(&self) -> NotificationId {
        self.id
    }
    #[must_use]
    pub const fn recipient(&self) -> NotificationRecipient {
        self.recipient
    }
    #[must_use]
    pub const fn event_type(&self) -> NotificationEventType {
        self.event_type
    }
    #[must_use]
    pub fn status_key(&self) -> Option<&str> {
        self.status_key.as_deref()
    }
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
    #[must_use]
    pub const fn content(&self) -> &NotificationContent {
        &self.content
    }
    #[must_use]
    pub fn card_key(&self) -> Option<&str> {
        match &self.content {
            NotificationContent::LegacyMessage(_) => self.status_key(),
            NotificationContent::Media(notification) => Some(notification.card_key()),
        }
    }
    #[must_use]
    pub fn lifecycle_cycle(&self) -> Option<u64> {
        match &self.content {
            NotificationContent::LegacyMessage(_) => None,
            NotificationContent::Media(notification) => Some(notification.lifecycle_cycle()),
        }
    }
    pub fn rehydrate_media(
        id: NotificationId,
        recipient: NotificationRecipient,
        event_type: NotificationEventType,
        notification: MediaNotification,
        generation: u64,
        attempt_count: u32,
    ) -> Result<Self, NotificationValidationError> {
        if generation == 0 || generation > i64::MAX as u64 {
            return Err(NotificationValidationError::InvalidGeneration);
        }
        Ok(Self {
            id,
            recipient,
            event_type,
            status_key: Some(notification.card_key().to_owned()),
            message: String::new(),
            content: NotificationContent::Media(Box::new(notification)),
            generation,
            attempt_count,
        })
    }
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    #[must_use]
    pub const fn attempt_count(&self) -> u32 {
        self.attempt_count
    }
}

/// The outcome of a failed delivery attempt. A retryable failure is rescheduled
/// with backoff; a terminal failure (for example an authentication or signature
/// rejection that will never succeed on replay) is moved to a dead state so the
/// outbox stops re-leasing it.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct NotificationDeliveryFailure {
    code: &'static str,
    retryable: bool,
}

impl NotificationDeliveryFailure {
    #[must_use]
    pub const fn retryable(code: &'static str) -> Self {
        Self {
            code,
            retryable: true,
        }
    }

    #[must_use]
    pub const fn terminal(code: &'static str) -> Self {
        Self {
            code,
            retryable: false,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.retryable
    }
}

#[async_trait::async_trait]
pub trait NotificationOutboxPort: Send + Sync {
    async fn lease_pending(
        &self,
        worker: NotificationId,
        now: time::OffsetDateTime,
        ttl: time::Duration,
        limit: u32,
    ) -> Result<Vec<NotificationDelivery>, PortError>;
    async fn mark_delivered(
        &self,
        id: NotificationId,
        worker: NotificationId,
        generation: u64,
    ) -> Result<(), PortError>;
    async fn mark_failed(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        generation: u64,
        error_code: &str,
    ) -> Result<(), PortError>;
    /// Records a terminal failure so the delivery is never leased again.
    async fn mark_dead(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        generation: u64,
        error_code: &str,
    ) -> Result<(), PortError>;
}

#[async_trait::async_trait]
pub trait NotificationSink: Send + Sync {
    async fn deliver(
        &self,
        delivery: &NotificationDelivery,
    ) -> Result<(), NotificationDeliveryFailure>;
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct NotificationDispatchResult {
    pub delivered: u32,
    pub failed: u32,
    pub dead: u32,
}

pub struct NotificationDispatcher {
    outbox: Arc<dyn NotificationOutboxPort>,
    sink: Arc<dyn NotificationSink>,
}

impl NotificationDispatcher {
    #[must_use]
    pub fn new(outbox: Arc<dyn NotificationOutboxPort>, sink: Arc<dyn NotificationSink>) -> Self {
        Self { outbox, sink }
    }

    pub async fn run_once(
        &self,
        worker: NotificationId,
        now: time::OffsetDateTime,
        limit: u32,
    ) -> Result<NotificationDispatchResult, PortError> {
        let deliveries = self
            .outbox
            .lease_pending(worker, now, time::Duration::seconds(30), limit)
            .await?;
        let mut result = NotificationDispatchResult::default();
        for delivery in deliveries {
            match self.sink.deliver(&delivery).await {
                Ok(()) => {
                    self.outbox
                        .mark_delivered(delivery.id(), worker, delivery.generation())
                        .await?;
                    result.delivered += 1;
                }
                Err(failure) if failure.is_retryable() => {
                    self.outbox
                        .mark_failed(
                            delivery.id(),
                            worker,
                            now,
                            delivery.generation(),
                            failure.code(),
                        )
                        .await?;
                    result.failed += 1;
                }
                Err(failure) => {
                    self.outbox
                        .mark_dead(
                            delivery.id(),
                            worker,
                            now,
                            delivery.generation(),
                            failure.code(),
                        )
                        .await?;
                    result.dead += 1;
                }
            }
        }
        Ok(result)
    }
}
