use crate::{NotificationId, TrackingId};
pub use dispatch::{
    NotificationDeliveryFailure, NotificationDeliveryFence, NotificationDeliveryPermit,
    NotificationDispatchResult, NotificationDispatcher, NotificationOutboxPort, NotificationSink,
    NotificationSinkOutcome,
};
use validation::{
    valid_card_key, validate_channel_layout, validate_codec, validate_display_field,
    validate_human_label, validate_language, validate_machine_metadata, validate_profile,
};

mod delivery;
mod dispatch;
mod validation;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum NotificationContent {
    /// Text payloads are retained only for historical outbox rows.
    LegacyMessage(String),
    Media(Box<MediaNotification>),
    SourceChoice(SourceChoiceNotification),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum SourceChoiceAction {
    All,
    Rezka,
    Prowlarr,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceChoiceNotification {
    card_key: String,
    tracking_id: TrackingId,
    title: String,
    season: u32,
    episode: u32,
    actions: Vec<SourceChoiceAction>,
    poster_url: Option<String>,
    choice_set_id: Option<String>,
    choice_set_expires_at: Option<String>,
    rezka_count: Option<u32>,
    prowlarr_count: Option<u32>,
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
    SearchAlternative,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationOrigin {
    TrackedEpisode,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationMedia {
    job_id: crate::JobId,
    title: String,
    kind: MediaNotificationKind,
    provider: String,
    season: Option<u32>,
    translation: Option<String>,
    origin: Option<MediaNotificationOrigin>,
    poster_url: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationProgress {
    completed_episodes: Option<u32>,
    total_episodes: Option<u32>,
    current_episode: Option<u32>,
    missing_episodes: Vec<MediaNotificationEpisode>,
    downloaded_bytes: Option<u64>,
    total_bytes: Option<u64>,
    download_speed_bps: Option<u64>,
    percentage: Option<u8>,
    eta_seconds: Option<u64>,
    seeds: Option<u64>,
    peers: Option<u64>,
    source_state: Option<String>,
    connection_attempt: Option<u32>,
    connection_attempt_limit: Option<u32>,
    vpn_rotation_pending: Option<bool>,
    storage_available_bytes: Option<u64>,
    storage_required_bytes: Option<u64>,
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
pub struct MediaNotificationResult {
    video: Option<MediaNotificationVideo>,
    audio: Option<MediaNotificationAudio>,
    subtitles: Option<MediaNotificationSubtitles>,
    file_size_bytes: Option<u64>,
    duration_seconds: Option<u64>,
    processing: Option<MediaNotificationProcessing>,
    publication: Option<MediaNotificationPublication>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationVideo {
    codec: String,
    profile: Option<String>,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationAudio {
    language: Option<String>,
    codec: String,
    channels: Option<u32>,
    channel_layout: Option<String>,
    title: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationSubtitles {
    downloaded: u32,
    missing: u32,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationProcessingMode {
    VaapiUpscale,
    Original,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationProcessing {
    mode: MediaNotificationProcessingMode,
    elapsed_seconds: Option<u64>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaNotificationLibrary {
    Movies,
    TvShows,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaNotificationPublication {
    library: MediaNotificationLibrary,
    title: String,
    season: Option<u32>,
    episode: Option<u32>,
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
    result: Option<MediaNotificationResult>,
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
    #[error("notification media result is invalid")]
    InvalidMediaResult,
    #[error("notification connection attempt progress is invalid")]
    InvalidAttemptProgress,
    #[error("notification storage progress is invalid")]
    InvalidStorageProgress,
    #[error("source choice episode is invalid")]
    InvalidSourceChoiceEpisode,
    #[error("source choice actions are invalid")]
    InvalidSourceChoiceActions,
}

impl SourceChoiceNotification {
    pub fn new(
        card_key: String,
        tracking_id: TrackingId,
        title: String,
        season: u32,
        episode: u32,
        actions: Vec<SourceChoiceAction>,
    ) -> Result<Self, NotificationValidationError> {
        if !valid_card_key(&card_key) {
            return Err(NotificationValidationError::InvalidCardKey);
        }
        validate_human_label(&title)?;
        if episode == 0 {
            return Err(NotificationValidationError::InvalidSourceChoiceEpisode);
        }
        if !matches!(
            actions.as_slice(),
            [SourceChoiceAction::Rezka]
                | [SourceChoiceAction::Prowlarr]
                | [
                    SourceChoiceAction::All,
                    SourceChoiceAction::Rezka,
                    SourceChoiceAction::Prowlarr
                ]
        ) {
            return Err(NotificationValidationError::InvalidSourceChoiceActions);
        }
        Ok(Self {
            card_key,
            tracking_id,
            title,
            season,
            episode,
            actions,
            poster_url: None,
            choice_set_id: None,
            choice_set_expires_at: None,
            rezka_count: None,
            prowlarr_count: None,
        })
    }

    #[must_use]
    pub fn with_poster_url(mut self, poster_url: Option<String>) -> Self {
        self.poster_url = poster_url;
        self
    }

    #[must_use]
    pub fn with_choice_set(
        mut self,
        choice_set_id: String,
        expires_at: String,
        rezka_count: u32,
        prowlarr_count: u32,
    ) -> Self {
        self.choice_set_id = Some(choice_set_id);
        self.choice_set_expires_at = Some(expires_at);
        self.rezka_count = Some(rezka_count);
        self.prowlarr_count = Some(prowlarr_count);
        self
    }

    #[must_use]
    pub fn card_key(&self) -> &str {
        &self.card_key
    }

    #[must_use]
    pub const fn tracking_id(&self) -> TrackingId {
        self.tracking_id
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub const fn season(&self) -> u32 {
        self.season
    }

    #[must_use]
    pub const fn episode(&self) -> u32 {
        self.episode
    }

    #[must_use]
    pub fn actions(&self) -> &[SourceChoiceAction] {
        &self.actions
    }

    #[must_use]
    pub fn poster_url(&self) -> Option<&str> {
        self.poster_url.as_deref()
    }

    #[must_use]
    pub fn choice_set_id(&self) -> Option<&str> {
        self.choice_set_id.as_deref()
    }

    #[must_use]
    pub fn choice_set_expires_at(&self) -> Option<&str> {
        self.choice_set_expires_at.as_deref()
    }

    #[must_use]
    pub const fn rezka_count(&self) -> Option<u32> {
        self.rezka_count
    }

    #[must_use]
    pub const fn prowlarr_count(&self) -> Option<u32> {
        self.prowlarr_count
    }
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
        validate_human_label(&title)?;
        if !matches!(provider.as_str(), "rezka" | "prowlarr") {
            return Err(NotificationValidationError::InvalidProvider);
        }
        if let Some(translation) = &translation {
            validate_human_label(translation)?;
        }
        Ok(Self {
            job_id,
            title,
            kind,
            provider,
            season,
            translation,
            origin: None,
            poster_url: None,
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
    #[must_use]
    pub const fn origin(&self) -> Option<MediaNotificationOrigin> {
        self.origin
    }
    #[must_use]
    pub fn with_origin(mut self, origin: MediaNotificationOrigin) -> Self {
        self.origin = Some(origin);
        self
    }
    #[must_use]
    pub fn with_poster_url(mut self, poster_url: Option<String>) -> Self {
        self.poster_url = poster_url;
        self
    }
    #[must_use]
    pub fn poster_url(&self) -> Option<&str> {
        self.poster_url.as_deref()
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
            || current_episode == Some(0)
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
            total_bytes: None,
            download_speed_bps,
            percentage,
            eta_seconds: None,
            seeds: None,
            peers: None,
            source_state: None,
            connection_attempt: None,
            connection_attempt_limit: None,
            vpn_rotation_pending: None,
            storage_available_bytes: None,
            storage_required_bytes: None,
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
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }
    #[must_use]
    pub const fn download_speed_bps(&self) -> Option<u64> {
        self.download_speed_bps
    }
    #[must_use]
    pub const fn percentage(&self) -> Option<u8> {
        self.percentage
    }
    #[must_use]
    pub const fn eta_seconds(&self) -> Option<u64> {
        self.eta_seconds
    }
    #[must_use]
    pub const fn seeds(&self) -> Option<u64> {
        self.seeds
    }
    #[must_use]
    pub const fn peers(&self) -> Option<u64> {
        self.peers
    }
    #[must_use]
    pub fn source_state(&self) -> Option<&str> {
        self.source_state.as_deref()
    }
    #[must_use]
    pub const fn connection_attempt(&self) -> Option<u32> {
        self.connection_attempt
    }
    #[must_use]
    pub const fn connection_attempt_limit(&self) -> Option<u32> {
        self.connection_attempt_limit
    }
    #[must_use]
    pub const fn vpn_rotation_pending(&self) -> Option<bool> {
        self.vpn_rotation_pending
    }
    #[must_use]
    pub const fn storage_available_bytes(&self) -> Option<u64> {
        self.storage_available_bytes
    }
    #[must_use]
    pub const fn storage_required_bytes(&self) -> Option<u64> {
        self.storage_required_bytes
    }
    pub fn with_transfer_details(
        mut self,
        total_bytes: Option<u64>,
        eta_seconds: Option<u64>,
        seeds: Option<u64>,
        peers: Option<u64>,
        source_state: Option<String>,
    ) -> Result<Self, NotificationValidationError> {
        if self
            .downloaded_bytes
            .is_some_and(|downloaded| total_bytes.is_some_and(|total| downloaded > total))
        {
            return Err(NotificationValidationError::InvalidMediaResult);
        }
        if let Some(source_state) = &source_state {
            validate_machine_metadata(source_state, |character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })?;
        }
        self.total_bytes = total_bytes;
        self.eta_seconds = eta_seconds;
        self.seeds = seeds;
        self.peers = peers;
        self.source_state = source_state;
        Ok(self)
    }
    pub fn with_recovery(
        mut self,
        connection_attempt: Option<u32>,
        connection_attempt_limit: Option<u32>,
        vpn_rotation_pending: Option<bool>,
    ) -> Result<Self, NotificationValidationError> {
        if connection_attempt == Some(0)
            || connection_attempt_limit == Some(0)
            || connection_attempt.is_some_and(|attempt| {
                connection_attempt_limit.is_some_and(|limit| attempt > limit)
            })
        {
            return Err(NotificationValidationError::InvalidAttemptProgress);
        }
        self.connection_attempt = connection_attempt;
        self.connection_attempt_limit = connection_attempt_limit;
        self.vpn_rotation_pending = vpn_rotation_pending;
        Ok(self)
    }
    pub fn with_storage(
        mut self,
        storage_available_bytes: Option<u64>,
        storage_required_bytes: Option<u64>,
    ) -> Result<Self, NotificationValidationError> {
        if storage_required_bytes.is_some() != storage_available_bytes.is_some() {
            return Err(NotificationValidationError::InvalidStorageProgress);
        }
        self.storage_available_bytes = storage_available_bytes;
        self.storage_required_bytes = storage_required_bytes;
        Ok(self)
    }
}

impl MediaNotificationEpisode {
    pub const fn new(season: u32, episode: u32) -> Result<Self, NotificationValidationError> {
        if episode == 0 {
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

impl MediaNotificationResult {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        video: Option<MediaNotificationVideo>,
        audio: Option<MediaNotificationAudio>,
        subtitles: Option<MediaNotificationSubtitles>,
        file_size_bytes: Option<u64>,
        duration_seconds: Option<u64>,
        processing: Option<MediaNotificationProcessing>,
        publication: Option<MediaNotificationPublication>,
    ) -> Self {
        Self {
            video,
            audio,
            subtitles,
            file_size_bytes,
            duration_seconds,
            processing,
            publication,
        }
    }
    #[must_use]
    pub fn video(&self) -> Option<&MediaNotificationVideo> {
        self.video.as_ref()
    }
    #[must_use]
    pub fn audio(&self) -> Option<&MediaNotificationAudio> {
        self.audio.as_ref()
    }
    #[must_use]
    pub fn subtitles(&self) -> Option<&MediaNotificationSubtitles> {
        self.subtitles.as_ref()
    }
    #[must_use]
    pub const fn file_size_bytes(&self) -> Option<u64> {
        self.file_size_bytes
    }
    #[must_use]
    pub const fn duration_seconds(&self) -> Option<u64> {
        self.duration_seconds
    }
    #[must_use]
    pub fn processing(&self) -> Option<&MediaNotificationProcessing> {
        self.processing.as_ref()
    }
    #[must_use]
    pub fn publication(&self) -> Option<&MediaNotificationPublication> {
        self.publication.as_ref()
    }
}

impl MediaNotificationVideo {
    pub fn new(
        codec: String,
        profile: Option<String>,
        width: u32,
        height: u32,
    ) -> Result<Self, NotificationValidationError> {
        validate_codec(&codec)?;
        if let Some(profile) = &profile {
            validate_profile(profile)?;
        }
        if width == 0 || height == 0 {
            return Err(NotificationValidationError::InvalidMediaResult);
        }
        Ok(Self {
            codec,
            profile,
            width,
            height,
        })
    }
    #[must_use]
    pub fn codec(&self) -> &str {
        &self.codec
    }
    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }
}

impl MediaNotificationAudio {
    pub fn new(
        language: Option<String>,
        codec: String,
        channels: Option<u32>,
        channel_layout: Option<String>,
        title: Option<String>,
    ) -> Result<Self, NotificationValidationError> {
        if let Some(language) = &language {
            validate_language(language)?;
        }
        validate_codec(&codec)?;
        if let Some(channel_layout) = &channel_layout {
            validate_channel_layout(channel_layout)?;
        }
        if let Some(title) = &title {
            validate_human_label(title)?;
        }
        Ok(Self {
            language,
            codec,
            channels,
            channel_layout,
            title,
        })
    }
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }
    #[must_use]
    pub fn codec(&self) -> &str {
        &self.codec
    }
    #[must_use]
    pub const fn channels(&self) -> Option<u32> {
        self.channels
    }
    #[must_use]
    pub fn channel_layout(&self) -> Option<&str> {
        self.channel_layout.as_deref()
    }
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}

impl MediaNotificationSubtitles {
    #[must_use]
    pub const fn new(downloaded: u32, missing: u32) -> Self {
        Self {
            downloaded,
            missing,
        }
    }
    #[must_use]
    pub const fn downloaded(&self) -> u32 {
        self.downloaded
    }
    #[must_use]
    pub const fn missing(&self) -> u32 {
        self.missing
    }
}

impl MediaNotificationProcessing {
    #[must_use]
    pub const fn new(mode: MediaNotificationProcessingMode, elapsed_seconds: Option<u64>) -> Self {
        Self {
            mode,
            elapsed_seconds,
        }
    }
    #[must_use]
    pub const fn mode(&self) -> MediaNotificationProcessingMode {
        self.mode
    }
    #[must_use]
    pub const fn elapsed_seconds(&self) -> Option<u64> {
        self.elapsed_seconds
    }
}

impl MediaNotificationPublication {
    pub fn new(
        library: MediaNotificationLibrary,
        title: String,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Self, NotificationValidationError> {
        validate_human_label(&title)?;
        Ok(Self {
            library,
            title,
            season,
            episode,
        })
    }
    #[must_use]
    pub const fn library(&self) -> MediaNotificationLibrary {
        self.library
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub const fn season(&self) -> Option<u32> {
        self.season
    }
    #[must_use]
    pub const fn episode(&self) -> Option<u32> {
        self.episode
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
            result: None,
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
    pub fn result(&self) -> Option<&MediaNotificationResult> {
        self.result.as_ref()
    }
    pub fn with_result(
        mut self,
        result: MediaNotificationResult,
    ) -> Result<Self, NotificationValidationError> {
        if self.media.provider == "prowlarr"
            && result.processing().is_some_and(|processing| {
                processing.mode() == MediaNotificationProcessingMode::VaapiUpscale
            })
        {
            return Err(NotificationValidationError::InvalidMediaResult);
        }
        self.result = Some(result);
        Ok(self)
    }
    #[must_use]
    pub fn actions(&self) -> &[MediaNotificationAction] {
        &self.actions
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MediaNotification, MediaNotificationAudio, MediaNotificationDeliveryKind,
        MediaNotificationEpisode, MediaNotificationKind, MediaNotificationLibrary,
        MediaNotificationMedia, MediaNotificationProcessing, MediaNotificationProcessingMode,
        MediaNotificationProgress, MediaNotificationPublication, MediaNotificationResult,
        MediaNotificationState, MediaNotificationVideo, NotificationValidationError,
        SourceChoiceAction, SourceChoiceNotification,
    };
    use crate::{JobId, TrackingId};

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

    fn notification(provider: &str) -> MediaNotification {
        MediaNotification::new(
            MediaNotificationDeliveryKind::Card,
            "media-job:example".to_owned(),
            1,
            1,
            false,
            MediaNotificationState::Processing,
            MediaNotificationMedia::new(
                JobId::new(),
                "Example Show".to_owned(),
                MediaNotificationKind::Series,
                provider.to_owned(),
                Some(1),
                None,
            )
            .unwrap(),
            None,
            None,
            None,
            None,
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn source_choice_accepts_only_supported_provider_action_sets() {
        for actions in [
            vec![SourceChoiceAction::Rezka],
            vec![SourceChoiceAction::Prowlarr],
            vec![
                SourceChoiceAction::All,
                SourceChoiceAction::Rezka,
                SourceChoiceAction::Prowlarr,
            ],
        ] {
            assert!(
                SourceChoiceNotification::new(
                    "tracking:example:3:5".to_owned(),
                    TrackingId::new(),
                    "Example Show".to_owned(),
                    3,
                    5,
                    actions,
                )
                .is_ok()
            );
        }

        for actions in [
            vec![],
            vec![SourceChoiceAction::All],
            vec![SourceChoiceAction::Rezka, SourceChoiceAction::Prowlarr],
            vec![SourceChoiceAction::Prowlarr, SourceChoiceAction::Rezka],
        ] {
            assert_eq!(
                SourceChoiceNotification::new(
                    "tracking:example:3:5".to_owned(),
                    TrackingId::new(),
                    "Example Show".to_owned(),
                    3,
                    5,
                    actions,
                ),
                Err(NotificationValidationError::InvalidSourceChoiceActions),
            );
        }
    }

    #[test]
    fn specials_use_zero_season_but_episode_numbers_remain_positive() {
        assert!(MediaNotificationEpisode::new(0, 1).is_ok());
        assert_eq!(
            MediaNotificationEpisode::new(1, 0),
            Err(NotificationValidationError::InvalidEpisodeProgress),
        );
        assert!(
            MediaNotificationMedia::new(
                JobId::new(),
                "Example Show".to_owned(),
                MediaNotificationKind::Series,
                "rezka".to_owned(),
                Some(0),
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn absolute_episode_number_is_independent_from_job_task_count() {
        assert!(
            MediaNotificationProgress::new(
                Some(0),
                Some(1),
                Some(13),
                Vec::new(),
                None,
                None,
                None,
            )
            .is_ok()
        );
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

    #[test]
    fn detailed_result_requires_nonzero_video_dimensions() {
        assert_eq!(
            MediaNotificationVideo::new("hevc".to_owned(), None, 0, 1080),
            Err(NotificationValidationError::InvalidMediaResult),
        );
    }

    #[test]
    fn detailed_result_text_fields_allow_human_labels_but_reject_internal_values() {
        assert!(
            MediaNotificationVideo::new(
                "H.265 / HEVC".to_owned(),
                Some("Main 10-bit".to_owned()),
                1920,
                1080,
            )
            .is_ok()
        );
        assert!(
            MediaNotificationAudio::new(
                Some("Русский / 日本語".to_owned()),
                "AAC-LC".to_owned(),
                Some(6),
                Some("5.1 (side)".to_owned()),
                Some("AniLibria, Dub!".to_owned()),
            )
            .is_ok()
        );
        assert!(
            MediaNotificationPublication::new(
                MediaNotificationLibrary::TvShows,
                "Клинки Хранителей: сезон 2".to_owned(),
                Some(2),
                Some(8),
            )
            .is_ok()
        );

        assert_eq!(
            MediaNotificationVideo::new(
                "https://example.invalid/video".to_owned(),
                None,
                1920,
                1080
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationVideo::new(
                "hevc".to_owned(),
                Some("/srv/media/private.mkv".to_owned()),
                1920,
                1080,
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationAudio::new(
                Some("~/private/audio".to_owned()),
                "aac".to_owned(),
                None,
                None,
                None,
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationAudio::new(
                None,
                "C:\\media\\private.mkv".to_owned(),
                None,
                None,
                None,
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationAudio::new(
                None,
                "aac".to_owned(),
                None,
                Some("curl --data token=value".to_owned()),
                None,
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationAudio::new(
                None,
                "aac".to_owned(),
                None,
                None,
                Some("api_key=very-secret-value".to_owned()),
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationPublication::new(
                MediaNotificationLibrary::TvShows,
                "MEDIA_PROCESSING_FAILED".to_owned(),
                Some(2),
                Some(8),
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
    }

    #[test]
    fn detailed_result_text_fields_reject_relative_paths_and_command_forms() {
        assert!(
            MediaNotificationMedia::new(
                JobId::new(),
                "Title / Alternate: сезон 2".to_owned(),
                MediaNotificationKind::Series,
                "rezka".to_owned(),
                Some(2),
                Some("Русский / 日本語".to_owned()),
            )
            .is_ok()
        );
        assert!(
            MediaNotificationVideo::new(
                "H.265 / HEVC".to_owned(),
                Some("Main 10@L5.1".to_owned()),
                1920,
                1080,
            )
            .is_ok()
        );
        assert!(
            MediaNotificationAudio::new(
                Some("Русский / 日本語".to_owned()),
                "AAC-LC".to_owned(),
                Some(6),
                Some("5.1 (side)".to_owned()),
                Some("Title / Alternate".to_owned()),
            )
            .is_ok()
        );
        assert!(
            MediaNotificationPublication::new(
                MediaNotificationLibrary::TvShows,
                "Title / Alternate: сезон 2".to_owned(),
                Some(2),
                Some(8),
            )
            .is_ok()
        );

        assert_eq!(
            MediaNotificationVideo::new("../private.mkv".to_owned(), None, 1920, 1080),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationVideo::new(
                "hevc".to_owned(),
                Some("media/private.mkv".to_owned()),
                1920,
                1080,
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationAudio::new(
                Some("media\\private.srt".to_owned()),
                "aac".to_owned(),
                None,
                None,
                None,
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationAudio::new(None, "ls -la".to_owned(), None, None, None),
            Err(NotificationValidationError::InvalidDisplayField),
        );
        assert_eq!(
            MediaNotificationAudio::new(
                None,
                "aac".to_owned(),
                None,
                Some("cat private.mkv".to_owned()),
                Some("title; cat private.mkv".to_owned()),
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );

        for value in [
            "../private.mkv",
            "media/private.mkv",
            ".\\private.mkv",
            "media\\private.srt",
            "ls -la",
            "cat private.mkv",
            "--version",
            "title; cat private.mkv",
            "$(cat private.mkv)",
            "token=very-secret-value",
            "MEDIA_PROCESSING_FAILED",
        ] {
            assert_eq!(
                MediaNotificationPublication::new(
                    MediaNotificationLibrary::TvShows,
                    value.to_owned(),
                    Some(2),
                    Some(8),
                ),
                Err(NotificationValidationError::InvalidDisplayField),
                "{value} must not be accepted as a public display label",
            );
        }

        assert_eq!(
            MediaNotificationMedia::new(
                JobId::new(),
                "../private.mkv".to_owned(),
                MediaNotificationKind::Series,
                "rezka".to_owned(),
                Some(2),
                Some("media/private.mkv".to_owned()),
            ),
            Err(NotificationValidationError::InvalidDisplayField),
        );
    }

    #[test]
    fn prowlarr_result_cannot_claim_vaapi_upscale() {
        let result = MediaNotificationResult::new(
            None,
            None,
            None,
            None,
            None,
            Some(MediaNotificationProcessing::new(
                MediaNotificationProcessingMode::VaapiUpscale,
                Some(252),
            )),
            None,
        );

        assert_eq!(
            notification("prowlarr").with_result(result.clone()),
            Err(NotificationValidationError::InvalidMediaResult),
        );
        assert!(notification("rezka").with_result(result).is_ok());
    }

    #[test]
    fn recovery_progress_rejects_zero_or_exhausted_connection_attempts() {
        let progress =
            MediaNotificationProgress::new(None, None, None, Vec::new(), None, None, None).unwrap();

        for (attempt, limit) in [(Some(0), None), (None, Some(0)), (Some(21), Some(20))] {
            assert_eq!(
                progress.clone().with_recovery(attempt, limit, None),
                Err(NotificationValidationError::InvalidAttemptProgress),
            );
        }
    }

    #[test]
    fn storage_progress_requires_available_and_required_bytes_together() {
        let progress =
            MediaNotificationProgress::new(None, None, None, Vec::new(), None, None, None).unwrap();

        assert_eq!(
            progress.with_storage(Some(440_401_920), None),
            Err(NotificationValidationError::InvalidStorageProgress),
        );
    }
}
