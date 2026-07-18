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
