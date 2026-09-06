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

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingReleaseSourceDto {
    Tvmaze,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingReleaseIdentityDto {
    pub source: TrackingReleaseSourceDto,
    pub source_id: u64,
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
    pub poster_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_identity: Option<TrackingReleaseIdentityDto>,
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
    pub poster_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    pub next_check_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_identity: Option<TrackingReleaseIdentityDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<TrackingDownloadDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_episodes: Vec<EpisodeSnapshotDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_age_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackingListDto {
    pub tracking: Vec<TrackingDto>,
}

#[cfg(test)]
mod tests {
    use super::{CreateTrackingRequest, TrackingDto};

    #[test]
    fn tracking_posters_are_optional_and_backward_compatible() {
        let legacy: CreateTrackingRequest = serde_json::from_value(serde_json::json!({
            "provider": "rezka",
            "title": "Show",
            "translation": "release-calendar",
            "known_episodes": [{"season": 1, "episode": 1}],
            "scope": "personal",
            "series_ongoing": true
        }))
        .unwrap();
        assert_eq!(legacy.poster_url, None);

        let tracking: TrackingDto = serde_json::from_value(serde_json::json!({
            "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
            "provider": "rezka",
            "title": "Show",
            "translation": "release-calendar",
            "known_episodes": [{"season": 1, "episode": 1}],
            "scope": "personal",
            "state": "active",
            "check_status": "never",
            "next_check_at": "2026-08-10T00:00:00Z",
            "poster_url": "https://image.tmdb.org/t/p/w780/show.jpg"
        }))
        .unwrap();
        assert_eq!(
            tracking.poster_url.as_deref(),
            Some("https://image.tmdb.org/t/p/w780/show.jpg")
        );
    }

    #[test]
    fn awaiting_source_fields_round_trip() {
        let tracking: TrackingDto = serde_json::from_value(serde_json::json!({
            "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
            "provider": "rezka",
            "title": "Show",
            "translation": "release-calendar",
            "known_episodes": [{"season": 2, "episode": 9}],
            "scope": "personal",
            "state": "active",
            "check_status": "awaiting_source",
            "last_checked_at": "2026-09-06T20:00:00Z",
            "next_check_at": "2026-09-06T23:00:00Z",
            "pending_episodes": [{"season": 2, "episode": 10}],
            "pending_since": "2026-09-06T18:00:00Z",
            "pending_age_seconds": 7200,
            "last_error": null,
            "status_reason": "aired S02E10, waiting for Rezka/Prowlarr"
        }))
        .unwrap();
        assert_eq!(tracking.pending_episodes.len(), 1);
        assert_eq!(tracking.pending_episodes[0].season, 2);
        assert_eq!(tracking.pending_episodes[0].episode, 10);
        assert_eq!(tracking.pending_age_seconds, Some(7200));
        assert_eq!(
            tracking.status_reason.as_deref(),
            Some("aired S02E10, waiting for Rezka/Prowlarr")
        );
    }
}
