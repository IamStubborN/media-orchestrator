use std::sync::Arc;

use axum::{Router, http::request::Parts};
use media_contract::{
    AlternativeSearchRequest, BestPageDto, BestRankingDto, ContinueSearchRequest,
    CreateTrackingRequest, DiscoverPageDto, EpisodeSnapshotDto, ExecutionSelectionDto,
    GenreListDto, MediaKindDto, PatchTrackingRequest, PremiereFeedDto, PremieresPageDto,
    ProviderDto, ReleaseQueryRequest, ResolveEpisodeMappingRequest, SearchScopeDto,
    SelectResultRequest, SeriesGroupIdentityDto, SeriesGroupSourceDto, StartSearchRequest,
    TrackingDownloadDto, TrackingReleaseIdentityDto, TrackingReleaseSourceDto, TrackingScopeDto,
    TrendingCategoryDto, TrendingItemDto, TrendingMediaTypeDto,
};
use media_core::{
    Actor, ApplicationError, EpisodeSnapshot, JobId, ReleaseQuery, ReleaseQueryError,
    TrackingApplicationError, TrackingId,
};
use rmcp::schemars;
use rmcp::{
    handler::server::{tool::Extension, wrapper::Parameters},
    model::{CallToolResult, ErrorData},
    tool, tool_router,
    transport::{StreamableHttpServerConfig, StreamableHttpService},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ApiState, ChoiceSetSelection, convert};

#[derive(Clone)]
struct MediaAdminMcp {
    state: ApiState,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct JobIdInput {
    #[schemars(description = "Public media job ID")]
    job_id: String,
    #[serde(default)]
    #[schemars(description = "Expected current notification lifecycle cycle for mutation fencing")]
    #[schemars(range(min = 1))]
    expected_lifecycle_cycle: Option<u64>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct TrackingIdInput {
    #[schemars(description = "Public tracking subscription ID")]
    tracking_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ChoiceSetInput {
    #[schemars(description = "Opaque tracked-episode choice set identifier")]
    choice_set_id: String,
}

#[derive(Debug, Copy, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SearchSourceInput {
    #[default]
    All,
    Rezka,
    Prowlarr,
}

#[derive(Debug, Copy, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SearchMediaKindInput {
    Movie,
    Series,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchInput {
    query: Option<String>,
    continuation: Option<String>,
    #[serde(default)]
    source: SearchSourceInput,
    media_kind: Option<SearchMediaKindInput>,
    #[schemars(
        description = "Season number (>=1) for Prowlarr series search. If unknown, omit it, search Rezka, and offer discovered seasons from availability.seasons; never guess 1."
    )]
    #[schemars(range(min = 1))]
    season: Option<u16>,
    #[schemars(description = "Stable TMDB series identity for Plex season grouping")]
    #[schemars(range(min = 1))]
    tmdb_id: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DownloadInput {
    session_id: String,
    result_id: String,
    translation_id: Option<u64>,
    season: Option<u32>,
    episode: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ChoiceSetDownloadInput {
    #[schemars(description = "Opaque tracked-episode choice set identifier")]
    choice_set_id: String,
    #[schemars(description = "Explicit provider: rezka or prowlarr")]
    source: SearchSourceInput,
    result_id: String,
    translation_id: Option<u64>,
    season: Option<u32>,
    episode: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReleaseInput {
    title: String,
    original_title: Option<String>,
    year: Option<i32>,
    source_id: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TrendingInput {
    #[serde(default = "default_trending_category")]
    category: String,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1))]
    page: u32,
}

#[derive(Debug, Copy, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum DiscoveryMediaType {
    Movie,
    Tv,
}

impl From<DiscoveryMediaType> for TrendingMediaTypeDto {
    fn from(value: DiscoveryMediaType) -> Self {
        match value {
            DiscoveryMediaType::Movie => Self::Movie,
            DiscoveryMediaType::Tv => Self::Tv,
        }
    }
}

#[derive(Debug, Copy, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum BestRankingInput {
    #[default]
    TopRated,
    Popular,
}

impl From<BestRankingInput> for BestRankingDto {
    fn from(value: BestRankingInput) -> Self {
        match value {
            BestRankingInput::TopRated => Self::TopRated,
            BestRankingInput::Popular => Self::Popular,
        }
    }
}

#[derive(Debug, Copy, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PremiereFeedInput {
    NowPlaying,
    Upcoming,
    OnTheAir,
    AiringToday,
}

