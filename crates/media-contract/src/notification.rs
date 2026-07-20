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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<MediaNotificationActionDto>,
}
