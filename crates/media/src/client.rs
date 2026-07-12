use std::time::Duration;

use media_contract::{
    ContinueSearchRequest, CreateJobRequest, NotifyScopeDto, ProviderDto, SelectResultRequest,
    StartSearchRequest,
};
use secrecy::{ExposeSecret, SecretString};

use crate::config::ClientConfig;

const REQUEST_ID_HEADER: &str = "x-request-id";
const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

pub struct HttpClient {
    client: reqwest::Client,
    service_url: reqwest::Url,
    token: SecretString,
}

impl HttpClient {
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        let (service_url, token) = config.into_parts();
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
    ) -> Result<String, ClientError> {
        let request = CreateJobRequest {
            provider,
            result_ref,
            notify_scope: NotifyScopeDto::Initiator,
        };
        self.execute(
            self.request(reqwest::Method::POST, "v1/jobs")?
                .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
                .json(&request),
        )
        .await
    }

    pub async fn get_job(&self, job_id: &str) -> Result<String, ClientError> {
        let path = format!("v1/jobs/{job_id}");
        self.execute(self.request(reqwest::Method::GET, &path)?)
            .await
    }

    pub async fn list_jobs(&self) -> Result<String, ClientError> {
        self.execute(self.request(reqwest::Method::GET, "v1/jobs")?)
            .await
    }

    pub async fn cancel_job(&self, job_id: &str) -> Result<String, ClientError> {
        let path = format!("v1/jobs/{job_id}/cancel");
        self.execute(
            self.request(reqwest::Method::POST, &path)?
                .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
                .json(&serde_json::json!({})),
        )
        .await
    }

    pub async fn queue_status(&self) -> Result<String, ClientError> {
        self.execute(self.request(reqwest::Method::GET, "v1/queue/status")?)
            .await
    }

    pub async fn search(&self, request: StartSearchRequest) -> Result<String, ClientError> {
        self.execute(
            self.request(reqwest::Method::POST, "v1/searches")?
                .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
                .json(&request),
        )
        .await
    }

    pub async fn continue_search(&self, continuation: String) -> Result<String, ClientError> {
        self.execute(
            self.request(reqwest::Method::POST, "v1/searches/continue")?
                .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
                .json(&ContinueSearchRequest { continuation }),
        )
        .await
    }

    pub async fn select(&self, request: SelectResultRequest) -> Result<String, ClientError> {
        self.execute(
            self.request(reqwest::Method::POST, "v1/selections")?
                .header(IDEMPOTENCY_KEY_HEADER, generated_identifier())
                .json(&request),
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

    async fn execute(&self, request: reqwest::RequestBuilder) -> Result<String, ClientError> {
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
        let value = serde_json::from_str::<serde_json::Value>(&body)
            .map_err(|_| ClientError::ResponseJson)?;
        serde_json::to_string(&value).map_err(|_| ClientError::ResponseJson)
    }
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpClient")
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
    #[error("successful media service response was not valid JSON")]
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
