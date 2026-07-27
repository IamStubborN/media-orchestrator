#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationEventTypeDto {
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

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HermesDeliverOnlyWebhook {
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_key: Option<String>,
    pub message: String,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceChoiceActionDto {
    All,
    Rezka,
    Prowlarr,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HermesSourceChoiceWebhook {
    pub event_type: String,
    pub schema_version: u16,
    pub card_key: String,
    pub tracking_id: crate::PublicId,
    pub title: String,
    pub season: u32,
    pub episode: u32,
    pub actions: Vec<SourceChoiceActionDto>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationDeliveryKindDto {
    Card,
    FinalPush,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationStateDto {
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

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationKindDto {
    Movie,
    Series,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationStageDto {
    Download,
    Process,
    Publish,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationNextStepDto {
    Download,
    Process,
    Publish,
    None,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationActionDto {
    Cancel,
    Details,
    Retry,
    RetryMissing,
    ResumeStorage,
    SearchAlternative,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationOriginDto {
    TrackedEpisode,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationDto {
    pub job_id: crate::PublicId,
    pub title: String,
    pub kind: MediaNotificationKindDto,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<MediaNotificationOriginDto>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationProgressDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_episodes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_episodes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_episode: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_episodes: Vec<MediaNotificationEpisodeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downloaded_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_speed_bps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_attempt_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vpn_rotation_pending: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_available_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_required_bytes: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationEpisodeDto {
    pub season: u32,
    pub episode: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationIssueDto {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationResultDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<MediaNotificationVideoDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<MediaNotificationAudioDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<MediaNotificationSubtitlesDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing: Option<MediaNotificationProcessingDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<MediaNotificationPublicationDto>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationVideoDto {
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationAudioDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationSubtitlesDto {
    pub downloaded: u32,
    pub missing: u32,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationProcessingModeDto {
    VaapiUpscale,
    Original,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationProcessingDto {
    pub mode: MediaNotificationProcessingModeDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationLibraryDto {
    Movies,
    TvShows,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationPublicationDto {
    pub library: MediaNotificationLibraryDto,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HermesMediaNotificationWebhook {
    pub event_type: String,
    pub schema_version: u16,
    pub delivery_kind: MediaNotificationDeliveryKindDto,
    pub card_key: String,
    pub revision: u64,
    pub lifecycle_cycle: u64,
    pub terminal: bool,
    pub state: MediaNotificationStateDto,
    pub media: MediaNotificationDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<MediaNotificationProgressDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<MediaNotificationStageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_step: Option<MediaNotificationNextStepDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<MediaNotificationIssueDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<MediaNotificationResultDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<MediaNotificationActionDto>,
}
