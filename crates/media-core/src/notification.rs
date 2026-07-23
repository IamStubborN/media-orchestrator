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
    #[error("notification media result is invalid")]
    InvalidMediaResult,
    #[error("notification connection attempt progress is invalid")]
    InvalidAttemptProgress,
    #[error("notification storage progress is invalid")]
    InvalidStorageProgress,
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
            origin: None,
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
            download_speed_bps,
            percentage,
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
    pub const fn download_speed_bps(&self) -> Option<u64> {
        self.download_speed_bps
    }
    #[must_use]
    pub const fn percentage(&self) -> Option<u8> {
        self.percentage
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
        validate_public_display_field(&codec)?;
        if let Some(profile) = &profile {
            validate_public_display_field(profile)?;
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
        for value in [
            language.as_deref(),
            Some(codec.as_str()),
            channel_layout.as_deref(),
            title.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_public_display_field(value)?;
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
        validate_public_display_field(&title)?;
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
    #[must_use]
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

fn validate_display_field(value: &str) -> Result<(), NotificationValidationError> {
    if value.trim().is_empty()
        || value.len() > MAX_NOTIFICATION_DISPLAY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(NotificationValidationError::InvalidDisplayField);
    }
    Ok(())
}

fn validate_public_display_field(value: &str) -> Result<(), NotificationValidationError> {
    validate_display_field(value)?;

    let trimmed = value.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    if lowercase.contains("://")
        || lowercase.starts_with("www.")
        || lowercase.starts_with("magnet:?")
        || is_absolute_or_home_path(trimmed)
        || is_shell_command_like(&lowercase)
        || contains_secret_label(&lowercase)
        || is_internal_error_code(trimmed)
    {
        return Err(NotificationValidationError::InvalidDisplayField);
    }
    Ok(())
}

fn is_absolute_or_home_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || (value.starts_with('~')
            && value[1..]
                .chars()
                .take_while(|character| !character.is_whitespace())
                .any(|character| matches!(character, '/' | '\\')))
        || value.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

fn is_shell_command_like(value: &str) -> bool {
    const COMMANDS: [&str; 14] = [
        "curl",
        "wget",
        "bash",
        "sh",
        "zsh",
        "pwsh",
        "powershell",
        "cmd",
        "sudo",
        "rm",
        "python",
        "python3",
        "ffmpeg",
        "yt-dlp",
    ];

    value.contains("$(")
        || value.contains('`')
        || COMMANDS.iter().any(|command| {
            value == *command
                || value
                    .strip_prefix(command)
                    .and_then(|suffix| suffix.chars().next())
                    .is_some_and(char::is_whitespace)
        })
}

fn contains_secret_label(value: &str) -> bool {
    const LABELS: [&str; 14] = [
        "api key",
        "api_key",
        "api-key",
        "access token",
        "access_token",
        "authorization",
        "token",
        "password",
        "passwd",
        "secret",
        "credential",
        "private key",
        "private_key",
        "private-key",
    ];

    LABELS.iter().any(|label| {
        value.match_indices(label).any(|(index, _)| {
            let prefix_is_boundary = index == 0
                || value.as_bytes()[index - 1].is_ascii_whitespace()
                || matches!(value.as_bytes()[index - 1], b';' | b',');
            let suffix = &value[index + label.len()..];
            prefix_is_boundary
                && matches!(suffix.trim_start().as_bytes().first(), Some(b':' | b'='))
        })
    }) || value.match_indices("bearer").any(|(index, _)| {
        let prefix_is_boundary = index == 0
            || value.as_bytes()[index - 1].is_ascii_whitespace()
            || matches!(value.as_bytes()[index - 1], b';' | b',');
        prefix_is_boundary
            && value[index + "bearer".len()..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
    })
}

fn is_internal_error_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    let error_number = bytes.len() >= 4
        && matches!(bytes[0], b'E' | b'e')
        && bytes[1..].iter().all(|byte| byte.is_ascii_digit());
    let lower = value.to_ascii_lowercase();
    error_number
        || (value.contains('_')
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            && lower.split('_').any(|segment| {
                matches!(
                    segment,
                    "error"
                        | "failed"
                        | "failure"
                        | "invalid"
                        | "not"
                        | "found"
                        | "unavailable"
                        | "forbidden"
                        | "denied"
                        | "timeout"
                        | "internal"
                        | "exception"
                )
            }))
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
        MediaNotification, MediaNotificationAudio, MediaNotificationDeliveryKind,
        MediaNotificationEpisode, MediaNotificationKind, MediaNotificationLibrary,
        MediaNotificationMedia, MediaNotificationProcessing, MediaNotificationProcessingMode,
        MediaNotificationProgress, MediaNotificationPublication, MediaNotificationResult,
        MediaNotificationState, MediaNotificationVideo, NotificationValidationError,
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