impl From<PremiereFeedInput> for PremiereFeedDto {
    fn from(value: PremiereFeedInput) -> Self {
        match value {
            PremiereFeedInput::NowPlaying => Self::NowPlaying,
            PremiereFeedInput::Upcoming => Self::Upcoming,
            PremiereFeedInput::OnTheAir => Self::OnTheAir,
            PremiereFeedInput::AiringToday => Self::AiringToday,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BestInput {
    media_type: DiscoveryMediaType,
    #[serde(default)]
    ranking: BestRankingInput,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1))]
    page: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PremieresInput {
    media_type: DiscoveryMediaType,
    feed: Option<PremiereFeedInput>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1))]
    page: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GenresInput {
    media_type: DiscoveryMediaType,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DiscoverInput {
    media_type: DiscoveryMediaType,
    #[schemars(range(min = 1), description = "TMDB genre identifier")]
    genre_id: u64,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1))]
    page: u32,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DiscoveryItemOutput {
    #[schemars(range(min = 1))]
    tmdb_id: u64,
    media_type: DiscoveryMediaType,
    title: String,
    original_title: Option<String>,
    year: Option<u16>,
    rating: Option<f32>,
    poster_url: Option<String>,
    overview: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct BestPageOutput {
    source: String,
    media_type: DiscoveryMediaType,
    ranking: BestRankingInput,
    #[schemars(range(min = 1))]
    page: u32,
    total_pages: u32,
    total_results: u32,
    #[schemars(length(max = 10))]
    results: Vec<DiscoveryItemOutput>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct PremieresPageOutput {
    source: String,
    media_type: DiscoveryMediaType,
    feed: PremiereFeedInput,
    #[schemars(range(min = 1))]
    page: u32,
    total_pages: u32,
    total_results: u32,
    #[schemars(length(max = 10))]
    results: Vec<DiscoveryItemOutput>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GenreOutput {
    id: u64,
    name: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GenreListOutput {
    source: String,
    media_type: DiscoveryMediaType,
    genres: Vec<GenreOutput>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DiscoverPageOutput {
    source: String,
    media_type: DiscoveryMediaType,
    #[schemars(range(min = 1))]
    genre_id: u64,
    #[schemars(range(min = 1))]
    page: u32,
    total_pages: u32,
    total_results: u32,
    #[schemars(length(max = 10))]
    results: Vec<DiscoveryItemOutput>,
}

impl From<TrendingMediaTypeDto> for DiscoveryMediaType {
    fn from(value: TrendingMediaTypeDto) -> Self {
        match value {
            TrendingMediaTypeDto::Movie => Self::Movie,
            TrendingMediaTypeDto::Tv => Self::Tv,
        }
    }
}

impl From<BestRankingDto> for BestRankingInput {
    fn from(value: BestRankingDto) -> Self {
        match value {
            BestRankingDto::TopRated => Self::TopRated,
            BestRankingDto::Popular => Self::Popular,
        }
    }
}

impl From<PremiereFeedDto> for PremiereFeedInput {
    fn from(value: PremiereFeedDto) -> Self {
        match value {
            PremiereFeedDto::NowPlaying => Self::NowPlaying,
            PremiereFeedDto::Upcoming => Self::Upcoming,
            PremiereFeedDto::OnTheAir => Self::OnTheAir,
            PremiereFeedDto::AiringToday => Self::AiringToday,
        }
    }
}

impl From<TrendingItemDto> for DiscoveryItemOutput {
    fn from(value: TrendingItemDto) -> Self {
        Self {
            tmdb_id: value.tmdb_id,
            media_type: value.media_type.into(),
            title: value.title,
            original_title: value.original_title,
            year: value.year,
            rating: value.rating,
            poster_url: value.poster_url,
            overview: value.overview,
        }
    }
}

impl From<BestPageDto> for BestPageOutput {
    fn from(value: BestPageDto) -> Self {
        Self {
            source: value.source,
            media_type: value.media_type.into(),
            ranking: value.ranking.into(),
            page: value.page,
            total_pages: value.total_pages,
            total_results: value.total_results,
            results: value.results.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<PremieresPageDto> for PremieresPageOutput {
    fn from(value: PremieresPageDto) -> Self {
        Self {
            source: value.source,
            media_type: value.media_type.into(),
            feed: value.feed.into(),
            page: value.page,
            total_pages: value.total_pages,
            total_results: value.total_results,
            results: value.results.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GenreListDto> for GenreListOutput {
    fn from(value: GenreListDto) -> Self {
        Self {
            source: value.source,
            media_type: value.media_type.into(),
            genres: value
                .genres
                .into_iter()
                .map(|genre| GenreOutput {
                    id: genre.id,
                    name: genre.name,
                })
                .collect(),
        }
    }
}

impl From<DiscoverPageDto> for DiscoverPageOutput {
    fn from(value: DiscoverPageDto) -> Self {
        Self {
            source: value.source,
            media_type: value.media_type.into(),
            genre_id: value.genre_id,
            page: value.page,
            total_pages: value.total_pages,
            total_results: value.total_results,
            results: value.results.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MediaDetailsInput {
    #[schemars(description = "TMDB media identifier")]
    tmdb_id: u64,
    #[schemars(description = "Media type: movie or tv")]
    media_type: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MediaSimilarInput {
    #[schemars(description = "TMDB media identifier")]
    tmdb_id: u64,
    #[schemars(description = "Media type: movie or tv")]
    media_type: String,
    #[serde(default = "default_page")]
    page: u32,
}

fn default_trending_category() -> String {
    "all".to_owned()
}

const fn default_page() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct EpisodeInput {
    #[schemars(description = "Season number, starting at 1")]
    #[schemars(range(min = 1))]
    season: u32,
    #[schemars(
        description = "Episode number, starting at 1; 0 is not allowed (use the first expected episode instead)"
    )]
    #[schemars(range(min = 1))]
    episode: u32,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct TrackingDownloadInput {
    provider_media_ref: String,
    translation_id: u64,
    season: u32,
}

#[derive(Debug, Copy, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TrackingReleaseSourceInput {
    Tvmaze,
}

#[derive(Debug, Copy, Clone, Deserialize, schemars::JsonSchema)]
struct TrackingReleaseIdentityInput {
    source: TrackingReleaseSourceInput,
    source_id: u64,
}

#[derive(Debug, Copy, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum TrackingProviderInput {
    #[default]
    Rezka,
    Prowlarr,
}

#[derive(Debug, Copy, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum TrackingScopeInput {
    Personal,
    Family,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TrackingCreateInput {
    #[serde(default)]
    provider: TrackingProviderInput,
    title: String,
    #[serde(default = "default_tracking_translation")]
    translation: String,
    known_episodes: Vec<EpisodeInput>,
    scope: TrackingScopeInput,
    #[serde(default = "default_true")]
    series_ongoing: bool,
    poster_url: Option<String>,
    release_identity: Option<TrackingReleaseIdentityInput>,
    download: Option<TrackingDownloadInput>,
}

fn default_tracking_translation() -> String {
    "release-calendar".to_owned()
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TrackingDownloadUpdateInput {
    tracking_id: String,
    translation: String,
    provider_media_ref: String,
    translation_id: u64,
    season: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TrackingBaselineInput {
    tracking_id: String,
    known_through: EpisodeInput,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AlternativeSearchInput {
    job_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ResolveEpisodeInput {
    job_id: String,
    season: u32,
    episode: u32,
    title: Option<String>,
}

#[derive(Debug, schemars::JsonSchema)]
#[allow(dead_code)]
struct ObjectOutput {
    #[serde(flatten)]
    fields: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LimitInput {
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 50))]
    limit: u16,
    #[schemars(description = "Optional Plex rating key to enrich with TMDB card metadata")]
    rating_key: Option<u64>,
}
fn default_limit() -> u16 {
    10
}

const MAX_PAGE_LIMIT: u16 = 50;
const TOOL_SCHEMA_TTL_MS: u64 = 300_000;

#[derive(Debug, Copy, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ReadView {
    #[default]
    Summary,
    Card,
    Diagnostic,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PageInput {
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 50), description = "Page size; default 10")]
    limit: u16,
    #[schemars(description = "Opaque cursor returned by the previous page")]
    cursor: Option<String>,
    #[serde(default)]
    #[schemars(description = "Response detail: summary, card, or diagnostic")]
    view: ReadView,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct PageMeta {
    returned: usize,
    total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct JobListItemOutput {
    id: String,
    provider: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    poster_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    season: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    episode: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    episode_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    library_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    release_year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    notify_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    needs_action_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    result_ref: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct JobListOutput {
    jobs: Vec<JobListItemOutput>,
    #[serde(flatten)]
    page: PageMeta,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
struct EpisodeOutput {
    season: u32,
    episode: u32,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TrackingReleaseIdentityOutput {
    source: String,
    source_id: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TrackingDownloadOutput {
    provider_media_ref: String,
    translation_id: u64,
    season: u32,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TrackingListItemOutput {
    id: String,
    title: String,
    provider: String,
    scope: String,
    state: String,
    check_status: String,
    known_episodes: Vec<EpisodeOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    poster_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    translation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    last_checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    next_check_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    release_identity: Option<TrackingReleaseIdentityOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    download: Option<TrackingDownloadOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(skip)]
    pending_episodes: Vec<EpisodeOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pending_since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pending_age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    status_reason: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TrackingListOutput {
    tracking: Vec<TrackingListItemOutput>,
    #[serde(flatten)]
    page: PageMeta,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct PlexRecentItemOutput {
    #[serde(rename = "ratingKey")]
    rating_key: String,
    #[serde(rename = "type")]
    media_type: String,
    title: String,
    #[serde(rename = "grandparentTitle", skip_serializing_if = "Option::is_none")]
    grandparent_title: Option<String>,
    #[serde(rename = "parentTitle", skip_serializing_if = "Option::is_none")]
    parent_title: Option<String>,
    #[serde(rename = "parentIndex", skip_serializing_if = "Option::is_none")]
    parent_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    year: Option<u64>,
    #[serde(rename = "addedAt", skip_serializing_if = "Option::is_none")]
    added_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    thumb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    tmdb_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    original_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    release_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    rating: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    poster_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    overview: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(skip)]
    countries: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(skip)]
    genres: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    season_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    episode_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    tmdb_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    imdb_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    trailer_url: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct PlexRecentOutput {
    items: Vec<PlexRecentItemOutput>,
    returned: usize,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct StorageRootOutput {
    path: String,
    total_bytes: u64,
    available_bytes: u64,
    used_bytes: u64,
    used_percent: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct StorageStatusOutput {
    roots: Vec<StorageRootOutput>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PlexSearchInput {
    query: String,
    #[serde(default = "default_limit")]
    limit: u16,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PlexLibraryInput {
    section_key: u32,
    #[serde(default)]
    start: u32,
    #[serde(default = "default_limit")]
    limit: u16,
    #[schemars(description = "Optional Plex rating key to enrich with TMDB card metadata")]
    rating_key: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RatingKeyInput {
    rating_key: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PlexRefreshInput {
    section_key: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TorrentListInput {
    filter: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TorrentInput {
    hash: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TorrentControlInput {
    hash: String,
    action: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FileInput {
    path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DestructivePrepareInput {
    action: String,
    target: String,
    #[serde(default)]
    delete_files: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DestructiveConfirmInput {
    confirmation_token: String,
}

#[tool_router]
impl MediaAdminMcp {
    fn new(state: ApiState) -> Self {
        Self { state }
    }

    #[tool(
        name = "media_jobs_list",
        description = "List the authenticated user's media jobs and their current states. Read-only.",
        output_schema = output_schema::<JobListOutput>(),
        annotations(title = "List media jobs", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_jobs(
        &self,
        Parameters(input): Parameters<PageInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let jobs = self
            .state
            .jobs()
            .list_jobs(&actor)
            .await
            .map_err(application_error)?;
        let total = jobs.len();
        let (start, end, next_cursor) = page_bounds(total, &input)?;
        let jobs = futures_util::future::join_all(jobs[start..end].iter().map(|job| async {
            let value =
                serde_json::to_value(convert::job(job)).expect("job DTOs serialize to JSON");
            enrich_job_value(&self.state, job.result_ref(), value).await
        }))
        .await;
        let jobs = jobs
            .into_iter()
            .map(|value| job_list_item(value, input.view))
            .collect();
        result_json_for(
            &parts,
            JobListOutput {
                jobs,
                page: PageMeta {
                    returned: end.saturating_sub(start),
                    total,
                    next_cursor,
                },
            },
        )
    }

    #[tool(
        name = "media_job_get",
        description = "Get sanitized details and measured progress for one of the authenticated user's media jobs. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Get media job", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_job(
        &self,
        Parameters(input): Parameters<JobIdInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let job_id = parse_job_id(&input.job_id)?;
        let detail = self
            .state
            .jobs()
            .get_job_detail(&actor, job_id)
            .await
            .map_err(application_error)?;
        let result_ref = detail.job.result_ref().to_owned();
        let value = serde_json::to_value(convert::job_detail(&detail))
            .map_err(|_| ErrorData::internal_error("failed to serialize media job", None))?;
        result_json_for(
            &parts,
            enrich_job_value(&self.state, &result_ref, value).await,
        )
    }

    #[tool(
        name = "media_queue_status",
        description = "Show the authenticated user's queue and runner state. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Get media queue status", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn queue_status(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let status = self
            .state
            .jobs()
            .queue_status(&actor)
            .await
            .map_err(application_error)?;
        result_json_for(&parts, convert::queue_status(status))
    }

    #[tool(
        name = "media_job_cancel",
        description = "Cancel one of the authenticated user's media jobs. This changes state but does not delete files.",
        output_schema = object_output_schema(),
        annotations(title = "Cancel media job", read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn cancel_job(
        &self,
        Parameters(input): Parameters<JobIdInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let job_id = parse_job_id(&input.job_id)?;
        let job = self
            .state
            .jobs()
            .cancel_job_if_current(
                &actor,
                stable_operation_key(
                    "cancel",
                    format!("{job_id}:{}", input.expected_lifecycle_cycle.unwrap_or(0)),
                ),
                job_id,
                input.expected_lifecycle_cycle,
            )
            .await
            .map_err(application_error)?;
        result_json_for(&parts, convert::job(&job))
    }

    #[tool(
        name = "media_job_retry",
        description = "Retry one of the authenticated user's failed or partial media jobs.",
        output_schema = object_output_schema(),
        annotations(title = "Retry media job", read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true)
    )]
    async fn retry_job(
        &self,
        Parameters(input): Parameters<JobIdInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let job_id = parse_job_id(&input.job_id)?;
        let job = self
            .state
            .jobs()
            .retry_job_if_current(
                &actor,
                unique_operation_key("retry", job_id),
                job_id,
                input.expected_lifecycle_cycle,
            )
            .await
            .map_err(application_error)?;
        result_json_for(&parts, convert::job(&job))
    }

    #[tool(
        name = "media_tracking_list",
        description = "List the authenticated user's tracking subscriptions and their check state. Read-only.",
        output_schema = output_schema::<TrackingListOutput>(),
        annotations(title = "List media tracking", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_tracking(
        &self,
        Parameters(input): Parameters<PageInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let tracking = self
            .state
            .tracking()
            .ok_or_else(|| ErrorData::internal_error("tracking is not configured", None))?;
        let values = tracking.list(&actor).await.map_err(tracking_error)?;
        let total = values.len();
        let (start, end, next_cursor) = page_bounds(total, &input)?;
        let tracking = values[start..end]
            .iter()
            .map(convert::tracking)
            .map(|value| tracking_list_item(value, input.view))
            .collect();
        result_json_for(
            &parts,
            TrackingListOutput {
                tracking,
                page: PageMeta {
                    returned: end.saturating_sub(start),
                    total,
                    next_cursor,
                },
            },
        )
    }

    #[tool(
        name = "media_tracking_check",
        description = "Run an immediate check for one of the authenticated user's tracking subscriptions.",
        output_schema = object_output_schema(),
        annotations(title = "Check media tracking", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn check_tracking(
        &self,
        Parameters(input): Parameters<TrackingIdInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let tracking = self
            .state
            .tracking()
            .ok_or_else(|| ErrorData::internal_error("tracking is not configured", None))?;
        let tracking_id = input
            .tracking_id
            .parse::<TrackingId>()
            .map_err(|_| ErrorData::invalid_params("tracking_id is invalid", None))?;
        let value = tracking
            .check_now(&actor, tracking_id)
            .await
            .map_err(tracking_error)?;
        result_json_for(&parts, convert::tracking(&value))
    }

    #[tool(
        name = "media_tracking_create",
        description = "Create personal or family tracking, optionally with an explicit Rezka translation for future downloads.",
        output_schema = object_output_schema(),
        annotations(title = "Create media tracking", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn create_tracking(
        &self,
        Parameters(input): Parameters<TrackingCreateInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let tracking = configured_tracking(&self.state)?;
        let provider = match input.provider {
            TrackingProviderInput::Rezka => ProviderDto::Rezka,
            TrackingProviderInput::Prowlarr => ProviderDto::Prowlarr,
        };
        let scope = match input.scope {
            TrackingScopeInput::Personal => TrackingScopeDto::Personal,
            TrackingScopeInput::Family => TrackingScopeDto::Family,
        };
        let request = CreateTrackingRequest {
            provider,
            title: input.title,
            translation: input.translation,
            known_episodes: input.known_episodes.into_iter().map(episode_dto).collect(),
            scope,
            series_ongoing: input.series_ongoing,
            poster_url: input.poster_url,
            release_identity: input
                .release_identity
                .map(|identity| TrackingReleaseIdentityDto {
                    source: match identity.source {
                        TrackingReleaseSourceInput::Tvmaze => TrackingReleaseSourceDto::Tvmaze,
                    },
                    source_id: identity.source_id,
                }),
            download: input.download.map(tracking_download_dto),
        };
        let owner = actor
            .user_id()
            .ok_or_else(|| ErrorData::internal_error("authenticated user is unavailable", None))?;
        let operation = stable_owner_payload_operation_key("tracking-create", owner, &request)?;
        let command = convert::new_tracking_command(request)
            .map_err(|_| ErrorData::invalid_params("tracking request is invalid", None))?;
        let value = tracking
            .add(&actor, operation, command)
            .await
            .map_err(tracking_error)?;
        result_json_for(&parts, convert::tracking(&value))
    }

    #[tool(
        name = "media_tracking_enable_download",
        description = "Enable future automatic Rezka episode downloads on an existing tracking subscription using an explicitly selected translation.",
        output_schema = object_output_schema(),
        annotations(title = "Enable tracking downloads", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn enable_tracking_download(
        &self,
        Parameters(input): Parameters<TrackingDownloadUpdateInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let tracking = configured_tracking(&self.state)?;
        let tracking_id = parse_tracking_id(&input.tracking_id)?;
        let request = PatchTrackingRequest {
            translation: input.translation,
            download: TrackingDownloadDto {
                provider_media_ref: input.provider_media_ref,
                translation_id: input.translation_id,
                season: input.season,
            },
        };
        let patch = convert::tracking_download_patch(request)
            .map_err(|_| ErrorData::invalid_params("tracking request is invalid", None))?;
        let value = tracking
            .patch_download(&actor, tracking_id, patch)
            .await
            .map_err(tracking_error)?;
        result_json_for(&parts, convert::tracking(&value))
    }

    #[tool(
        name = "media_tracking_set_baseline",
        description = "Change the known-through episode of an existing tracking subscription without recreating it.",
        output_schema = object_output_schema(),
        annotations(title = "Set tracking baseline", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn set_tracking_baseline(
        &self,
        Parameters(input): Parameters<TrackingBaselineInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let tracking = configured_tracking(&self.state)?;
        let tracking_id = parse_tracking_id(&input.tracking_id)?;
        let baseline =
            EpisodeSnapshot::new(input.known_through.season, input.known_through.episode)
                .map_err(|_| ErrorData::invalid_params("tracking baseline is invalid", None))?;
        let value = tracking
            .set_baseline(&actor, tracking_id, baseline)
            .await
            .map_err(tracking_error)?;
        result_json_for(&parts, convert::tracking(&value))
    }

    #[tool(
        name = "media_tracking_remove",
        description = "Remove one of the authenticated user's tracking subscriptions. Does not delete downloaded media.",
        output_schema = object_output_schema(),
        annotations(title = "Remove media tracking", read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn remove_tracking(
        &self,
        Parameters(input): Parameters<TrackingIdInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let tracking = configured_tracking(&self.state)?;
        let tracking_id = parse_tracking_id(&input.tracking_id)?;
        let value = tracking
            .remove(
                &actor,
                stable_operation_key("tracking-remove", tracking_id.to_string()),
                tracking_id,
            )
            .await
            .map_err(tracking_error)?;
        let mut value = convert::tracking(&value);
        value.state = media_contract::TrackingStateDto::Removed;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_search",
        description = "Search Rezka/Prowlarr or continue one page. Never downloads. For a series without a named season, search Rezka only, discover seasons from availability.seasons, and offer those; pass season>=1 before Prowlarr or source=all.",
        output_schema = object_output_schema(),
        annotations(title = "Search media providers", read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true)
    )]
    async fn search(
        &self,
        Parameters(input): Parameters<SearchInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        if input.continuation.is_some() {
            if input.query.is_some() {
                return Err(ErrorData::invalid_params(
                    "query and continuation are mutually exclusive",
                    None,
                ));
            }
            let page = self
                .state
                .search()
                .continue_search(
                    owner,
                    ContinueSearchRequest {
                        continuation: input.continuation.unwrap_or_default(),
                        scope: mcp_scope(owner),
                    },
                )
                .await
                .map_err(search_error)?;
            return result_json_for(&parts, page);
        }
        let query = input
            .query
            .ok_or_else(|| ErrorData::invalid_params("query or continuation is required", None))?;
        let media_kind = match input.media_kind {
            Some(SearchMediaKindInput::Movie) => Some(MediaKindDto::Movie),
            Some(SearchMediaKindInput::Series) => Some(MediaKindDto::Series),
            None => None,
        };
        if input.tmdb_id.is_some_and(|value| value == 0)
            || input.tmdb_id.is_some() && media_kind != Some(MediaKindDto::Series)
        {
            return Err(ErrorData::invalid_params(
                "tmdb_id requires a positive series search identity",
                None,
            ));
        }
        let series_group = input.tmdb_id.map(|source_id| SeriesGroupIdentityDto {
            source: SeriesGroupSourceDto::Tmdb,
            source_id,
        });
        let providers: &[ProviderDto] = match (input.source, media_kind, input.season) {
            (SearchSourceInput::All, Some(MediaKindDto::Series), None) => &[ProviderDto::Rezka],
            (SearchSourceInput::All, _, _) => &[ProviderDto::Rezka, ProviderDto::Prowlarr],
            (SearchSourceInput::Rezka, _, _) => &[ProviderDto::Rezka],
            (SearchSourceInput::Prowlarr, _, _) => &[ProviderDto::Prowlarr],
        };
        let mut results = serde_json::Map::new();
        for provider in providers {
            let request = StartSearchRequest {
                scope: mcp_scope(owner),
                source: *provider,
                query: query.clone(),
                media_kind,
                season: input.season,
                series_group,
                preferred_qualities: vec![],
                preferred_languages: vec![],
                preferred_codecs: vec![],
                preferred_release_groups: vec![],
            };
            let key = match provider {
                ProviderDto::Rezka => "rezka",
                ProviderDto::Prowlarr => "prowlarr",
            };
            let value = match self.state.search().start(owner, request).await {
                Ok(page) => serde_json::to_value(page)
                    .unwrap_or_else(|_| serde_json::json!({"error":"serialization_failed"})),
                Err(error) => serde_json::json!({"error": search_error_code(error)}),
            };
            results.insert(key.to_owned(), value);
        }
        result_json_for(&parts, serde_json::Value::Object(results))
    }

    #[tool(
        name = "media_episode_choice_set",
        description = "Read cached public provider choices for a tracked episode; never searches or exposes private locators.",
        output_schema = object_output_schema(),
        annotations(title = "Get episode choices", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn episode_choice_set(
        &self,
        Parameters(input): Parameters<ChoiceSetInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        if input.choice_set_id.trim().is_empty() || input.choice_set_id.len() > 64 {
            return Err(ErrorData::invalid_params("choice_set_id is invalid", None));
        }
        let value = self
            .state
            .search()
            .choice_set(owner, &input.choice_set_id)
            .await
            .map_err(search_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_episode_choice_set_refresh",
        description = "Refresh an expired tracked-episode choice set and return public provider choices.",
        output_schema = object_output_schema(),
        annotations(title = "Refresh episode choices", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn episode_choice_set_refresh(
        &self,
        Parameters(input): Parameters<ChoiceSetInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        if input.choice_set_id.trim().is_empty() || input.choice_set_id.len() > 64 {
            return Err(ErrorData::invalid_params("choice_set_id is invalid", None));
        }
        let value = self
            .state
            .search()
            .refresh_choice_set(owner, &input.choice_set_id)
            .await
            .map_err(search_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_episode_choice_set_download",
        description = "Download one explicit provider result from a tracked-episode choice set; no fallback or implicit selection.",
        output_schema = object_output_schema(),
        annotations(title = "Download tracked episode choice", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn episode_choice_set_download(
        &self,
        Parameters(input): Parameters<ChoiceSetDownloadInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        if input.choice_set_id.trim().is_empty()
            || input.choice_set_id.len() > 64
            || input.result_id.trim().is_empty()
            || input.result_id.len() > 512
            || input.result_id.chars().any(char::is_control)
        {
            return Err(ErrorData::invalid_params(
                "choice-set download identifiers are invalid",
                None,
            ));
        }
        let source = match input.source {
            SearchSourceInput::Rezka => ProviderDto::Rezka,
            SearchSourceInput::Prowlarr => ProviderDto::Prowlarr,
            SearchSourceInput::All => {
                return Err(ErrorData::invalid_params(
                    "source must be rezka or prowlarr",
                    None,
                ));
            }
        };
        let operation_payload = serde_json::json!({
            "choice_set_id": input.choice_set_id,
            "source": match source {
                ProviderDto::Rezka => "rezka",
                ProviderDto::Prowlarr => "prowlarr",
            },
            "result_id": input.result_id,
            "translation_id": input.translation_id,
            "season": input.season,
            "episode": input.episode,
        });
        let operation = choice_set_download_operation_key(owner, &operation_payload)?;
        let value = self
            .state
            .search()
            .select_choice_set(
                owner,
                ChoiceSetSelection {
                    operation,
                    choice_set_id: input.choice_set_id,
                    source,
                    result_id: input.result_id,
                    translation_id: input.translation_id,
                    season: input.season,
                    episode: input.episode,
                },
            )
            .await
            .map_err(search_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_download",
        description = "Download one explicit media_search result; never selects provider, translation, season, or episode.",
        output_schema = object_output_schema(),
        annotations(title = "Download selected media", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn download(
        &self,
        Parameters(input): Parameters<DownloadInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        let request = SelectResultRequest {
            session_id: input.session_id,
            result_id: input.result_id,
            translation_id: input.translation_id,
            season: input.season,
            episode: input.episode,
            scope: mcp_scope(owner),
        };
        let operation = stable_payload_operation_key("download", &request)?;
        let value = self
            .state
            .search()
            .select(owner, operation, request)
            .await
            .map_err(search_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_release_schedule",
        description = "Query the release calendar for episode counts, lifecycle, schedule, and next episode. Read-only and never starts a download.",
        output_schema = object_output_schema(),
        annotations(title = "Get release schedule", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn release_schedule(
        &self,
        Parameters(input): Parameters<ReleaseInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        actor_from_parts(&parts)?;
        let service = self
            .state
            .release_metadata()
            .ok_or_else(|| ErrorData::internal_error("release metadata is not configured", None))?;
        let request = ReleaseQueryRequest {
            title: input.title,
            original_title: input.original_title,
            year: input.year,
            source_id: input.source_id,
        };
        let mut query = ReleaseQuery::new(request.title, request.original_title, request.year)
            .map_err(|_| ErrorData::invalid_params("release query is invalid", None))?;
        if let Some(source_id) = request.source_id {
            query = query
                .with_source_id(source_id)
                .map_err(|_| ErrorData::invalid_params("release query is invalid", None))?;
        }
        let value = service.query(query).await.map_err(release_error)?;
        result_json_for(&parts, convert::release_result(value))
    }

    #[tool(
        name = "media_trending",
        description = "List worldwide weekly TMDB trends for movies, series, or both. Read-only and never starts a search or download.",
        output_schema = object_output_schema(),
        annotations(title = "Get weekly media trends", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn trending(
        &self,
        Parameters(input): Parameters<TrendingInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        actor_from_parts(&parts)?;
        if input.page == 0 {
            return Err(ErrorData::invalid_params("page must be positive", None));
        }
        let category = match input.category.as_str() {
            "all" => TrendingCategoryDto::All,
            "movie" => TrendingCategoryDto::Movie,
            "tv" => TrendingCategoryDto::Tv,
            _ => {
                return Err(ErrorData::invalid_params(
                    "category must be all, movie, or tv",
                    None,
                ));
            }
        };
        let value = self
            .state
            .trending()
            .trending(category, input.page)
            .await
            .map_err(trending_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_best",
        description = "List up to 10 top-rated or popular localized TMDB titles.",
        output_schema = output_schema::<BestPageOutput>(),
        annotations(title = "List best media", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn best(
        &self,
        Parameters(input): Parameters<BestInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        actor_from_parts(&parts)?;
        if input.page == 0 {
            return Err(ErrorData::invalid_params("page must be positive", None));
        }
        let value = self
            .state
            .trending()
            .best(input.media_type.into(), input.ranking.into(), input.page)
            .await
            .map_err(trending_error)?;
        result_json_for(&parts, BestPageOutput::from(value))
    }

    #[tool(
        name = "media_premieres",
        description = "List up to 10 current or upcoming TMDB movies or TV series.",
        output_schema = output_schema::<PremieresPageOutput>(),
        annotations(title = "List media premieres", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn premieres(
        &self,
        Parameters(input): Parameters<PremieresInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        actor_from_parts(&parts)?;
        if input.page == 0 {
            return Err(ErrorData::invalid_params("page must be positive", None));
        }
        let feed = input.feed.unwrap_or(match input.media_type {
            DiscoveryMediaType::Movie => PremiereFeedInput::NowPlaying,
            DiscoveryMediaType::Tv => PremiereFeedInput::OnTheAir,
        });
        let value = self
            .state
            .trending()
            .premieres(input.media_type.into(), feed.into(), input.page)
            .await
            .map_err(trending_error)?;
        result_json_for(&parts, PremieresPageOutput::from(value))
    }

    #[tool(
        name = "media_genres",
        description = "List localized TMDB genre IDs for media_discover.",
        output_schema = output_schema::<GenreListOutput>(),
        annotations(title = "List media genres", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn genres(
        &self,
        Parameters(input): Parameters<GenresInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        actor_from_parts(&parts)?;
        let value = self
            .state
            .trending()
            .genres(input.media_type.into())
            .await
            .map_err(trending_error)?;
        result_json_for(&parts, GenreListOutput::from(value))
    }

    #[tool(
        name = "media_discover",
        description = "List up to 10 localized TMDB titles in a genre by popularity.",
        output_schema = output_schema::<DiscoverPageOutput>(),
        annotations(title = "Discover media by genre", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn discover(
        &self,
        Parameters(input): Parameters<DiscoverInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        actor_from_parts(&parts)?;
        if input.genre_id == 0 || input.page == 0 {
            return Err(ErrorData::invalid_params(
                "genre_id and page must be positive",
                None,
            ));
        }
        let value = self
            .state
            .trending()
            .discover(input.media_type.into(), input.genre_id, input.page)
            .await
            .map_err(trending_error)?;
        result_json_for(&parts, DiscoverPageOutput::from(value))
    }

    #[tool(
        name = "media_details",
        description = "Get localized TMDB details for one movie or series, including poster, metadata, URLs, and TV episode data.",
        output_schema = object_output_schema(),
        annotations(title = "Get media details", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn details(
        &self,
        Parameters(input): Parameters<MediaDetailsInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        actor_from_parts(&parts)?;
        if input.tmdb_id == 0 {
            return Err(ErrorData::invalid_params("tmdb_id must be positive", None));
        }
        let media_type = parse_media_type(&input.media_type)?;
        let value = self
            .state
            .media_details()
            .details(input.tmdb_id, media_type)
            .await
            .map_err(media_details_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_similar",
        description = "List up to 10 read-only TMDB similar or recommended movies or series for one title. Use page for pagination.",
        output_schema = object_output_schema(),
        annotations(title = "Find similar media", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn similar(
        &self,
        Parameters(input): Parameters<MediaSimilarInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        actor_from_parts(&parts)?;
        if input.tmdb_id == 0 {
            return Err(ErrorData::invalid_params("tmdb_id must be positive", None));
        }
        if input.page == 0 {
            return Err(ErrorData::invalid_params("page must be positive", None));
        }
        let media_type = parse_media_type(&input.media_type)?;
        let value = self
            .state
            .media_details()
            .similar(input.tmdb_id, media_type, input.page)
            .await
            .map_err(media_details_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_job_alternatives",
        description = "Search for explicit alternative results for a failed or partial job. Never switches the source or starts a download automatically.",
        output_schema = object_output_schema(),
        annotations(title = "Find job alternatives", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn job_alternatives(
        &self,
        Parameters(input): Parameters<AlternativeSearchInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        let job_id = parse_job_id(&input.job_id)?;
        let value = self
            .state
            .search()
            .start_alternative(
                owner,
                job_id,
                AlternativeSearchRequest {
                    scope: mcp_scope(owner),
                },
            )
            .await
            .map_err(search_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_job_mapping_get",
        description = "Get the unresolved provider episode coordinate for a job that needs an explicit canonical Plex mapping. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Get episode mapping request", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn job_mapping_get(
        &self,
        Parameters(input): Parameters<JobIdInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        let value = self
            .state
            .search()
            .episode_mapping_action(owner, parse_job_id(&input.job_id)?)
            .await
            .map_err(search_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "media_job_mapping_resolve",
        description = "Apply an explicitly confirmed canonical Plex season and episode mapping to a job that is waiting for identity resolution.",
        output_schema = object_output_schema(),
        annotations(title = "Resolve episode mapping", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn job_mapping_resolve(
        &self,
        Parameters(input): Parameters<ResolveEpisodeInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        let job_id = parse_job_id(&input.job_id)?;
        let request = ResolveEpisodeMappingRequest {
            canonical_season: input.season,
            canonical_episode: input.episode,
            canonical_title: input.title,
        };
        let operation =
            stable_payload_operation_key(&format!("mapping-resolve:{job_id}"), &request)?;
        let value = self
            .state
            .search()
            .resolve_episode_mapping(owner, operation, job_id, request)
            .await
            .map_err(search_error)?;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "plex_search",
        description = "Search the shared Plex library. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Search Plex", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plex_search(
        &self,
        Parameters(input): Parameters<PlexSearchInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .plex_search(&actor, &input.query, input.limit)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "plex_recent",
        description = "List recently added Plex media. Read-only.",
        output_schema = output_schema::<PlexRecentOutput>(),
        annotations(title = "List recent Plex media", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plex_recent(
        &self,
        Parameters(input): Parameters<LimitInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let limit = validate_limit(input.limit)?;
        let mut value = self
            .state
            .admin()
            .plex_recent(&actor, limit)
            .await
            .map_err(admin_error)?;
        enrich_recent_card(&self.state, &actor, &mut value, input.rating_key).await;
        let items = plex_metadata(&value)
            .iter()
            .take(usize::from(limit))
            .filter_map(plex_recent_item)
            .collect::<Vec<_>>();
        result_json_for(
            &parts,
            PlexRecentOutput {
                returned: items.len(),
                items,
            },
        )
    }

    #[tool(
        name = "plex_library_summary",
        description = "Summarize configured Plex libraries and their item counts. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Summarize Plex libraries", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plex_library_summary(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .plex_library_summary(&actor)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "plex_library_items",
        description = "List one configured Plex library section. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "List Plex library items", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plex_library_items(
        &self,
        Parameters(input): Parameters<PlexLibraryInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let mut value = self
            .state
            .admin()
            .plex_library_items(&actor, input.section_key, input.start, input.limit)
            .await
            .map_err(admin_error)?;
        enrich_recent_card(&self.state, &actor, &mut value, input.rating_key).await;
        result_json_for(&parts, value)
    }

    #[tool(
        name = "plex_now_playing",
        description = "Show active Plex playback sessions. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Show Plex playback", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plex_now_playing(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .plex_now_playing(&actor)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "plex_item_get",
        description = "Get detailed Plex metadata for a rating key. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Get Plex item", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plex_item(
        &self,
        Parameters(input): Parameters<RatingKeyInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .plex_item(&actor, input.rating_key)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "plex_library_refresh",
        description = "Ask Plex to rescan a configured library section.",
        output_schema = object_output_schema(),
        annotations(title = "Refresh Plex library", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plex_refresh(
        &self,
        Parameters(input): Parameters<PlexRefreshInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .plex_refresh(&actor, input.section_key)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "qbittorrent_list",
        description = "List qBittorrent downloads with progress, speed, ETA, peers, and state. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "List torrents", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn torrent_list(
        &self,
        Parameters(input): Parameters<TorrentListInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .qbittorrent_list(&actor, input.filter.as_deref())
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "qbittorrent_details",
        description = "Get qBittorrent properties and files for an exact info hash. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Get torrent details", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn torrent_details(
        &self,
        Parameters(input): Parameters<TorrentInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .qbittorrent_details(&actor, &input.hash)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "qbittorrent_control",
        description = "Pause, resume, or recheck a torrent. Action must be pause, resume, or recheck.",
        output_schema = object_output_schema(),
        annotations(title = "Control torrent", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn torrent_control(
        &self,
        Parameters(input): Parameters<TorrentControlInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .qbittorrent_control(&actor, &input.hash, &input.action)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "media_file_inspect",
        description = "Inspect a file or directory only inside configured Plex and torrent roots. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Inspect media file", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn file_inspect(
        &self,
        Parameters(input): Parameters<FileInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .file_inspect(&actor, &input.path)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "media_infrastructure_status",
        description = "Check media-service, Plex, and qBittorrent application-level health without Docker access. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "Check media infrastructure", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn infrastructure_status(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .infrastructure_status(&actor)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "media_storage_status",
        description = "Show total, used, and available space for configured media roots. Read-only.",
        output_schema = output_schema::<StorageStatusOutput>(),
        annotations(title = "Show media storage", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn storage_status(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let value = self
            .state
            .admin()
            .storage_status(&actor)
            .await
            .map_err(admin_error)?;
        result_json_for(&parts, storage_status(value))
    }

    #[tool(
        name = "media_destructive_prepare",
        description = "Prepare and preview a destructive action. Supported actions: plex_delete, torrent_delete, file_quarantine. Does not execute it.",
        output_schema = object_output_schema(),
        annotations(title = "Preview destructive media action", read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn destructive_prepare(
        &self,
        Parameters(input): Parameters<DestructivePrepareInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .prepare_destructive(&actor, &input.action, &input.target, input.delete_files)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "media_destructive_confirm",
        description = "Execute exactly one previously previewed destructive action using its short-lived one-time confirmation token. Never call without explicit user confirmation.",
        output_schema = object_output_schema(),
        annotations(title = "Confirm destructive media action", read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn destructive_confirm(
        &self,
        Parameters(input): Parameters<DestructiveConfirmInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json_for(
            &parts,
            self.state
                .admin()
                .confirm_destructive(&actor, &input.confirmation_token)
                .await
                .map_err(admin_error)?,
        )
    }
}

pub(crate) fn routes(state: ApiState) -> Router<ApiState> {
    let session_manager = Arc::new(
        rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
    );
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_allowed_hosts([
            "media-service",
            "media-service:8080",
            "localhost",
            "localhost:8080",
            "127.0.0.1",
            "127.0.0.1:8080",
        ])
        .with_json_response(true);
    let service = StreamableHttpService::new(
        move || Ok(MediaAdminMcp::new(state.clone())),
        session_manager,
        config,
    );
    Router::new().route_service("/internal/mcp", service)
}

fn actor_from_parts(parts: &Parts) -> Result<Actor, ErrorData> {
    parts
        .extensions
        .get::<Actor>()
        .cloned()
        .ok_or_else(|| ErrorData::internal_error("authenticated actor is unavailable", None))
}

fn parse_job_id(value: &str) -> Result<JobId, ErrorData> {
    value
        .parse::<JobId>()
        .map_err(|_| ErrorData::invalid_params("job_id is invalid", None))
}

fn stable_operation_key(
    action: &str,
    identity: impl std::fmt::Display,
) -> media_core::OperationKey {
    use sha2::{Digest, Sha256};

    let digest: [u8; 32] = Sha256::digest(format!("mcp:{action}:{identity}").as_bytes()).into();
    media_core::OperationKey::from_bytes(digest)
}

fn unique_operation_key(
    action: &str,
    identity: impl std::fmt::Display,
) -> media_core::OperationKey {
    stable_operation_key(action, format!("{identity}:{}", uuid::Uuid::new_v4()))
}

fn stable_payload_operation_key<T: serde::Serialize>(
    action: &str,
    value: &T,
) -> Result<media_core::OperationKey, ErrorData> {
    let payload = serde_json::to_vec(value)
        .map_err(|_| ErrorData::internal_error("operation could not be serialized", None))?;
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"mcp:");
    digest.update(action.as_bytes());
    digest.update(b":");
    digest.update(payload);
    Ok(media_core::OperationKey::from_bytes(
        digest.finalize().into(),
    ))
}

fn stable_owner_payload_operation_key<T: serde::Serialize>(
    action: &str,
    owner: media_core::UserId,
    value: &T,
) -> Result<media_core::OperationKey, ErrorData> {
    let payload = serde_json::to_vec(value)
        .map_err(|_| ErrorData::internal_error("operation could not be serialized", None))?;
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"mcp:");
    digest.update(action.as_bytes());
    digest.update(b":owner:");
    digest.update(owner.as_uuid().as_bytes());
    digest.update(b":");
    digest.update(payload);
    Ok(media_core::OperationKey::from_bytes(
        digest.finalize().into(),
    ))
}

fn choice_set_download_operation_key<T: serde::Serialize>(
    owner: media_core::UserId,
    value: &T,
) -> Result<media_core::OperationKey, ErrorData> {
    stable_owner_payload_operation_key("choice-set-download", owner, value)
}

fn object_output_schema() -> Arc<rmcp::model::JsonObject> {
    rmcp::handler::server::tool::schema_for_type::<ObjectOutput>()
}

fn output_schema<T: schemars::JsonSchema + 'static>() -> Arc<rmcp::model::JsonObject> {
    rmcp::handler::server::tool::schema_for_type::<T>()
}

#[rmcp::tool_handler(router = Self::tool_router())]
impl rmcp::ServerHandler for MediaAdminMcp {
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, ErrorData> {
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28);
        let mut tools = Self::tool_router().list_all();
        for tool in &mut tools {
            compact_schema(&mut tool.input_schema);
            if let Some(schema) = &mut tool.output_schema {
                compact_schema(schema);
            }
        }
        Ok(rmcp::model::ListToolsResult {
            result_type: Some(rmcp::model::ResultType::COMPLETE),
            tools,
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(TOOL_SCHEMA_TTL_MS),
            cache_scope: supports_cache_hints.then_some(rmcp::model::CacheScope::Public),
        })
    }
}

fn compact_schema(schema: &mut Arc<rmcp::model::JsonObject>) {
    let schema = Arc::make_mut(schema);
    schema.remove("$schema");
    schema.remove("title");
    for value in schema.values_mut() {
        compact_schema_value(value);
    }
}

fn compact_schema_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if object.get("description").is_some_and(Value::is_string) {
                object.remove("description");
            }
            for value in object.values_mut() {
                compact_schema_value(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                compact_schema_value(value);
            }
        }
        _ => {}
    }
}

fn mcp_scope(owner: media_core::UserId) -> SearchScopeDto {
    SearchScopeDto {
        platform: "mcp".to_owned(),
        chat_id: owner.to_string(),
        thread_id: None,
    }
}

fn parse_tracking_id(value: &str) -> Result<TrackingId, ErrorData> {
    value
        .parse::<TrackingId>()
        .map_err(|_| ErrorData::invalid_params("tracking_id is invalid", None))
}

fn configured_tracking(state: &ApiState) -> Result<&media_core::TrackingApplication, ErrorData> {
    state
        .tracking()
        .ok_or_else(|| ErrorData::internal_error("tracking is not configured", None))
}

fn episode_dto(value: EpisodeInput) -> EpisodeSnapshotDto {
    EpisodeSnapshotDto {
        season: value.season,
        episode: value.episode,
    }
}

fn tracking_download_dto(value: TrackingDownloadInput) -> TrackingDownloadDto {
    TrackingDownloadDto {
        provider_media_ref: value.provider_media_ref,
        translation_id: value.translation_id,
        season: value.season,
    }
}

fn application_error(error: ApplicationError) -> ErrorData {
    match error {
        ApplicationError::Forbidden => ErrorData::invalid_request("operation is forbidden", None),
        ApplicationError::InvalidInput(_) => ErrorData::invalid_params("request is invalid", None),
        ApplicationError::NotFound => ErrorData::invalid_params("resource was not found", None),
        ApplicationError::Conflict => {
            ErrorData::invalid_request("operation conflicts with current state", None)
        }
        ApplicationError::Infrastructure => {
            ErrorData::internal_error("media service operation failed", None)
        }
    }
}

fn tracking_error(error: TrackingApplicationError) -> ErrorData {
    match error {
        TrackingApplicationError::Forbidden => {
            ErrorData::invalid_request("operation is forbidden", None)
        }
        TrackingApplicationError::InvalidInput(_) => {
            ErrorData::invalid_params("tracking request is invalid", None)
        }
        TrackingApplicationError::NotFound => {
            ErrorData::invalid_params("tracking subscription was not found", None)
        }
        TrackingApplicationError::Conflict => {
            ErrorData::invalid_request("operation conflicts with current state", None)
        }
        TrackingApplicationError::Infrastructure => {
            ErrorData::internal_error("tracking operation failed", None)
        }
    }
}

fn validate_limit(limit: u16) -> Result<u16, ErrorData> {
    if (1..=MAX_PAGE_LIMIT).contains(&limit) {
        Ok(limit)
    } else {
        Err(ErrorData::invalid_params(
            "limit must be between 1 and 50",
            None,
        ))
    }
}

fn page_bounds(
    total: usize,
    input: &PageInput,
) -> Result<(usize, usize, Option<String>), ErrorData> {
    let limit = usize::from(validate_limit(input.limit)?);
    let start = match input.cursor.as_deref() {
        None => 0,
        Some(cursor) => cursor
            .strip_prefix("v1:")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|offset| *offset <= total)
            .ok_or_else(|| ErrorData::invalid_params("cursor is invalid", None))?,
    };
    let end = start.saturating_add(limit).min(total);
    let next_cursor = (end < total).then(|| format!("v1:{end}"));
    Ok((start, end, next_cursor))
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn safe_poster_url(value: Option<String>) -> Option<String> {
    let value = value?;
    if value.is_empty() || value.len() > 2048 {
        return None;
    }
    let url = url::Url::parse(&value).ok()?;
    (url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none())
    .then_some(value)
}

fn job_list_item(value: Value, view: ReadView) -> JobListItemOutput {
    let card = !matches!(view, ReadView::Summary);
    let diagnostic = matches!(view, ReadView::Diagnostic);
    JobListItemOutput {
        id: json_string(&value, "id").unwrap_or_default(),
        provider: json_string(&value, "provider").unwrap_or_default(),
        state: json_string(&value, "state").unwrap_or_default(),
        title: json_string(&value, "title"),
        poster_url: card
            .then(|| safe_poster_url(json_string(&value, "poster_url")))
            .flatten(),
        media_kind: json_string(&value, "media_kind"),
        season: value.get("season").and_then(Value::as_u64),
        episode: value.get("episode").and_then(Value::as_u64),
        episode_count: value.get("episode_count").and_then(Value::as_u64),
        translation: card.then(|| json_string(&value, "translation")).flatten(),
        library_title: card.then(|| json_string(&value, "library_title")).flatten(),
        release_year: card
            .then(|| value.get("release_year").and_then(Value::as_i64))
            .flatten(),
        notify_scope: diagnostic
            .then(|| json_string(&value, "notify_scope"))
            .flatten(),
        needs_action_reason: diagnostic
            .then(|| json_string(&value, "needs_action_reason"))
            .flatten(),
        result_ref: diagnostic
            .then(|| json_string(&value, "result_ref"))
            .flatten(),
    }
}

fn serialized_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

fn tracking_list_item(
    value: media_contract::TrackingDto,
    view: ReadView,
) -> TrackingListItemOutput {
    let card = !matches!(view, ReadView::Summary);
    let diagnostic = matches!(view, ReadView::Diagnostic);
    let mut known_episodes = value
        .known_episodes
        .into_iter()
        .map(|item| EpisodeOutput {
            season: item.season,
            episode: item.episode,
        })
        .collect::<Vec<_>>();
    if !diagnostic {
        known_episodes = known_episodes
            .into_iter()
            .max_by_key(|item| (item.season, item.episode))
            .into_iter()
            .collect();
    }
    let awaiting = matches!(
        value.check_status,
        media_contract::TrackingCheckStatusDto::AwaitingSource
            | media_contract::TrackingCheckStatusDto::SourceError
            | media_contract::TrackingCheckStatusDto::ReleaseError
    );
    let show_pending = card || awaiting;
    TrackingListItemOutput {
        id: value.id.to_string(),
        title: value.title,
        provider: serialized_name(&value.provider),
        scope: serialized_name(&value.scope),
        state: serialized_name(&value.state),
        check_status: serialized_name(&value.check_status),
        known_episodes,
        poster_url: card.then_some(value.poster_url).flatten(),
        translation: card.then_some(value.translation),
        last_checked_at: card.then_some(value.last_checked_at).flatten(),
        next_check_at: card.then_some(value.next_check_at),
        release_identity: diagnostic
            .then(|| {
                value
                    .release_identity
                    .map(|item| TrackingReleaseIdentityOutput {
                        source: serialized_name(&item.source),
                        source_id: item.source_id,
                    })
            })
            .flatten(),
        download: card
            .then(|| {
                value.download.map(|item| TrackingDownloadOutput {
                    provider_media_ref: item.provider_media_ref,
                    translation_id: item.translation_id,
                    season: item.season,
                })
            })
            .flatten(),
        pending_episodes: if show_pending {
            value
                .pending_episodes
                .into_iter()
                .map(|item| EpisodeOutput {
                    season: item.season,
                    episode: item.episode,
                })
                .collect()
        } else {
            Vec::new()
        },
        pending_since: show_pending.then_some(value.pending_since).flatten(),
        pending_age_seconds: show_pending.then_some(value.pending_age_seconds).flatten(),
        last_error: show_pending.then_some(value.last_error).flatten(),
        status_reason: show_pending.then_some(value.status_reason).flatten(),
    }
}

fn value_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(parse_u64_value)
}

fn value_strings(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn plex_recent_item(value: &Value) -> Option<PlexRecentItemOutput> {
    Some(PlexRecentItemOutput {
        rating_key: plex_rating_key(value)?.to_string(),
        media_type: json_string(value, "type").unwrap_or_else(|| "unknown".to_owned()),
        title: json_string(value, "title").unwrap_or_else(|| "Untitled".to_owned()),
        grandparent_title: json_string(value, "grandparentTitle"),
        parent_title: json_string(value, "parentTitle"),
        parent_index: value_u64(value, "parentIndex"),
        index: value_u64(value, "index"),
        year: value_u64(value, "year"),
        added_at: value_u64(value, "addedAt"),
        thumb: json_string(value, "thumb"),
        summary: json_string(value, "summary"),
        tmdb_id: value_u64(value, "tmdb_id").or_else(|| plex_tmdb_id(value)),
        original_title: json_string(value, "original_title"),
        release_date: json_string(value, "release_date"),
        rating: value.get("rating").and_then(Value::as_f64),
        poster_url: json_string(value, "poster_url"),
        overview: json_string(value, "overview"),
        countries: value_strings(value, "countries"),
        genres: value_strings(value, "genres"),
        status: json_string(value, "status"),
        season_count: value_u64(value, "season_count"),
        episode_count: value_u64(value, "episode_count"),
        tmdb_url: json_string(value, "tmdb_url"),
        imdb_url: json_string(value, "imdb_url"),
        trailer_url: json_string(value, "trailer_url"),
    })
}

fn storage_status(value: Value) -> StorageStatusOutput {
    let roots = value
        .get("roots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|root| {
            Some(StorageRootOutput {
                path: json_string(root, "path")?,
                total_bytes: value_u64(root, "total_bytes")?,
                available_bytes: value_u64(root, "available_bytes")?,
                used_bytes: value_u64(root, "used_bytes")?,
                used_percent: value_u64(root, "used_percent")?,
            })
        })
        .collect();
    StorageStatusOutput { roots }
}

fn result_json_for<T: serde::Serialize>(
    parts: &Parts,
    value: T,
) -> Result<CallToolResult, ErrorData> {
    let value = serde_json::to_value(value)
        .map_err(|_| ErrorData::internal_error("result could not be serialized", None))?;
    let structured = match value {
        serde_json::Value::Object(_) => value,
        serde_json::Value::Array(items) => serde_json::json!({ "items": items }),
        value => serde_json::json!({ "value": value }),
    };
    let mut result = CallToolResult::structured(structured);
    if parts
        .headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok())
        == Some("2026-07-28")
    {
        result.content.clear();
    }
    Ok(result)
}

async fn enrich_recent_card(
    state: &ApiState,
    actor: &Actor,
    payload: &mut Value,
    requested_rating_key: Option<u64>,
) {
    let Some(items) = payload
        .pointer("/MediaContainer/Metadata")
        .and_then(Value::as_array)
    else {
        return;
    };
    let Some(index) = requested_rating_key
        .and_then(|rating_key| {
            items
                .iter()
                .position(|item| plex_rating_key(item) == Some(rating_key))
        })
        .or_else(|| (!items.is_empty()).then_some(0))
    else {
        return;
    };

    let item = items[index].clone();
    let media_type = if item.get("type").and_then(Value::as_str) == Some("movie") {
        TrendingMediaTypeDto::Movie
    } else {
        TrendingMediaTypeDto::Tv
    };
    let tmdb_id = match media_type {
        TrendingMediaTypeDto::Movie => plex_tmdb_id(&item),
        TrendingMediaTypeDto::Tv => {
            let parent_key = item
                .get("grandparentRatingKey")
                .or_else(|| item.get("parentRatingKey"))
                .and_then(parse_u64_value);
            if let Some(parent_key) = parent_key {
                state
                    .admin()
                    .plex_item(actor, parent_key)
                    .await
                    .ok()
                    .and_then(|parent| plex_metadata(&parent).first().and_then(plex_tmdb_id))
                    .or_else(|| plex_tmdb_id(&item))
            } else {
                plex_tmdb_id(&item)
            }
        }
    };
    let Some(tmdb_id) = tmdb_id else {
        return;
    };
    let Ok(details) = state.media_details().details(tmdb_id, media_type).await else {
        return;
    };
    let Ok(Value::Object(details)) = serde_json::to_value(details) else {
        return;
    };
    let Some(item) = payload
        .pointer_mut("/MediaContainer/Metadata")
        .and_then(Value::as_array_mut)
        .and_then(|items| items.get_mut(index))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    item.extend(details);
}

fn plex_metadata(payload: &Value) -> &[Value] {
    payload
        .pointer("/MediaContainer/Metadata")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn plex_rating_key(item: &Value) -> Option<u64> {
    item.get("ratingKey").and_then(parse_u64_value)
}

fn plex_tmdb_id(item: &Value) -> Option<u64> {
    item.get("Guid")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|guid| guid.get("id").and_then(Value::as_str))
        .find_map(|guid| guid.strip_prefix("tmdb://")?.parse().ok())
}

fn parse_u64_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

async fn enrich_job_value(state: &ApiState, result_ref: &str, mut value: Value) -> Value {
    let Ok(selection) = state.search().execution_for(result_ref).await else {
        return value;
    };
    let Some(object) = value.as_object_mut() else {
        return value;
    };

    match selection {
        ExecutionSelectionDto::Rezka {
            media_kind,
            translation,
            season,
            episode,
            episodes,
            release_year,
            library_title,
            thumbnail_url,
            title,
            ..
        } => {
            object.insert("title".to_owned(), Value::String(title));
            object.insert("media_kind".to_owned(), serde_json::json!(media_kind));
            if let Some(translation) = translation {
                object.insert("translation".to_owned(), Value::String(translation));
            }
            if let Some(release_year) = release_year {
                object.insert("release_year".to_owned(), serde_json::json!(release_year));
            }
            if let Some(library_title) = library_title {
                object.insert("library_title".to_owned(), Value::String(library_title));
            }
            insert_safe_poster(object, thumbnail_url);
            let derived_season = season.or_else(|| {
                let first = episodes.first()?.season;
                episodes
                    .iter()
                    .all(|item| item.season == first)
                    .then_some(first)
            });
            if let Some(season) = derived_season {
                object.insert("season".to_owned(), serde_json::json!(season));
            }
            if let Some(episode) = episode {
                object.insert("episode".to_owned(), serde_json::json!(episode));
            }
            if !episodes.is_empty() {
                object.insert(
                    "episode_count".to_owned(),
                    serde_json::json!(episodes.len()),
                );
            }
        }
        ExecutionSelectionDto::Prowlarr {
            media_kind,
            season,
            episode,
            library_title,
            tmdb_id,
            thumbnail_url,
            title,
            ..
        } => {
            object.insert("title".to_owned(), Value::String(title));
            object.insert("media_kind".to_owned(), serde_json::json!(media_kind));
            if let Some(library_title) = library_title {
                object.insert("library_title".to_owned(), Value::String(library_title));
            }
            if let Some(tmdb_id) = tmdb_id {
                object.insert("tmdb_id".to_owned(), serde_json::json!(tmdb_id));
            }
            insert_safe_poster(object, thumbnail_url);
            if let Some(season) = season {
                object.insert("season".to_owned(), serde_json::json!(season));
            }
            if let Some(episode) = episode {
                object.insert("episode".to_owned(), serde_json::json!(episode));
            }
        }
    }
    value
}

fn insert_safe_poster(object: &mut serde_json::Map<String, Value>, value: Option<String>) {
    if let Some(value) = safe_poster_url(value) {
        object.insert("poster_url".to_owned(), Value::String(value));
    }
}

fn admin_error(error: crate::MediaAdminError) -> ErrorData {
    match error {
        crate::MediaAdminError::InvalidRequest => {
            ErrorData::invalid_params("admin request is invalid", None)
        }
        crate::MediaAdminError::Forbidden => {
            ErrorData::invalid_request("admin operation is forbidden", None)
        }
        crate::MediaAdminError::NotFound => {
            ErrorData::invalid_params("admin resource was not found", None)
        }
        crate::MediaAdminError::InvalidConfirmation => {
            ErrorData::invalid_request("confirmation is invalid or expired", None)
        }
        crate::MediaAdminError::Unavailable | crate::MediaAdminError::Provider => {
            ErrorData::internal_error("media administration operation failed", None)
        }
    }
}

fn search_error_code(error: crate::SearchError) -> &'static str {
    match error {
        crate::SearchError::InvalidRequest => "invalid_request",
        crate::SearchError::Forbidden => "forbidden",
        crate::SearchError::NotFound => "not_found",
        crate::SearchError::Conflict => "conflict",
        crate::SearchError::Provider => "provider_failed",
        crate::SearchError::ProviderUnavailable => "provider_unavailable",
        crate::SearchError::VpnRotationRequired => "vpn_rotation_required",
        crate::SearchError::RezkaDiagnostic(diagnostic) => rezka_diagnostic_wire_label(diagnostic),
        crate::SearchError::Infrastructure => "infrastructure_failed",
    }
}

fn search_error(error: crate::SearchError) -> ErrorData {
    match error {
        crate::SearchError::InvalidRequest => {
            ErrorData::invalid_params("search request is invalid", None)
        }
        crate::SearchError::Forbidden => ErrorData::invalid_request("operation is forbidden", None),
        crate::SearchError::NotFound => {
            ErrorData::invalid_params("search resource was not found", None)
        }
        crate::SearchError::Conflict => {
            ErrorData::invalid_request("search operation conflicts with current state", None)
        }
        crate::SearchError::ProviderUnavailable => {
            ErrorData::internal_error("media provider is temporarily unavailable", None)
        }
        crate::SearchError::VpnRotationRequired => {
            ErrorData::internal_error("VPN rotation is required before another Rezka search", None)
        }
        crate::SearchError::RezkaDiagnostic(diagnostic) => {
            ErrorData::internal_error(rezka_diagnostic_wire_label(diagnostic), None)
        }
        crate::SearchError::Provider | crate::SearchError::Infrastructure => {
            ErrorData::internal_error("media search failed", None)
        }
    }
}

fn rezka_diagnostic_wire_label(
    diagnostic: media_contract::RezkaDiagnosticCategoryDto,
) -> &'static str {
    match diagnostic {
        media_contract::RezkaDiagnosticCategoryDto::RezkaReachable => "RezkaReachable",
        media_contract::RezkaDiagnosticCategoryDto::AnubisChallengeRequired => {
            "AnubisChallengeRequired"
        }
        media_contract::RezkaDiagnosticCategoryDto::AnubisChallengeFailed => {
            "AnubisChallengeFailed"
        }
        media_contract::RezkaDiagnosticCategoryDto::RezkaProviderRejected => {
            "RezkaProviderRejected"
        }
        media_contract::RezkaDiagnosticCategoryDto::RezkaParserInvalid => "RezkaParserInvalid",
        media_contract::RezkaDiagnosticCategoryDto::SessionStoreError => "SessionStoreError",
    }
}

fn release_error(error: ReleaseQueryError) -> ErrorData {
    match error {
        ReleaseQueryError::EmptyTitle
        | ReleaseQueryError::EmptyOriginalTitle
        | ReleaseQueryError::ZeroSourceId => {
            ErrorData::invalid_params("release query is invalid", None)
        }
        ReleaseQueryError::Provider => ErrorData::internal_error("release provider failed", None),
    }
}

fn trending_error(error: crate::TrendingServiceError) -> ErrorData {
    match error {
        crate::TrendingServiceError::InvalidRequest => {
            ErrorData::invalid_params("trending request is invalid", None)
        }
        crate::TrendingServiceError::Unavailable => {
            ErrorData::internal_error("trending integration is unavailable", None)
        }
        crate::TrendingServiceError::Provider => {
            ErrorData::internal_error("trending provider failed", None)
        }
    }
}

fn parse_media_type(value: &str) -> Result<TrendingMediaTypeDto, ErrorData> {
    match value {
        "movie" => Ok(TrendingMediaTypeDto::Movie),
        "tv" => Ok(TrendingMediaTypeDto::Tv),
        _ => Err(ErrorData::invalid_params(
            "media_type must be movie or tv",
            None,
        )),
    }
}

fn media_details_error(error: crate::MediaDetailsServiceError) -> ErrorData {
    match error {
        crate::MediaDetailsServiceError::InvalidRequest => {
            ErrorData::invalid_params("media details request is invalid", None)
        }
        crate::MediaDetailsServiceError::Unavailable => {
            ErrorData::internal_error("media details integration is unavailable", None)
        }
        crate::MediaDetailsServiceError::Provider => {
            ErrorData::internal_error("media details provider failed", None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PageInput, ReadView, choice_set_download_operation_key, job_list_item, page_bounds,
        parse_job_id, plex_recent_item, result_json_for, rezka_diagnostic_wire_label,
        search_error_code, tracking_list_item,
    };
    use axum::http::{HeaderValue, Request};

    #[test]
    fn rejects_invalid_job_ids_before_touching_storage() {
        assert!(parse_job_id("not-a-job").is_err());
    }

    #[test]
    fn rezka_diagnostics_keep_exact_safe_categories_at_the_mcp_boundary() {
        use media_contract::RezkaDiagnosticCategoryDto as Diagnostic;

        for (diagnostic, expected) in [
            (Diagnostic::RezkaReachable, "RezkaReachable"),
            (
                Diagnostic::AnubisChallengeRequired,
                "AnubisChallengeRequired",
            ),
            (Diagnostic::AnubisChallengeFailed, "AnubisChallengeFailed"),
            (Diagnostic::RezkaProviderRejected, "RezkaProviderRejected"),
            (Diagnostic::RezkaParserInvalid, "RezkaParserInvalid"),
            (Diagnostic::SessionStoreError, "SessionStoreError"),
        ] {
            assert_eq!(
                search_error_code(crate::SearchError::RezkaDiagnostic(diagnostic)),
                expected
            );
            assert_eq!(rezka_diagnostic_wire_label(diagnostic), expected);
        }
    }

    #[test]
    fn vpn_rotation_keeps_a_stable_search_error_code_at_the_mcp_boundary() {
        assert_eq!(
            search_error_code(crate::SearchError::VpnRotationRequired),
            "vpn_rotation_required"
        );
    }

    #[test]
    fn choice_set_download_idempotency_is_stable_per_owner() {
        let payload = serde_json::json!({
            "choice_set_id": "shared-family-choice",
            "source": "rezka",
            "result_id": "rezka:42",
            "translation_id": 7,
            "season": 1,
            "episode": 2,
        });
        let primary =
            choice_set_download_operation_key(media_core::PRIMARY_USER_ID, &payload).unwrap();
        let replay =
            choice_set_download_operation_key(media_core::PRIMARY_USER_ID, &payload).unwrap();
        let secondary =
            choice_set_download_operation_key(media_core::SECONDARY_USER_ID, &payload).unwrap();

        assert_eq!(primary, replay);
        assert_ne!(primary, secondary);
    }

    #[test]
    fn wraps_array_results_in_an_mcp_structured_content_object() {
        let parts = Request::new(()).into_parts().0;
        let result = result_json_for(&parts, Vec::<String>::new()).unwrap();

        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({ "items": [] }))
        );
    }

    #[test]
    fn modern_results_do_not_repeat_structured_json_as_text() {
        let mut parts = Request::new(()).into_parts().0;
        parts.headers.insert(
            "mcp-protocol-version",
            HeaderValue::from_static("2026-07-28"),
        );
        let result = result_json_for(&parts, serde_json::json!({ "queued": 2 })).unwrap();

        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({ "queued": 2 }))
        );
        assert!(result.content.is_empty());
    }

    #[test]
    fn page_limits_and_cursors_are_bounded() {
        let first = PageInput {
            limit: 10,
            cursor: None,
            view: ReadView::Summary,
        };
        assert_eq!(
            page_bounds(23, &first).unwrap(),
            (0, 10, Some("v1:10".into()))
        );
        let next = PageInput {
            limit: 10,
            cursor: Some("v1:10".into()),
            view: ReadView::Summary,
        };
        assert_eq!(
            page_bounds(23, &next).unwrap(),
            (10, 20, Some("v1:20".into()))
        );
        let oversized = PageInput {
            limit: 51,
            cursor: None,
            view: ReadView::Summary,
        };
        assert!(page_bounds(23, &oversized).is_err());
    }

    #[test]
    fn plex_recent_item_drops_large_provider_objects() {
        let item = serde_json::json!({
            "ratingKey": "42",
            "type": "episode",
            "title": "Episode title",
            "grandparentTitle": "Show title",
            "parentIndex": 2,
            "index": 7,
            "Media": [{"Part": [{"Stream": [{"raw": "x".repeat(50_000)}]}]}],
            "Guid": [{"id": "tmdb://123"}]
        });
        let compact = plex_recent_item(&item).unwrap();
        let serialized = serde_json::to_vec(&compact).unwrap();

        assert!(
            serialized.len() < 1_000,
            "compact item is {} bytes",
            serialized.len()
        );
        assert_eq!(compact.rating_key, "42");
        assert_eq!(compact.tmdb_id, Some(123));
    }

    #[test]
    fn job_cards_keep_only_safe_poster_urls() {
        let safe = job_list_item(
            serde_json::json!({
                "id": "job-1",
                "provider": "rezka",
                "state": "queued",
                "poster_url": "https://image.tmdb.org/t/p/w780/show.jpg"
            }),
            ReadView::Card,
        );
        assert_eq!(
            safe.poster_url.as_deref(),
            Some("https://image.tmdb.org/t/p/w780/show.jpg")
        );

        let unsafe_item = job_list_item(
            serde_json::json!({
                "id": "job-2",
                "provider": "rezka",
                "state": "queued",
                "poster_url": "https://user@example.test/show.jpg"
            }),
            ReadView::Card,
        );
        assert_eq!(unsafe_item.poster_url, None);
    }

    #[test]
    fn tracking_cards_keep_the_persisted_poster() {
        let value: media_contract::TrackingDto = serde_json::from_value(serde_json::json!({
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

        let card = tracking_list_item(value, ReadView::Card);
        assert_eq!(
            card.poster_url.as_deref(),
            Some("https://image.tmdb.org/t/p/w780/show.jpg")
        );
    }
}
