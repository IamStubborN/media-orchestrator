#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationEventTypeDto {
    Started,
    ChoiceNeeded,
    DownloadingStarted,
    Downloaded,
    TranscodingStarted,
    EncodingComplete,
    PlexAdded,
    SessionRefreshed,
    Partial,
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
