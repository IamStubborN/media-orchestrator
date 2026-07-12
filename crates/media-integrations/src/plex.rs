use std::{
    fmt,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use reqwest::{StatusCode, header::ACCEPT};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PlexErrorCode {
    Configuration,
    InvalidRequest,
    Transport,
    Unauthorized,
    ProviderResponse,
}

#[derive(Debug, thiserror::Error)]
pub enum PlexError {
    #[error("Plex configuration is invalid: {message}")]
    Configuration { message: &'static str },
    #[error("Plex request is invalid: {message}")]
    InvalidRequest { message: &'static str },
    #[error("Plex request failed")]
    Transport,
    #[error("Plex authentication failed")]
    Unauthorized,
    #[error("Plex returned an invalid response ({status})")]
    ProviderResponse { status: StatusCode },
}

impl PlexError {
    #[must_use]
    pub const fn code(&self) -> PlexErrorCode {
        match self {
            Self::Configuration { .. } => PlexErrorCode::Configuration,
            Self::InvalidRequest { .. } => PlexErrorCode::InvalidRequest,
            Self::Transport => PlexErrorCode::Transport,
            Self::Unauthorized => PlexErrorCode::Unauthorized,
            Self::ProviderResponse { .. } => PlexErrorCode::ProviderResponse,
        }
    }
}

pub struct PlexConfig {
    base_url: Url,
    token: SecretString,
    timeout: Duration,
}

impl PlexConfig {
    pub fn new(
        mut base_url: Url,
        token: SecretString,
        timeout: Duration,
    ) -> Result<Self, PlexError> {
        if base_url.cannot_be_a_base() || base_url.host_str().is_none() {
            return Err(PlexError::Configuration {
                message: "base URL must be absolute",
            });
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            return Err(PlexError::Configuration {
                message: "base URL must not contain credentials",
            });
        }
        if base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(PlexError::Configuration {
                message: "base URL must not contain a query or fragment",
            });
        }
        if token.expose_secret().is_empty() {
            return Err(PlexError::Configuration {
                message: "token must not be empty",
            });
        }
        if timeout.is_zero() {
            return Err(PlexError::Configuration {
                message: "request timeout must be positive",
            });
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            base_url,
            token,
            timeout,
        })
    }
}

impl fmt::Debug for PlexConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlexConfig")
            .field("base_url", &self.base_url)
            .field("token", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ScanRequest {
    pub section_key: u32,
    pub path: PathBuf,
}

impl ScanRequest {
    pub fn new(section_key: u32, path: impl Into<PathBuf>) -> Result<Self, PlexError> {
        let path = path.into();
        validate_absolute_path(&path)?;
        if section_key == 0 {
            return Err(PlexError::InvalidRequest {
                message: "library section key must be positive",
            });
        }
        Ok(Self { section_key, path })
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PlexMediaKind {
    Movie,
    Episode { season: u16, episode: u16 },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExpectedPlexItem {
    pub rating_key: u64,
    pub path: PathBuf,
    pub canonical_identity: String,
    pub kind: PlexMediaKind,
}

impl ExpectedPlexItem {
    pub fn movie(
        rating_key: u64,
        path: impl Into<PathBuf>,
        canonical_identity: impl Into<String>,
    ) -> Result<Self, PlexError> {
        Self::new(
            rating_key,
            path.into(),
            canonical_identity.into(),
            PlexMediaKind::Movie,
        )
    }

    pub fn episode(
        rating_key: u64,
        path: impl Into<PathBuf>,
        canonical_identity: impl Into<String>,
        season: u16,
        episode: u16,
    ) -> Result<Self, PlexError> {
        if season == 0 || episode == 0 {
            return Err(PlexError::InvalidRequest {
                message: "season and episode must be positive",
            });
        }
        Self::new(
            rating_key,
            path.into(),
            canonical_identity.into(),
            PlexMediaKind::Episode { season, episode },
        )
    }

    fn new(
        rating_key: u64,
        path: PathBuf,
        canonical_identity: String,
        kind: PlexMediaKind,
    ) -> Result<Self, PlexError> {
        if rating_key == 0 {
            return Err(PlexError::InvalidRequest {
                message: "rating key must be positive",
            });
        }
        validate_absolute_path(&path)?;
        let mut identity = canonical_identity.split("//");
        if canonical_identity.trim().is_empty()
            || !canonical_identity.contains("://")
            || identity.next().is_none()
            || identity.next().is_none()
        {
            return Err(PlexError::InvalidRequest {
                message: "canonical identity must use a provider URI",
            });
        }
        Ok(Self {
            rating_key,
            path,
            canonical_identity,
            kind,
        })
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PlexMismatch {
    RatingKey,
    MediaType,
    Path,
    CanonicalIdentity,
    Season,
    Episode,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PlexVerification {
    Matched { rating_key: u64, plex_guid: String },
    NotFound,
    Mismatch(Vec<PlexMismatch>),
}

pub struct PlexClient {
    client: reqwest::Client,
    config: PlexConfig,
}

impl PlexClient {
    pub fn new(config: PlexConfig) -> Result<Self, PlexError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| PlexError::Configuration {
                message: "HTTP client could not be configured",
            })?;
        Ok(Self { client, config })
    }

    pub async fn trigger_scan(&self, request: &ScanRequest) -> Result<(), PlexError> {
        let mut endpoint = self
            .config
            .base_url
            .join(&format!("library/sections/{}/refresh", request.section_key))
            .map_err(|_| PlexError::Configuration {
                message: "scan endpoint could not be constructed",
            })?;
        endpoint
            .query_pairs_mut()
            .append_pair("path", &request.path.to_string_lossy());
        self.send_get(endpoint).await?;
        Ok(())
    }

    pub async fn verify(&self, expected: &ExpectedPlexItem) -> Result<PlexVerification, PlexError> {
        let mut endpoint = self
            .config
            .base_url
            .join(&format!("library/metadata/{}", expected.rating_key))
            .map_err(|_| PlexError::Configuration {
                message: "metadata endpoint could not be constructed",
            })?;
        endpoint.query_pairs_mut().append_pair("includeGuids", "1");
        let response = self.send_get(endpoint).await?;
        let status = response.status();
        let payload: MetadataResponse = response
            .json()
            .await
            .map_err(|_| PlexError::ProviderResponse { status })?;
        let Some(item) = payload.media_container.metadata.into_iter().next() else {
            return Ok(PlexVerification::NotFound);
        };
        let mut mismatches = Vec::new();
        if item.rating_key.parse::<u64>().ok() != Some(expected.rating_key) {
            mismatches.push(PlexMismatch::RatingKey);
        }
        let expected_type = match expected.kind {
            PlexMediaKind::Movie => "movie",
            PlexMediaKind::Episode { .. } => "episode",
        };
        if item.media_type != expected_type {
            mismatches.push(PlexMismatch::MediaType);
        }
        let path_matches = item
            .media
            .iter()
            .flat_map(|media| &media.parts)
            .any(|part| Path::new(&part.file) == expected.path);
        if !path_matches {
            mismatches.push(PlexMismatch::Path);
        }
        if item.guid != expected.canonical_identity
            && !item
                .guids
                .iter()
                .any(|guid| guid.id == expected.canonical_identity)
        {
            mismatches.push(PlexMismatch::CanonicalIdentity);
        }
        if let PlexMediaKind::Episode { season, episode } = expected.kind {
            if item.parent_index != Some(season) {
                mismatches.push(PlexMismatch::Season);
            }
            if item.index != Some(episode) {
                mismatches.push(PlexMismatch::Episode);
            }
        }
        if mismatches.is_empty() {
            Ok(PlexVerification::Matched {
                rating_key: expected.rating_key,
                plex_guid: item.guid,
            })
        } else {
            Ok(PlexVerification::Mismatch(mismatches))
        }
    }

    pub async fn verify_path(
        &self,
        section_key: u32,
        path: &Path,
        canonical_identity: &str,
        season: Option<u16>,
        episode: Option<u16>,
    ) -> Result<PlexVerification, PlexError> {
        validate_absolute_path(path)?;
        if section_key == 0 || season.is_some() != episode.is_some() {
            return Err(PlexError::InvalidRequest {
                message: "path verification request is invalid",
            });
        }
        let mut endpoint = self
            .config
            .base_url
            .join(&format!("library/sections/{section_key}/all"))
            .map_err(|_| PlexError::Configuration {
                message: "section endpoint could not be constructed",
            })?;
        endpoint.query_pairs_mut().append_pair("includeGuids", "1");
        let response = self.send_get(endpoint).await?;
        let status = response.status();
        let payload: MetadataResponse = response
            .json()
            .await
            .map_err(|_| PlexError::ProviderResponse { status })?;
        let Some(item) = payload.media_container.metadata.into_iter().find(|item| {
            item.media
                .iter()
                .flat_map(|media| &media.parts)
                .any(|part| Path::new(&part.file) == path)
        }) else {
            return Ok(PlexVerification::NotFound);
        };
        let mut mismatches = Vec::new();
        let expected_type = if season.is_some() { "episode" } else { "movie" };
        if item.media_type != expected_type {
            mismatches.push(PlexMismatch::MediaType);
        }
        if item.guid != canonical_identity
            && !item.guids.iter().any(|guid| guid.id == canonical_identity)
        {
            mismatches.push(PlexMismatch::CanonicalIdentity);
        }
        if item.parent_index != season {
            mismatches.push(PlexMismatch::Season);
        }
        if item.index != episode {
            mismatches.push(PlexMismatch::Episode);
        }
        let rating_key = item
            .rating_key
            .parse::<u64>()
            .map_err(|_| PlexError::ProviderResponse { status })?;
        if mismatches.is_empty() {
            Ok(PlexVerification::Matched {
                rating_key,
                plex_guid: item.guid,
            })
        } else {
            Ok(PlexVerification::Mismatch(mismatches))
        }
    }

    async fn send_get(&self, endpoint: Url) -> Result<reqwest::Response, PlexError> {
        let response = self
            .client
            .get(endpoint)
            .header("X-Plex-Token", self.config.token.expose_secret())
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| PlexError::Transport)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(PlexError::Unauthorized);
        }
        if !status.is_success() {
            return Err(PlexError::ProviderResponse { status });
        }
        Ok(response)
    }
}

#[derive(Deserialize)]
struct MetadataResponse {
    #[serde(rename = "MediaContainer")]
    media_container: MediaContainer,
}

#[derive(Deserialize)]
struct MediaContainer {
    #[serde(rename = "Metadata", default)]
    metadata: Vec<MetadataItem>,
}

#[derive(Deserialize)]
struct MetadataItem {
    #[serde(rename = "ratingKey")]
    rating_key: String,
    guid: String,
    #[serde(rename = "type")]
    media_type: String,
    #[serde(rename = "parentIndex")]
    parent_index: Option<u16>,
    index: Option<u16>,
    #[serde(rename = "Guid", default)]
    guids: Vec<Guid>,
    #[serde(rename = "Media", default)]
    media: Vec<Media>,
}

#[derive(Deserialize)]
struct Guid {
    id: String,
}

#[derive(Deserialize)]
struct Media {
    #[serde(rename = "Part", default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    file: String,
}

fn validate_absolute_path(path: &Path) -> Result<(), PlexError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PlexError::InvalidRequest {
            message: "media path must be absolute and normalized",
        });
    }
    Ok(())
}
