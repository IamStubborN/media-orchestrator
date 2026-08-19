#![forbid(unsafe_code)]

use std::time::Duration;

use media_contract::{
    AlternativeSearchRequest, ContinueSearchRequest, CreateJobRequest, CreateTrackingRequest,
    EpisodeMappingActionDto, EpisodeSnapshotDto, JobDetailDto, JobDto, JobListDto, MediaDetailsDto,
    NotifyScopeDto, PatchTrackingRequest, ProviderDto, QueueStatusDto, ReleaseQueryRequest,
    ReleaseQueryResponse, ResolveEpisodeMappingRequest, SearchPageDto, SearchScopeDto,
    SelectResultRequest, SetTrackingBaselineRequest, SimilarPageDto, StartSearchRequest,
    TrackingDto, TrackingListDto, TrendingCategoryDto, TrendingMediaTypeDto, TrendingPageDto,
};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;

const REQUEST_ID_HEADER: &str = "x-request-id";
const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

pub struct MediaClient {
    client: reqwest::Client,
    service_url: reqwest::Url,
    token: SecretString,
}

pub trait IntoClientParts {
    fn into_client_parts(self) -> (reqwest::Url, SecretString);
}

pub trait JsonResponse {
    fn to_json_value(&self) -> serde_json::Value;
}

impl<T: serde::Serialize> JsonResponse for T {
    fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("typed media responses are serializable")
    }
}

impl IntoClientParts for (reqwest::Url, SecretString) {
    fn into_client_parts(self) -> (reqwest::Url, SecretString) {
        self
    }
}

