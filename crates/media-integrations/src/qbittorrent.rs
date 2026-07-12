use std::{
    collections::HashMap,
    fmt,
    path::{Component, PathBuf},
    time::Duration,
};

use reqwest::{
    StatusCode,
    header::{COOKIE, LOCATION, REFERER, SET_COOKIE},
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

const MAX_TORRENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_REDIRECTS: u8 = 5;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum QbittorrentErrorCode {
    Configuration,
    InvalidSelection,
    Transport,
    Unauthorized,
    ProviderResponse,
    TorrentNotFound,
    IdentityMismatch,
}

#[derive(Debug, thiserror::Error)]
pub enum QbittorrentError {
    #[error("qBittorrent configuration is invalid: {message}")]
    Configuration { message: &'static str },
    #[error("torrent selection is invalid: {message}")]
    InvalidSelection { message: &'static str },
    #[error("qBittorrent request failed")]
    Transport,
    #[error("qBittorrent authentication failed")]
    Unauthorized,
    #[error("qBittorrent returned an invalid response ({status})")]
    ProviderResponse { status: StatusCode },
    #[error("qBittorrent torrent was not found")]
    TorrentNotFound,
    #[error("qBittorrent torrent identity did not match")]
    IdentityMismatch,
}

impl QbittorrentError {
    #[must_use]
    pub const fn code(&self) -> QbittorrentErrorCode {
        match self {
            Self::Configuration { .. } => QbittorrentErrorCode::Configuration,
            Self::InvalidSelection { .. } => QbittorrentErrorCode::InvalidSelection,
            Self::Transport => QbittorrentErrorCode::Transport,
            Self::Unauthorized => QbittorrentErrorCode::Unauthorized,
            Self::ProviderResponse { .. } => QbittorrentErrorCode::ProviderResponse,
            Self::TorrentNotFound => QbittorrentErrorCode::TorrentNotFound,
            Self::IdentityMismatch => QbittorrentErrorCode::IdentityMismatch,
        }
    }
}

pub struct QbittorrentConfig {
    base_url: Url,
    category: String,
    username: String,
    password: SecretString,
    timeout: Duration,
}

impl QbittorrentConfig {
    pub fn new(
        mut base_url: Url,
        category: impl Into<String>,
        username: impl Into<String>,
        password: SecretString,
        timeout: Duration,
    ) -> Result<Self, QbittorrentError> {
        let category = category.into();
        let username = username.into();
        if base_url.cannot_be_a_base() || base_url.host_str().is_none() {
            return Err(QbittorrentError::Configuration {
                message: "base URL must be absolute",
            });
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            return Err(QbittorrentError::Configuration {
                message: "base URL must not contain credentials",
            });
        }
        if base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(QbittorrentError::Configuration {
                message: "base URL must not contain a query or fragment",
            });
        }
        if category.trim().is_empty() {
            return Err(QbittorrentError::Configuration {
                message: "existing category must not be empty",
            });
        }
        if username.trim().is_empty() || password.expose_secret().is_empty() {
            return Err(QbittorrentError::Configuration {
                message: "credentials must not be empty",
            });
        }
        if timeout.is_zero() {
            return Err(QbittorrentError::Configuration {
                message: "request timeout must be positive",
            });
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            base_url,
            category,
            username,
            password,
            timeout,
        })
    }
}

impl fmt::Debug for QbittorrentConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QbittorrentConfig")
            .field("base_url", &self.base_url)
            .field("category", &self.category)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

pub struct ExplicitTorrentSelection {
    source_identity: String,
    info_hash: String,
    uri: Url,
}

impl ExplicitTorrentSelection {
    pub fn new(
        source_identity: impl Into<String>,
        info_hash: impl Into<String>,
        uri: impl AsRef<str>,
    ) -> Result<Self, QbittorrentError> {
        let source_identity = source_identity.into();
        let info_hash = info_hash.into().to_ascii_lowercase();
        if source_identity.trim().is_empty() {
            return Err(QbittorrentError::InvalidSelection {
                message: "source identity must not be empty",
            });
        }
        if info_hash.len() != 40 || !info_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(QbittorrentError::InvalidSelection {
                message: "info hash must be a 40-character hexadecimal value",
            });
        }
        let uri = Url::parse(uri.as_ref()).map_err(|_| QbittorrentError::InvalidSelection {
            message: "torrent URI must be absolute",
        })?;
        if !matches!(uri.scheme(), "magnet" | "http" | "https") {
            return Err(QbittorrentError::InvalidSelection {
                message: "torrent URI scheme is not supported",
            });
        }
        if uri.scheme() == "magnet" {
            let magnet_matches = uri.query_pairs().any(|(name, value)| {
                name.eq_ignore_ascii_case("xt")
                    && value
                        .strip_prefix("urn:btih:")
                        .is_some_and(|hash| hash.eq_ignore_ascii_case(&info_hash))
            });
            if !magnet_matches {
                return Err(QbittorrentError::IdentityMismatch);
            }
        }
        Ok(Self {
            source_identity,
            info_hash,
            uri,
        })
    }
}

