#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseQueryRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u64>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleasePrecisionDto {
    Date,
    DateTime,
    Unknown,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseLifecycleDto {
    Ongoing,
    Ended,
    Upcoming,
    Unknown,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseCandidateDto {
    pub source_id: u64,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub lifecycle: ReleaseLifecycleDto,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduledEpisodeDto {
    pub source_id: u64,
    pub season: u32,
    pub episode: u32,
    pub title: String,
    pub air_at: Option<String>,
    pub precision: ReleasePrecisionDto,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReleaseQueryResponse {
    Matched {
        source: String,
        fetched_at: String,
        show: ReleaseCandidateDto,
        precision: ReleasePrecisionDto,
        lifecycle: ReleaseLifecycleDto,
        released_episodes: u32,
        expected_episodes: Option<u32>,
        next_episode: Option<ScheduledEpisodeDto>,
        schedule: Vec<ScheduledEpisodeDto>,
    },
    ChoiceNeeded {
        source: String,
        fetched_at: String,
        candidates: Vec<ReleaseCandidateDto>,
    },
}
