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
#[serde(deny_unknown_fields)]
pub struct EpisodeSnapshotDto {
    pub season: u32,
    pub episode: u32,
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
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackingListDto {
    pub tracking: Vec<TrackingDto>,
}