impl fmt::Debug for ExplicitTorrentSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExplicitTorrentSelection")
            .field("source_identity", &self.source_identity)
            .field("info_hash", &self.info_hash)
            .field("uri", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TorrentHandle {
    pub source_identity: String,
    pub hash: String,
    pub category: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TorrentState {
    Downloading,
    Checking,
    Queued,
    Stalled,
    Complete,
    Error,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TorrentSnapshot {
    pub name: String,
    pub state: TorrentState,
    pub progress: f64,
    pub amount_left: u64,
    pub content_path: PathBuf,
    pub save_path: PathBuf,
    pub completion_on: Option<i64>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TorrentContent {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

pub struct QbittorrentClient {
    client: reqwest::Client,
    config: QbittorrentConfig,
    cookie: Option<SecretString>,
}

impl QbittorrentClient {
    pub async fn connect(config: QbittorrentConfig) -> Result<Self, QbittorrentError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| QbittorrentError::Configuration {
                message: "HTTP client could not be configured",
            })?;
        let whitelist_probe = client
            .get(endpoint(&config.base_url, "api/v2/app/version")?)
            .header(REFERER, config.base_url.as_str())
            .send()
            .await
            .map_err(|_| QbittorrentError::Transport)?;
        if whitelist_probe.status().is_success() {
            return Ok(Self {
                client,
                config,
                cookie: None,
            });
        }
        let endpoint = endpoint(&config.base_url, "api/v2/auth/login")?;
        let response = client
            .post(endpoint)
            .header(REFERER, config.base_url.as_str())
            .form(&[
                ("username", config.username.as_str()),
                ("password", config.password.expose_secret()),
            ])
            .send()
            .await
            .map_err(|_| QbittorrentError::Transport)?;
        if !response.status().is_success() {
            return Err(QbittorrentError::Unauthorized);
        }
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .filter(|value| value.starts_with("SID=") && value.len() > 4)
            .ok_or(QbittorrentError::Unauthorized)?
            .to_owned();
        let body = response
            .text()
            .await
            .map_err(|_| QbittorrentError::Transport)?;
        if body.trim() != "Ok." {
            return Err(QbittorrentError::Unauthorized);
        }
        Ok(Self {
            client,
            config,
            cookie: Some(SecretString::from(cookie)),
        })
    }

    pub async fn submit_selected(
        &self,
        selection: ExplicitTorrentSelection,
    ) -> Result<TorrentHandle, QbittorrentError> {
        self.submit_selected_with_category(selection, self.config.category.clone())
            .await
    }

    pub async fn submit_selected_to_category(
        &self,
        selection: ExplicitTorrentSelection,
        category: impl Into<String>,
    ) -> Result<TorrentHandle, QbittorrentError> {
        let category = category.into();
        self.ensure_category_exists(&category).await?;
        self.submit_selected_with_category(selection, category)
            .await
    }

    async fn submit_selected_with_category(
        &self,
        selection: ExplicitTorrentSelection,
        category: String,
    ) -> Result<TorrentHandle, QbittorrentError> {
        let handle = TorrentHandle {
            source_identity: selection.source_identity.clone(),
            hash: selection.info_hash.clone(),
            category: category.clone(),
        };
        match self.monitor(&handle).await {
            Ok(_) => return Ok(handle),
            Err(QbittorrentError::TorrentNotFound) => {}
            Err(error) => return Err(error),
        }
        let source = if matches!(selection.uri.scheme(), "http" | "https") {
            self.resolve_http_source(selection.uri.clone(), &selection.info_hash)
                .await?
        } else {
            ResolvedTorrentSource::Magnet(selection.uri)
        };
        let form = match source {
            ResolvedTorrentSource::Torrent(torrent) => reqwest::multipart::Form::new()
                .part(
                    "torrents",
                    reqwest::multipart::Part::bytes(torrent)
                        .file_name("selected.torrent")
                        .mime_str("application/x-bittorrent")
                        .map_err(|_| QbittorrentError::Configuration {
                            message: "torrent MIME type is invalid",
                        })?,
                )
                .text("category", category.clone()),
            ResolvedTorrentSource::Magnet(uri) => reqwest::multipart::Form::new()
                .text("urls", uri.as_str().to_owned())
                .text("category", category.clone()),
        };
        let mut request = self
            .client
            .post(endpoint(&self.config.base_url, "api/v2/torrents/add")?)
            .header(REFERER, self.config.base_url.as_str());
        if let Some(cookie) = &self.cookie {
            request = request.header(COOKIE, cookie.expose_secret());
        }
        let response = request
            .multipart(form)
            .send()
            .await
            .map_err(|_| QbittorrentError::Transport)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(QbittorrentError::Unauthorized);
        }
        if !status.is_success() {
            return Err(QbittorrentError::ProviderResponse { status });
        }
        let body = response
            .text()
            .await
            .map_err(|_| QbittorrentError::Transport)?;
        if !add_response_accepted(status, &body) {
            return Err(QbittorrentError::ProviderResponse { status });
        }
        Ok(handle)
    }

    async fn resolve_http_source(
        &self,
        mut url: Url,
        expected_hash: &str,
    ) -> Result<ResolvedTorrentSource, QbittorrentError> {
        let mut last_redirect = StatusCode::FOUND;
        for _ in 0..=MAX_SOURCE_REDIRECTS {
            let response = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|_| QbittorrentError::Transport)?;
            let status = response.status();
            if status.is_redirection() {
                last_redirect = status;
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(QbittorrentError::ProviderResponse { status })?;
                let next = Url::parse(location)
                    .or_else(|_| url.join(location))
                    .map_err(|_| QbittorrentError::ProviderResponse { status })?;
                if next.scheme() == "magnet" {
                    if !magnet_matches_hash(&next, expected_hash) {
                        return Err(QbittorrentError::IdentityMismatch);
                    }
                    return Ok(ResolvedTorrentSource::Magnet(next));
                }
                if !matches!(next.scheme(), "http" | "https") {
                    return Err(QbittorrentError::ProviderResponse { status });
                }
                url = next;
                continue;
            }
            if !status.is_success() {
                return Err(QbittorrentError::ProviderResponse { status });
            }
            let torrent = crate::prowlarr::read_capped(response, MAX_TORRENT_BYTES)
                .await
                .ok_or(QbittorrentError::ProviderResponse { status })?;
            let info_hash = crate::prowlarr::torrent_info_hash(&torrent)
                .ok_or(QbittorrentError::ProviderResponse { status })?;
            if !info_hash.eq_ignore_ascii_case(expected_hash) {
                return Err(QbittorrentError::IdentityMismatch);
            }
            return Ok(ResolvedTorrentSource::Torrent(torrent));
        }
        Err(QbittorrentError::ProviderResponse {
            status: last_redirect,
        })
    }

    async fn ensure_category_exists(&self, category: &str) -> Result<(), QbittorrentError> {
        if category.trim().is_empty() || category.trim() != category {
            return Err(QbittorrentError::InvalidSelection {
                message: "category must not be empty or padded",
            });
        }
        let response = self
            .get(endpoint(
                &self.config.base_url,
                "api/v2/torrents/categories",
            )?)
            .await?;
        let status = response.status();
        let categories: HashMap<String, serde::de::IgnoredAny> = response
            .json()
            .await
            .map_err(|_| QbittorrentError::ProviderResponse { status })?;
        if !categories.contains_key(category) {
            return Err(QbittorrentError::InvalidSelection {
                message: "category does not exist",
            });
        }
        Ok(())
    }

    pub async fn monitor(
        &self,
        handle: &TorrentHandle,
    ) -> Result<TorrentSnapshot, QbittorrentError> {
        self.validate_handle(handle)?;
        let mut url = endpoint(&self.config.base_url, "api/v2/torrents/info")?;
        url.query_pairs_mut()
            .append_pair("hashes", &handle.hash)
            .append_pair("category", &handle.category);
        let response = self.get(url).await?;
        let status = response.status();
        let torrents: Vec<RawTorrent> = response
            .json()
            .await
            .map_err(|_| QbittorrentError::ProviderResponse { status })?;
        let torrent = torrents
            .into_iter()
            .next()
            .ok_or(QbittorrentError::TorrentNotFound)?;
        if torrent.hash != handle.hash || torrent.category != handle.category {
            return Err(QbittorrentError::IdentityMismatch);
        }
        let state = classify_state(&torrent.state, torrent.progress, torrent.amount_left);
        Ok(TorrentSnapshot {
            name: torrent.name,
            state,
            progress: torrent.progress,
            amount_left: torrent.amount_left,
            content_path: PathBuf::from(torrent.content_path),
            save_path: PathBuf::from(torrent.save_path),
            completion_on: (torrent.completion_on > 0).then_some(torrent.completion_on),
        })
    }

    pub async fn discover_content(
        &self,
        handle: &TorrentHandle,
    ) -> Result<TorrentContent, QbittorrentError> {
        let snapshot = self.monitor(handle).await?;
        if snapshot.state != TorrentState::Complete {
            return Err(QbittorrentError::InvalidSelection {
                message: "torrent content is not complete",
            });
        }
        let mut url = endpoint(&self.config.base_url, "api/v2/torrents/files")?;
        url.query_pairs_mut().append_pair("hash", &handle.hash);
        let response = self.get(url).await?;
        let status = response.status();
        let files: Vec<RawTorrentFile> = response
            .json()
            .await
            .map_err(|_| QbittorrentError::ProviderResponse { status })?;
        let mut paths = Vec::with_capacity(files.len());
        for file in files {
            let relative = PathBuf::from(file.name);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(QbittorrentError::ProviderResponse { status });
            }
            paths.push(snapshot.save_path.join(relative));
        }
        Ok(TorrentContent {
            root: snapshot.content_path,
            files: paths,
        })
    }

    async fn get(&self, url: Url) -> Result<reqwest::Response, QbittorrentError> {
        let mut request = self
            .client
            .get(url)
            .header(REFERER, self.config.base_url.as_str());
        if let Some(cookie) = &self.cookie {
            request = request.header(COOKIE, cookie.expose_secret());
        }
        let response = request
            .send()
            .await
            .map_err(|_| QbittorrentError::Transport)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(QbittorrentError::Unauthorized);
        }
        if status == StatusCode::NOT_FOUND {
            return Err(QbittorrentError::TorrentNotFound);
        }
        if !status.is_success() {
            return Err(QbittorrentError::ProviderResponse { status });
        }
        Ok(response)
    }

    fn validate_handle(&self, handle: &TorrentHandle) -> Result<(), QbittorrentError> {
        if handle.category.trim().is_empty()
            || handle.category.trim() != handle.category
            || handle.hash.len() != 40
            || !handle.hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(QbittorrentError::IdentityMismatch);
        }
        Ok(())
    }
}