impl MediaClient {
    pub fn new(config: impl IntoClientParts) -> Result<Self, ClientError> {
        let (service_url, token) = config.into_client_parts();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| ClientError::Configuration)?;
        Ok(Self {
            client,
            service_url,
            token,
        })
    }

    pub async fn create_job(
        &self,
        provider: ProviderDto,
        result_ref: String,
    ) -> Result<JobDto, ClientError> {
        self.post_idempotent(
            "v1/jobs",
            &CreateJobRequest {
                provider,
                result_ref,
                notify_scope: NotifyScopeDto::Initiator,
            },
        )
        .await
    }

    pub async fn get_job(&self, job_id: &str) -> Result<JobDetailDto, ClientError> {
        self.get(&format!("v1/jobs/{job_id}")).await
    }

    pub async fn list_jobs(&self) -> Result<JobListDto, ClientError> {
        self.get("v1/jobs").await
    }

    pub async fn cancel_job(&self, job_id: &str) -> Result<JobDto, ClientError> {
        self.post_idempotent(&format!("v1/jobs/{job_id}/cancel"), &serde_json::json!({}))
            .await
    }

    pub async fn retry_job(&self, job_id: &str) -> Result<JobDto, ClientError> {
        self.post_idempotent(&format!("v1/jobs/{job_id}/retry"), &serde_json::json!({}))
            .await
    }

    pub async fn alternative_search(
        &self,
        job_id: &str,
        scope: SearchScopeDto,
    ) -> Result<SearchPageDto, ClientError> {
        self.execute(
            self.request(
                reqwest::Method::POST,
                &format!("v1/jobs/{job_id}/alternative-search"),
            )?
            .timeout(Duration::from_secs(150))
            .json(&AlternativeSearchRequest { scope }),
        )
        .await
    }

    pub async fn episode_mapping_action(
        &self,
        job_id: &str,
    ) -> Result<EpisodeMappingActionDto, ClientError> {
        self.get(&format!("v1/jobs/{job_id}/episode-mapping-action"))
            .await
    }

    pub async fn resolve_episode_mapping(
        &self,
        job_id: &str,
        canonical_season: u32,
        canonical_episode: u32,
        canonical_title: Option<String>,
    ) -> Result<JobDto, ClientError> {
        self.post_idempotent(
            &format!("v1/jobs/{job_id}/episode-mapping-action"),
            &ResolveEpisodeMappingRequest {
                canonical_season,
                canonical_episode,
                canonical_title,
            },
        )
        .await
    }

    pub async fn queue_status(&self) -> Result<QueueStatusDto, ClientError> {
        self.get("v1/queue/status").await
    }

    pub async fn query_release(
        &self,
        request: ReleaseQueryRequest,
    ) -> Result<ReleaseQueryResponse, ClientError> {
        self.post("v1/releases/query", &request).await
    }

    pub async fn trending(
        &self,
        category: TrendingCategoryDto,
        page: u32,
    ) -> Result<TrendingPageDto, ClientError> {
        let category = match category {
            TrendingCategoryDto::All => "all",
            TrendingCategoryDto::Movie => "movie",
            TrendingCategoryDto::Tv => "tv",
        };
        self.get(&format!("v1/trending?category={category}&page={page}"))
            .await
    }

    pub async fn media_details(
        &self,
        tmdb_id: u64,
        media_type: TrendingMediaTypeDto,
    ) -> Result<MediaDetailsDto, ClientError> {
        self.get(&format!(
            "v1/media/details?tmdb_id={tmdb_id}&media_type={}",
            media_type_path(media_type)
        ))
        .await
    }

    pub async fn media_similar(
        &self,
        tmdb_id: u64,
        media_type: TrendingMediaTypeDto,
        page: u32,
    ) -> Result<SimilarPageDto, ClientError> {
        self.get(&format!(
            "v1/media/similar?tmdb_id={tmdb_id}&media_type={}&page={page}",
            media_type_path(media_type)
        ))
        .await
    }

    pub async fn add_tracking(
        &self,
        request: CreateTrackingRequest,
    ) -> Result<TrackingDto, ClientError> {
        self.post_idempotent("v1/tracking", &request).await
    }

    pub async fn search(&self, request: StartSearchRequest) -> Result<SearchPageDto, ClientError> {
        self.execute(
            self.request(reqwest::Method::POST, "v1/searches")?
                .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
                .timeout(Duration::from_secs(150))
                .json(&request),
        )
        .await
    }

    pub async fn list_tracking(&self) -> Result<TrackingListDto, ClientError> {
        self.get("v1/tracking").await
    }

    pub async fn patch_tracking(
        &self,
        tracking_id: &str,
        request: PatchTrackingRequest,
    ) -> Result<TrackingDto, ClientError> {
        self.execute(
            self.request(
                reqwest::Method::PATCH,
                &format!("v1/tracking/{tracking_id}"),
            )?
            .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
            .json(&request),
        )
        .await
    }

    pub async fn remove_tracking(&self, tracking_id: &str) -> Result<TrackingDto, ClientError> {
        self.execute(
            self.request(
                reqwest::Method::DELETE,
                &format!("v1/tracking/{tracking_id}"),
            )?
            .header(IDEMPOTENCY_KEY_HEADER, generated_identifier()),
        )
        .await
    }

    pub async fn set_tracking_baseline(
        &self,
        tracking_id: &str,
        known_through: EpisodeSnapshotDto,
    ) -> Result<TrackingDto, ClientError> {
        self.post_idempotent(
            &format!("v1/tracking/{tracking_id}/baseline"),
            &SetTrackingBaselineRequest { known_through },
        )
        .await
    }

    pub async fn check_tracking_now(&self, tracking_id: &str) -> Result<TrackingDto, ClientError> {
        self.execute(
            self.request(
                reqwest::Method::POST,
                &format!("v1/tracking/{tracking_id}/check"),
            )?
            .header(IDEMPOTENCY_KEY_HEADER, generated_identifier()),
        )
        .await
    }

    pub async fn continue_search(
        &self,
        continuation: String,
        scope: SearchScopeDto,
    ) -> Result<SearchPageDto, ClientError> {
        self.execute(
            self.request(reqwest::Method::POST, "v1/searches/continue")?
                .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
                .timeout(Duration::from_secs(150))
                .json(&ContinueSearchRequest {
                    continuation,
                    scope,
                }),
        )
        .await
    }

    pub async fn select(&self, request: SelectResultRequest) -> Result<JobDto, ClientError> {
        self.post_idempotent("v1/selections", &request).await
    }

    pub async fn refresh_rezka_session(
        &self,
        credential_request_id: String,
    ) -> Result<JobDto, ClientError> {
        self.post_idempotent(
            "v1/rezka/session/refresh",
            &media_contract::RezkaSessionRefreshRequest {
                credential_request_id,
            },
        )
        .await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        self.execute(self.request(reqwest::Method::GET, path)?)
            .await
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, ClientError> {
        self.execute(self.request(reqwest::Method::POST, path)?.json(body))
            .await
    }

    async fn post_idempotent<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, ClientError> {
        self.execute(
            self.request(reqwest::Method::POST, path)?
                .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
                .json(body),
        )
        .await
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, ClientError> {
        let endpoint = self
            .service_url
            .join(path)
            .map_err(|_| ClientError::Configuration)?;
        Ok(self
            .client
            .request(method, endpoint)
            .bearer_auth(self.token.expose_secret())
            .header(REQUEST_ID_HEADER, generated_identifier()))
    }

    async fn execute<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ClientError> {
        let response = request.send().await.map_err(classify_request_error)?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| ClientError::ResponseRead)?;
        let body = String::from_utf8(bytes.to_vec()).map_err(|_| ClientError::ResponseEncoding)?;
        if !status.is_success() {
            return Err(ClientError::Http { status, body });
        }
        serde_json::from_str(&body).map_err(|_| ClientError::ResponseJson)
    }
}

impl std::fmt::Debug for MediaClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaClient")
            .field("client", &"[REDACTED]")
            .field("service_url", &"[REDACTED]")
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("HTTP client configuration failed")]
    Configuration,
    #[error("HTTP request timed out")]
    Timeout,
    #[error("could not connect to the media service")]
    Connect,
    #[error("HTTP request failed before a response was received")]
    Request,
    #[error("could not read the media service response")]
    ResponseRead,
    #[error("media service response was not valid UTF-8")]
    ResponseEncoding,
    #[error("successful media service response did not match the typed contract")]
    ResponseJson,
    #[error("HTTP {status}: {body}")]
    Http {
        status: reqwest::StatusCode,
        body: String,
    },
}

fn classify_request_error(error: reqwest::Error) -> ClientError {
    if error.is_timeout() {
        ClientError::Timeout
    } else if error.is_connect() {
        ClientError::Connect
    } else {
        ClientError::Request
    }
}

fn generated_identifier() -> String {
    uuid::Uuid::new_v4().to_string()
}

const fn media_type_path(media_type: TrendingMediaTypeDto) -> &'static str {
    match media_type {
        TrendingMediaTypeDto::Movie => "movie",
        TrendingMediaTypeDto::Tv => "tv",
    }
}
