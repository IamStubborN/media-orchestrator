use std::sync::Arc;

use axum::{Router, http::request::Parts};
use media_contract::{
    AlternativeSearchRequest, ContinueSearchRequest, CreateTrackingRequest, EpisodeSnapshotDto,
    ExecutionSelectionDto, MediaKindDto, PatchTrackingRequest, ProviderDto, ReleaseQueryRequest,
    ResolveEpisodeMappingRequest, RezkaSessionRefreshRequest, SearchScopeDto, SelectResultRequest,
    StartSearchRequest, TrackingDownloadDto, TrackingListDto, TrackingReleaseIdentityDto,
    TrackingReleaseSourceDto, TrackingScopeDto, TrendingCategoryDto, TrendingMediaTypeDto,
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
use serde::Deserialize;
use serde_json::Value;

use crate::{ApiState, convert};

#[derive(Clone)]
struct MediaAdminMcp {
    state: ApiState,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct JobIdInput {
    #[schemars(description = "Public media job ID")]
    job_id: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct TrackingIdInput {
    #[schemars(description = "Public tracking subscription ID")]
    tracking_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchInput {
    query: Option<String>,
    continuation: Option<String>,
    #[serde(default = "default_source")]
    source: String,
    media_kind: Option<String>,
    season: Option<u16>,
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
struct RezkaSessionRefreshInput {
    #[schemars(description = "One-time approved Vaultwarden credential request ID")]
    credential_request_id: String,
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
    page: u32,
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
    season: u32,
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TrackingCreateInput {
    #[serde(default = "default_tracking_provider")]
    provider: String,
    title: String,
    #[serde(default = "default_tracking_translation")]
    translation: String,
    known_episodes: Vec<EpisodeInput>,
    scope: String,
    #[serde(default = "default_true")]
    series_ongoing: bool,
    release_identity: Option<TrackingReleaseIdentityInput>,
    download: Option<TrackingDownloadInput>,
}

fn default_tracking_provider() -> String {
    "rezka".to_owned()
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

fn default_source() -> String {
    "all".to_owned()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LimitInput {
    #[serde(default = "default_limit")]
    limit: u16,
    #[schemars(description = "Optional Plex rating key to enrich with TMDB card metadata")]
    rating_key: Option<u64>,
}
fn default_limit() -> u16 {
    10
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

#[tool_router(server_handler)]
impl MediaAdminMcp {
    fn new(state: ApiState) -> Self {
        Self { state }
    }

    #[tool(
        name = "media_jobs_list",
        description = "List the authenticated user's media jobs and their current states. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "List media jobs", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_jobs(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let jobs = self
            .state
            .jobs()
            .list_jobs(&actor)
            .await
            .map_err(application_error)?;
        let jobs = futures_util::future::join_all(jobs.iter().map(|job| async {
            let value =
                serde_json::to_value(convert::job(job)).expect("job DTOs serialize to JSON");
            enrich_job_value(&self.state, job.result_ref(), value).await
        }))
        .await;
        result_json(serde_json::json!({"jobs": jobs}))
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
        result_json(enrich_job_value(&self.state, &result_ref, value).await)
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
        result_json(convert::queue_status(status))
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
            .cancel_job(&actor, stable_operation_key("cancel", job_id), job_id)
            .await
            .map_err(application_error)?;
        result_json(convert::job(&job))
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
            .retry_job(&actor, unique_operation_key("retry", job_id), job_id)
            .await
            .map_err(application_error)?;
        result_json(convert::job(&job))
    }

    #[tool(
        name = "media_tracking_list",
        description = "List the authenticated user's tracking subscriptions and their check state. Read-only.",
        output_schema = object_output_schema(),
        annotations(title = "List media tracking", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_tracking(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let tracking = self
            .state
            .tracking()
            .ok_or_else(|| ErrorData::internal_error("tracking is not configured", None))?;
        let values = tracking.list(&actor).await.map_err(tracking_error)?;
        result_json(TrackingListDto {
            tracking: values.iter().map(convert::tracking).collect(),
        })
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
        result_json(convert::tracking(&value))
    }

    #[tool(
        name = "media_tracking_create",
        description = "Create a personal or family release tracking subscription. Optionally enables the explicitly selected Rezka translation for future automatic downloads.",
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
        let provider = parse_provider(&input.provider)?;
        let scope = parse_tracking_scope(&input.scope)?;
        let request = CreateTrackingRequest {
            provider,
            title: input.title,
            translation: input.translation,
            known_episodes: input.known_episodes.into_iter().map(episode_dto).collect(),
            scope,
            series_ongoing: input.series_ongoing,
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
        let operation = stable_payload_operation_key("tracking-create", &request)?;
        let command = convert::new_tracking_command(request)
            .map_err(|_| ErrorData::invalid_params("tracking request is invalid", None))?;
        let value = tracking
            .add(&actor, operation, command)
            .await
            .map_err(tracking_error)?;
        result_json(convert::tracking(&value))
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
        result_json(convert::tracking(&value))
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
        result_json(convert::tracking(&value))
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
        result_json(value)
    }

    #[tool(
        name = "media_search",
        description = "Search Rezka, Prowlarr, or both, or continue one provider page with a continuation token. Returns separate provider results and never downloads automatically.",
        output_schema = object_output_schema(),
        annotations(title = "Search media providers", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
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
            return result_json(page);
        }
        let query = input
            .query
            .ok_or_else(|| ErrorData::invalid_params("query or continuation is required", None))?;
        let media_kind = match input.media_kind.as_deref() {
            Some("movie") => Some(MediaKindDto::Movie),
            Some("series") => Some(MediaKindDto::Series),
            Some(_) => {
                return Err(ErrorData::invalid_params(
                    "media_kind must be movie or series",
                    None,
                ));
            }
            None => None,
        };
        let providers: &[ProviderDto] = match input.source.as_str() {
            "all" => &[ProviderDto::Rezka, ProviderDto::Prowlarr],
            "rezka" => &[ProviderDto::Rezka],
            "prowlarr" => &[ProviderDto::Prowlarr],
            _ => {
                return Err(ErrorData::invalid_params(
                    "source must be all, rezka, or prowlarr",
                    None,
                ));
            }
        };
        let mut results = serde_json::Map::new();
        for provider in providers {
            let request = StartSearchRequest {
                scope: mcp_scope(owner),
                source: *provider,
                query: query.clone(),
                media_kind,
                season: input.season,
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
        result_json(serde_json::Value::Object(results))
    }

    #[tool(
        name = "media_download",
        description = "Create a download from one exact result in a previous media_search response. Never chooses a provider, result, translation, season, or episode implicitly.",
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
        result_json(value)
    }

    #[tool(
        name = "media_rezka_session_refresh",
        description = "Queue a Rezka session refresh from one approved credential request. The credential is resolved and consumed only by download-runner and is never returned to the caller.",
        output_schema = object_output_schema(),
        annotations(title = "Refresh Rezka session", read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn refresh_rezka_session(
        &self,
        Parameters(input): Parameters<RezkaSessionRefreshInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let owner = actor
            .require_user()
            .map_err(|_| ErrorData::invalid_request("operation is forbidden", None))?;
        let request = RezkaSessionRefreshRequest {
            credential_request_id: input.credential_request_id,
        };
        let operation = stable_payload_operation_key("rezka-session-refresh", &request)?;
        let value = self
            .state
            .search()
            .refresh_rezka_session(owner, operation, request)
            .await
            .map_err(search_error)?;
        result_json(value)
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
        result_json(convert::release_result(value))
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
        result_json(value)
    }

    #[tool(
        name = "media_details",
        description = "Get read-only localized and original TMDB metadata for one movie or series, including poster, overview, genres, countries, status, provider URLs, TV episode counts, and the next scheduled episode when available.",
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
        result_json(value)
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
        result_json(value)
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
        result_json(value)
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
        result_json(value)
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
        result_json(value)
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
        result_json(
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
        output_schema = object_output_schema(),
        annotations(title = "List recent Plex media", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plex_recent(
        &self,
        Parameters(input): Parameters<LimitInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        let mut value = self
            .state
            .admin()
            .plex_recent(&actor, input.limit)
            .await
            .map_err(admin_error)?;
        enrich_recent_card(&self.state, &actor, &mut value, input.rating_key).await;
        result_json(value)
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
        result_json(
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
        result_json(value)
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
        result_json(
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
        result_json(
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
        result_json(
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
        result_json(
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
        result_json(
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
        result_json(
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
        result_json(
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
        result_json(
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
        output_schema = object_output_schema(),
        annotations(title = "Show media storage", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn storage_status(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json(
            self.state
                .admin()
                .storage_status(&actor)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "media_destructive_prepare",
        description = "Prepare and preview a destructive action. Supported actions: plex_delete, torrent_delete, file_quarantine. Does not execute it.",
        output_schema = object_output_schema(),
        annotations(title = "Preview destructive media action", read_only_hint = true, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn destructive_prepare(
        &self,
        Parameters(input): Parameters<DestructivePrepareInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json(
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
        result_json(
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

fn object_output_schema() -> Arc<rmcp::model::JsonObject> {
    rmcp::handler::server::tool::schema_for_type::<ObjectOutput>()
}

fn mcp_scope(owner: media_core::UserId) -> SearchScopeDto {
    SearchScopeDto {
        platform: "mcp".to_owned(),
        chat_id: owner.to_string(),
        thread_id: None,
    }
}

fn parse_provider(value: &str) -> Result<ProviderDto, ErrorData> {
    match value {
        "rezka" => Ok(ProviderDto::Rezka),
        "prowlarr" => Ok(ProviderDto::Prowlarr),
        _ => Err(ErrorData::invalid_params(
            "provider must be rezka or prowlarr",
            None,
        )),
    }
}

fn parse_tracking_scope(value: &str) -> Result<TrackingScopeDto, ErrorData> {
    match value {
        "personal" => Ok(TrackingScopeDto::Personal),
        "family" => Ok(TrackingScopeDto::Family),
        _ => Err(ErrorData::invalid_params(
            "scope must be personal or family",
            None,
        )),
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

fn result_json<T: serde::Serialize>(value: T) -> Result<CallToolResult, ErrorData> {
    let value = serde_json::to_value(value)
        .map_err(|_| ErrorData::internal_error("result could not be serialized", None))?;
    let structured = match value {
        serde_json::Value::Object(_) => value,
        serde_json::Value::Array(items) => serde_json::json!({ "items": items }),
        value => serde_json::json!({ "value": value }),
    };
    Ok(CallToolResult::structured(structured))
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
        ExecutionSelectionDto::RezkaSessionRefresh { .. } => {}
        ExecutionSelectionDto::Rezka {
            media_kind,
            translation,
            season,
            episode,
            episodes,
            release_year,
            library_title,
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
            title,
            ..
        } => {
            object.insert("title".to_owned(), Value::String(title));
            object.insert("media_kind".to_owned(), serde_json::json!(media_kind));
            if let Some(library_title) = library_title {
                object.insert("library_title".to_owned(), Value::String(library_title));
            }
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
        crate::SearchError::Provider | crate::SearchError::Infrastructure => {
            ErrorData::internal_error("media search failed", None)
        }
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
    use super::{parse_job_id, result_json};

    #[test]
    fn rejects_invalid_job_ids_before_touching_storage() {
        assert!(parse_job_id("not-a-job").is_err());
    }

    #[test]
    fn wraps_array_results_in_an_mcp_structured_content_object() {
        let result = result_json(Vec::<String>::new()).unwrap();

        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({ "items": [] }))
        );
    }
}
