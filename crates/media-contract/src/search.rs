use crate::ProviderDto;

pub const MAX_SEARCH_RESULTS_PER_PAGE: usize = 10;

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartSearchRequest {
    pub scope: SearchScopeDto,
    pub source: ProviderDto,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_kind: Option<MediaKindDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_qualities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_codecs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_release_groups: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinueSearchRequest {
    pub continuation: String,
    pub scope: SearchScopeDto,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlternativeSearchRequest {
    pub scope: SearchScopeDto,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectResultRequest {
    pub session_id: String,
    pub result_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    pub scope: SearchScopeDto,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchScopeDto {
    pub platform: String,
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKindDto {
    Movie,
    Series,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RezkaTranslationDto {
    pub id: u64,
    pub name: String,
    pub premium: bool,
    pub director: bool,
    pub camrip: bool,
    pub has_ads: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SeasonAvailabilityDto {
    pub season: u32,
    pub episodes: Vec<u32>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackingPromptDto {
    pub title: String,
    pub latest_season: u32,
    pub latest_episode: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SeriesAvailabilityDto {
    pub lifecycle_status: SeriesLifecycleStatusDto,
    pub incomplete: bool,
    pub seasons: Vec<SeasonAvailabilityDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracking_prompt: Option<TrackingPromptDto>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesLifecycleStatusDto {
    Completed,
    Ongoing,
    Unknown,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProwlarrRankingDto {
    pub exact_title: bool,
    pub exact_season: bool,
    pub quality_preference: usize,
    pub language_preference: usize,
    pub seeders: i32,
    pub size_bytes: u64,
    pub codec_preference: usize,
    pub release_group_preference: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum SearchResultDto {
    Rezka {
        result_id: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        original_title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        year: Option<u16>,
        media_kind: MediaKindDto,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumbnail_url: Option<String>,
        translations: Vec<RezkaTranslationDto>,
        #[serde(skip_serializing_if = "Option::is_none")]
        availability: Option<SeriesAvailabilityDto>,
    },
    Prowlarr {
        result_id: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumbnail_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        website_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        indexer: Option<String>,
        size_bytes: u64,
        seeders: i32,
        #[serde(default)]
        leechers: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        published_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        age_days: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        release_group: Option<String>,
        ranking: ProwlarrRankingDto,
    },
}

const fn legacy_prowlarr_media_kind() -> MediaKindDto {
    MediaKindDto::Movie
}

impl SearchResultDto {
    #[must_use]
    pub fn result_id(&self) -> &str {
        match self {
            Self::Rezka { result_id, .. } | Self::Prowlarr { result_id, .. } => result_id,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchPageDto {
    pub api_version: String,
    pub session_id: String,
    pub source: ProviderDto,
    pub expires_at: String,
    pub results: Vec<SearchResultDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RezkaSessionRefreshRequest {
    pub credential_request_id: String,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeCoordinateDto {
    pub season: u32,
    pub episode: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeCoordinateMappingDto {
    pub provider: EpisodeCoordinateDto,
    pub canonical: EpisodeCoordinateDto,
    pub canonical_title: String,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmbiguousEpisodeDto {
    pub provider: EpisodeCoordinateDto,
    pub label: String,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeMappingActionDto {
    pub job_id: crate::PublicId,
    pub title: String,
    pub provider_media_ref: String,
    pub provider: EpisodeCoordinateDto,
    pub label: String,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveEpisodeMappingRequest {
    pub canonical_season: u32,
    pub canonical_episode: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_title: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ExecutionSelectionDto {
    RezkaSessionRefresh {
        credential_request_id: String,
    },
    Rezka {
        locator: String,
        title_id: u64,
        media_kind: MediaKindDto,
        translation_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        translation: Option<String>,
        director: bool,
        camrip: bool,
        has_ads: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        season: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        episode: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        episodes: Vec<crate::EpisodeSnapshotDto>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        episode_mappings: Vec<EpisodeCoordinateMappingDto>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ambiguous_episodes: Vec<AmbiguousEpisodeDto>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release_year: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thumbnail_url: Option<String>,
        title: String,
    },
    Prowlarr {
        source_identity: String,
        info_hash: String,
        uri: String,
        #[serde(default = "legacy_prowlarr_media_kind")]
        media_kind: MediaKindDto,
        #[serde(skip_serializing_if = "Option::is_none")]
        season: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        episode: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thumbnail_url: Option<String>,
        title: String,
    },
}
