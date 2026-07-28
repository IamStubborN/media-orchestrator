use crate::{ProviderDto, PublicId};

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingScopeDto {
    Personal,
    Family,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingStateDto {
    Active,
    ChoiceNeeded,
    Removed,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingCheckStatusDto {
    Never,
    NoNewEpisode,
    AwaitingSource,
    EpisodeFound,
    DownloadQueued,
    ReleaseError,
    SourceError,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeSnapshotDto {
    pub season: u32,
    pub episode: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingDownloadDto {
    pub provider_media_ref: String,
    pub translation_id: u64,
    pub season: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTrackingRequest {
    pub provider: ProviderDto,
    pub title: String,
    pub translation: String,
    pub known_episodes: Vec<EpisodeSnapshotDto>,
    pub scope: TrackingScopeDto,
    pub series_ongoing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<TrackingDownloadDto>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchTrackingRequest {
    pub translation: String,
    pub download: TrackingDownloadDto,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetTrackingBaselineRequest {
    pub known_through: EpisodeSnapshotDto,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackingDto {
    pub id: PublicId,
    pub provider: ProviderDto,
    pub title: String,
    pub translation: String,
    pub known_episodes: Vec<EpisodeSnapshotDto>,
    pub scope: TrackingScopeDto,
    pub state: TrackingStateDto,
    pub check_status: TrackingCheckStatusDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    pub next_check_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<TrackingDownloadDto>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackingListDto {
    pub tracking: Vec<TrackingDto>,
}
