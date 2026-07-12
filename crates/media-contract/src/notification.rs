#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationEventTypeDto {
    Started,
    ChoiceNeeded,
    Downloaded,
    EncodingComplete,
    PlexAdded,
    Partial,
    Failed,
    FutureEpisodeFound,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HermesDeliverOnlyWebhook {
    pub event_type: String,
    pub message: String,
}
