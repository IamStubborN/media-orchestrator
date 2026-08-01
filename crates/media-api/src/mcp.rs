use std::sync::Arc;

use axum::{Router, http::request::Parts};
use media_contract::{
    JobListDto, MediaKindDto, ProviderDto, SearchScopeDto, StartSearchRequest, TrackingListDto,
};
use media_core::{Actor, ApplicationError, JobId, TrackingApplicationError, TrackingId};
use rmcp::schemars;
use rmcp::{
    handler::server::{tool::Extension, wrapper::Parameters},
    model::{CallToolResult, ErrorData},
    tool, tool_router,
    transport::{StreamableHttpServerConfig, StreamableHttpService},
};
use serde::Deserialize;

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
    query: String,
    #[serde(default = "default_source")]
    source: String,
    media_kind: Option<String>,
    season: Option<u16>,
}

fn default_source() -> String {
    "all".to_owned()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LimitInput {
    #[serde(default = "default_limit")]
    limit: u16,
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
        description = "List the authenticated user's media jobs and their current states. Read-only."
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
        result_json(JobListDto {
            jobs: jobs.iter().map(convert::job).collect(),
        })
    }

    #[tool(
        name = "media_job_get",
        description = "Get sanitized details and measured progress for one of the authenticated user's media jobs. Read-only."
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
        result_json(convert::job_detail(&detail))
    }

    #[tool(
        name = "media_queue_status",
        description = "Show the authenticated user's queue and runner state. Read-only."
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
        description = "Cancel one of the authenticated user's media jobs. This changes state but does not delete files."
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
        description = "Retry one of the authenticated user's failed or partial media jobs."
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
            .retry_job(&actor, stable_operation_key("retry", job_id), job_id)
            .await
            .map_err(application_error)?;
        result_json(convert::job(&job))
    }

    #[tool(
        name = "media_tracking_list",
        description = "List the authenticated user's tracking subscriptions and their check state. Read-only."
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
        description = "Run an immediate check for one of the authenticated user's tracking subscriptions."
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
        name = "media_search",
        description = "Search Rezka, Prowlarr, or both. Returns separate provider results and never downloads automatically."
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
                scope: SearchScopeDto {
                    platform: "mcp".to_owned(),
                    chat_id: owner.to_string(),
                    thread_id: None,
                },
                source: *provider,
                query: input.query.clone(),
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
        name = "plex_search",
        description = "Search the shared Plex library. Read-only."
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
        description = "List recently added Plex media. Read-only."
    )]
    async fn plex_recent(
        &self,
        Parameters(input): Parameters<LimitInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = actor_from_parts(&parts)?;
        result_json(
            self.state
                .admin()
                .plex_recent(&actor, input.limit)
                .await
                .map_err(admin_error)?,
        )
    }

    #[tool(
        name = "plex_now_playing",
        description = "Show active Plex playback sessions. Read-only."
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
        description = "Get detailed Plex metadata for a rating key. Read-only."
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
        description = "Ask Plex to rescan an allowlisted library section. Primary administrator only."
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
        description = "List qBittorrent downloads with progress, speed, ETA, peers, and state. Read-only."
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
        description = "Get qBittorrent properties and files for an exact info hash. Read-only."
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
        description = "Pause, resume, or recheck a torrent. Primary administrator only; action must be pause, resume, or recheck."
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
        description = "Inspect a file or directory only inside configured Plex and torrent roots. Read-only."
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
        description = "Check media-service, Plex, and qBittorrent application-level health without Docker access. Read-only."
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
        name = "media_destructive_prepare",
        description = "Prepare and preview an administrator-only destructive action. Supported actions: plex_delete, torrent_delete, file_quarantine. Does not execute it."
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
        description = "Execute exactly one previously previewed destructive action using its short-lived one-time confirmation token. Never call without explicit user confirmation."
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

fn stable_operation_key(action: &str, job_id: JobId) -> media_core::OperationKey {
    use sha2::{Digest, Sha256};

    let digest: [u8; 32] = Sha256::digest(format!("mcp:{action}:{job_id}").as_bytes()).into();
    media_core::OperationKey::from_bytes(digest)
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
        crate::SearchError::Infrastructure => "infrastructure_failed",
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