enum ResolvedTorrentSource {
    Magnet(Url),
    Torrent(Vec<u8>),
}

fn magnet_matches_hash(uri: &Url, expected_hash: &str) -> bool {
    uri.query_pairs().any(|(name, value)| {
        name.eq_ignore_ascii_case("xt")
            && value
                .strip_prefix("urn:btih:")
                .is_some_and(|hash| hash.eq_ignore_ascii_case(expected_hash))
    })
}

#[derive(Deserialize)]
struct AddResponse {
    failure_count: u64,
    pending_count: u64,
    success_count: u64,
}

fn add_response_accepted(status: StatusCode, body: &str) -> bool {
    if body.trim() == "Ok." {
        return true;
    }
    if status != StatusCode::ACCEPTED {
        return false;
    }
    serde_json::from_str::<AddResponse>(body).is_ok_and(|response| {
        response.failure_count == 0
            && response
                .pending_count
                .saturating_add(response.success_count)
                > 0
    })
}

#[derive(Deserialize)]
struct RawTorrent {
    hash: String,
    name: String,
    category: String,
    state: String,
    progress: f64,
    amount_left: u64,
    content_path: String,
    save_path: String,
    #[serde(default)]
    completion_on: i64,
}

#[derive(Deserialize)]
struct RawTorrentFile {
    name: String,
}

fn classify_state(state: &str, progress: f64, amount_left: u64) -> TorrentState {
    if progress >= 1.0 && amount_left == 0 {
        return TorrentState::Complete;
    }
    match state {
        "error" | "missingFiles" => TorrentState::Error,
        "checkingUP" | "checkingDL" | "checkingResumeData" => TorrentState::Checking,
        "queuedUP" | "queuedDL" => TorrentState::Queued,
        "stalledUP" | "stalledDL" => TorrentState::Stalled,
        "allocating" | "downloading" | "metaDL" | "pausedDL" | "forcedDL" | "moving" => {
            TorrentState::Downloading
        }
        other => TorrentState::Unknown(other.to_owned()),
    }
}

fn endpoint(base_url: &Url, path: &str) -> Result<Url, QbittorrentError> {
    base_url
        .join(path)
        .map_err(|_| QbittorrentError::Configuration {
            message: "API endpoint could not be constructed",
        })
}
