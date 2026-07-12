use std::{cmp::Reverse, fmt, time::Duration};

use futures_util::StreamExt;
use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use url::Url;

pub const RESULTS_PER_PAGE: u32 = 5;
const MAX_TORRENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_BENCODE_DEPTH: usize = 128;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProwlarrErrorCode {
    Configuration,
    InvalidRequest,
    Transport,
    Unauthorized,
    ProviderResponse,
}

#[derive(Debug, thiserror::Error)]
pub enum ProwlarrError {
    #[error("Prowlarr configuration is invalid: {message}")]
    Configuration { message: &'static str },
    #[error("Prowlarr search request is invalid: {message}")]
    InvalidRequest { message: &'static str },
    #[error("Prowlarr request failed")]
    Transport,
    #[error("Prowlarr authentication failed")]
    Unauthorized,
    #[error("Prowlarr returned an invalid response ({status})")]
    ProviderResponse { status: StatusCode },
}

impl ProwlarrError {
    #[must_use]
    pub const fn code(&self) -> ProwlarrErrorCode {
        match self {
            Self::Configuration { .. } => ProwlarrErrorCode::Configuration,
            Self::InvalidRequest { .. } => ProwlarrErrorCode::InvalidRequest,
            Self::Transport => ProwlarrErrorCode::Transport,
            Self::Unauthorized => ProwlarrErrorCode::Unauthorized,
            Self::ProviderResponse { .. } => ProwlarrErrorCode::ProviderResponse,
        }
    }
}

pub struct ProwlarrConfig {
    base_url: Url,
    api_key: SecretString,
    timeout: Duration,
}

impl ProwlarrConfig {
    pub fn new(
        mut base_url: Url,
        api_key: SecretString,
        timeout: Duration,
    ) -> Result<Self, ProwlarrError> {
        if base_url.cannot_be_a_base() || base_url.host_str().is_none() {
            return Err(ProwlarrError::Configuration {
                message: "base URL must be absolute",
            });
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            return Err(ProwlarrError::Configuration {
                message: "base URL must not contain credentials",
            });
        }
        if !base_url.query_pairs().collect::<Vec<_>>().is_empty() || base_url.fragment().is_some() {
            return Err(ProwlarrError::Configuration {
                message: "base URL must not contain a query or fragment",
            });
        }
        if api_key.expose_secret().is_empty() {
            return Err(ProwlarrError::Configuration {
                message: "API key must not be empty",
            });
        }
        if timeout.is_zero() {
            return Err(ProwlarrError::Configuration {
                message: "request timeout must be positive",
            });
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            base_url,
            api_key,
            timeout,
        })
    }
}

impl fmt::Debug for ProwlarrConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProwlarrConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MediaKind {
    Movie,
    Series { season: u16 },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaQuery {
    pub title: String,
    pub kind: MediaKind,
    pub preferred_qualities: Vec<String>,
    pub preferred_languages: Vec<String>,
    pub preferred_codecs: Vec<String>,
    pub preferred_release_groups: Vec<String>,
}

impl MediaQuery {
    #[must_use]
    pub fn movie(title: impl Into<String>) -> Self {
        Self::new(title.into(), MediaKind::Movie)
    }

    #[must_use]
    pub fn series(title: impl Into<String>, season: u16) -> Self {
        Self::new(title.into(), MediaKind::Series { season })
    }

    fn new(title: String, kind: MediaKind) -> Self {
        Self {
            title,
            kind,
            preferred_qualities: Vec::new(),
            preferred_languages: Vec::new(),
            preferred_codecs: Vec::new(),
            preferred_release_groups: Vec::new(),
        }
    }

    #[must_use]
    pub fn prefer_quality<const N: usize>(mut self, values: [&str; N]) -> Self {
        self.preferred_qualities = values.into_iter().map(str::to_owned).collect();
        self
    }

    #[must_use]
    pub fn prefer_languages<const N: usize>(mut self, values: [&str; N]) -> Self {
        self.preferred_languages = values.into_iter().map(str::to_owned).collect();
        self
    }

    #[must_use]
    pub fn prefer_codecs<const N: usize>(mut self, values: [&str; N]) -> Self {
        self.preferred_codecs = values.into_iter().map(str::to_owned).collect();
        self
    }

    #[must_use]
    pub fn prefer_release_groups<const N: usize>(mut self, values: [&str; N]) -> Self {
        self.preferred_release_groups = values.into_iter().map(str::to_owned).collect();
        self
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SearchSession {
    pub id: String,
    pub query: MediaQuery,
}

impl SearchSession {
    pub fn new(id: impl Into<String>, query: MediaQuery) -> Result<Self, ProwlarrError> {
        let session = Self {
            id: id.into(),
            query,
        };
        if session.id.trim().is_empty() {
            return Err(ProwlarrError::InvalidRequest {
                message: "search session ID must not be empty",
            });
        }
        if session.query.title.trim().is_empty() {
            return Err(ProwlarrError::InvalidRequest {
                message: "search title must not be empty",
            });
        }
        if matches!(session.query.kind, MediaKind::Series { season: 0 }) {
            return Err(ProwlarrError::InvalidRequest {
                message: "season must be positive",
            });
        }
        Ok(session)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SearchPageRequest {
    pub session: SearchSession,
    pub offset: u32,
}

impl SearchPageRequest {
    pub fn new(session: SearchSession, offset: u32) -> Result<Self, ProwlarrError> {
        if !offset.is_multiple_of(RESULTS_PER_PAGE) {
            return Err(ProwlarrError::InvalidRequest {
                message: "offset must align to the five-result page size",
            });
        }
        Ok(Self { session, offset })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProwlarrIdentity {
    pub result_id: i32,
    pub indexer_id: i32,
    pub guid: String,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ReleaseSource {
    pub info_hash: Option<String>,
    pub magnet_url: Option<String>,
    pub download_url: Option<String>,
}

impl fmt::Debug for ReleaseSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReleaseSource")
            .field("info_hash", &self.info_hash)
            .field(
                "magnet_url",
                &self.magnet_url.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "download_url",
                &self.download_url.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RankingScore {
    pub exact_title: bool,
    pub exact_season: bool,
    pub quality_preference: usize,
    pub language_preference: usize,
    pub seeders: i32,
    pub size_bytes: u64,
    pub codec_preference: usize,
    pub release_group_preference: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProwlarrResult {
    pub identity: ProwlarrIdentity,
    pub indexer: Option<String>,
    pub title: String,
    pub size_bytes: u64,
    pub seeders: i32,
    pub release_group: Option<String>,
    pub source: ReleaseSource,
    pub ranking: RankingScore,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SearchPage {
    pub session: SearchSession,
    pub offset: u32,
    pub results: Vec<ProwlarrResult>,
    pub continuation: Option<SearchPageRequest>,
}

pub struct ProwlarrClient {
    client: reqwest::Client,
    config: ProwlarrConfig,
}

impl ProwlarrClient {
    pub fn new(config: ProwlarrConfig) -> Result<Self, ProwlarrError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|_| ProwlarrError::Configuration {
                message: "HTTP client could not be configured",
            })?;
        Ok(Self { client, config })
    }

    pub async fn search(&self, request: SearchPageRequest) -> Result<SearchPage, ProwlarrError> {
        let mut results = {
            let mut endpoint = self.config.base_url.join("api/v1/search").map_err(|_| {
                ProwlarrError::Configuration {
                    message: "search endpoint could not be constructed",
                }
            })?;
            let search_type = match request.session.query.kind {
                MediaKind::Movie => "movie",
                MediaKind::Series { .. } => "tvsearch",
            };
            endpoint
                .query_pairs_mut()
                .append_pair("query", &request.session.query.title)
                .append_pair("type", search_type)
                .append_pair("indexerIds", "-2")
                .append_pair("limit", "5")
                .append_pair("offset", "0");
            let response = self
                .client
                .get(endpoint)
                .header("X-Api-Key", self.config.api_key.expose_secret())
                .send()
                .await
                .map_err(|_| ProwlarrError::Transport)?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(ProwlarrError::Unauthorized);
            }
            if !status.is_success() {
                return Err(ProwlarrError::ProviderResponse { status });
            }
            let raw: Vec<RawRelease> = response
                .json()
                .await
                .map_err(|_| ProwlarrError::ProviderResponse { status })?;
            raw.into_iter()
                .filter(|release| release.protocol == "torrent")
                .filter_map(|release| ProwlarrResult::from_raw(release, &request.session.query))
                .collect::<Vec<_>>()
        };

        results.sort_by_key(ranking_key);
        let total_results = results.len();
        let results = results
            .into_iter()
            .skip(request.offset as usize)
            .take(RESULTS_PER_PAGE as usize);
        let mut resolved = Vec::with_capacity(results.len());
        for mut result in results {
            if result.source.info_hash.is_none()
                && let Some(download_url) = result.source.download_url.as_deref()
            {
                let Some(info_hash) = self.resolve_info_hash(download_url).await else {
                    continue;
                };
                result.source.info_hash = Some(info_hash);
            }
            resolved.push(result);
        }
        let continuation = ((request.offset as usize + RESULTS_PER_PAGE as usize) < total_results)
            .then(|| SearchPageRequest {
                session: request.session.clone(),
                offset: request.offset + RESULTS_PER_PAGE,
            });
        Ok(SearchPage {
            session: request.session,
            offset: request.offset,
            results: resolved,
            continuation,
        })
    }

    async fn resolve_info_hash(&self, download_url: &str) -> Option<String> {
        let url = Url::parse(download_url).ok()?;
        if !same_origin(&self.config.base_url, &url) {
            return None;
        }
        let response = self
            .client
            .get(url)
            .header("X-Api-Key", self.config.api_key.expose_secret())
            .send()
            .await
            .ok()?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_TORRENT_BYTES as u64)
        {
            return None;
        }

        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.ok()?;
            if body.len().saturating_add(chunk.len()) > MAX_TORRENT_BYTES {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        let info = torrent_info_bytes(&body)?;
        Some(hex::encode(Sha1::digest(info)))
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn torrent_info_bytes(value: &[u8]) -> Option<&[u8]> {
    if value.first() != Some(&b'd') {
        return None;
    }
    let mut position = 1;
    let mut info_range = None;
    while value.get(position) != Some(&b'e') {
        let (key, next) = parse_bytes(value, position)?;
        position = next;
        let start = position;
        position = skip_bencode(value, position, 1)?;
        if key == b"info" {
            if value.get(start) != Some(&b'd') || info_range.is_some() {
                return None;
            }
            info_range = Some(start..position);
        }
    }
    position += 1;
    if position != value.len() {
        return None;
    }
    info_range.map(|range| &value[range])
}

fn skip_bencode(value: &[u8], position: usize, depth: usize) -> Option<usize> {
    if depth > MAX_BENCODE_DEPTH {
        return None;
    }
    match value.get(position)? {
        b'0'..=b'9' => parse_bytes(value, position).map(|(_, next)| next),
        b'i' => skip_integer(value, position),
        b'l' => skip_collection(value, position, depth, false),
        b'd' => skip_collection(value, position, depth, true),
        _ => None,
    }
}

fn skip_collection(value: &[u8], position: usize, depth: usize, dictionary: bool) -> Option<usize> {
    let mut position = position + 1;
    while value.get(position) != Some(&b'e') {
        if dictionary {
            position = parse_bytes(value, position)?.1;
        }
        position = skip_bencode(value, position, depth + 1)?;
    }
    Some(position + 1)
}

fn skip_integer(value: &[u8], position: usize) -> Option<usize> {
    let start = position + 1;
    let end = value.get(start..)?.iter().position(|byte| *byte == b'e')? + start;
    let digits = value.get(start..end)?;
    let digits = digits.strip_prefix(b"-").unwrap_or(digits);
    if digits.is_empty()
        || !digits.iter().all(u8::is_ascii_digit)
        || (digits.len() > 1 && digits.first() == Some(&b'0'))
    {
        return None;
    }
    Some(end + 1)
}

fn parse_bytes(value: &[u8], position: usize) -> Option<(&[u8], usize)> {
    let colon = value
        .get(position..)?
        .iter()
        .position(|byte| *byte == b':')?
        + position;
    let length_bytes = value.get(position..colon)?;
    if length_bytes.is_empty()
        || !length_bytes.iter().all(u8::is_ascii_digit)
        || (length_bytes.len() > 1 && length_bytes.first() == Some(&b'0'))
    {
        return None;
    }
    let length = std::str::from_utf8(length_bytes)
        .ok()?
        .parse::<usize>()
        .ok()?;
    let start = colon + 1;
    let end = start.checked_add(length)?;
    Some((value.get(start..end)?, end))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRelease {
    id: Option<i32>,
    guid: Option<String>,
    indexer_id: i32,
    indexer: Option<String>,
    title: Option<String>,
    #[serde(default)]
    size: u64,
    seeders: Option<i32>,
    protocol: String,
    info_hash: Option<String>,
    magnet_url: Option<String>,
    download_url: Option<String>,
    sub_group: Option<String>,
}

impl ProwlarrResult {
    fn from_raw(raw: RawRelease, query: &MediaQuery) -> Option<Self> {
        let guid = raw.guid?;
        let result_id = raw
            .id
            .filter(|value| *value > 0)
            .unwrap_or_else(|| stable_result_id(&guid));
        let title = raw.title?;
        let seeders = raw.seeders.unwrap_or_default().max(0);
        let normalized_title = normalize(&title);
        let normalized_query = normalize(&query.title);
        let exact_title = normalized_title == normalized_query
            || normalized_title.starts_with(&format!("{normalized_query} "));
        let exact_season = match query.kind {
            MediaKind::Movie => true,
            MediaKind::Series { season } => {
                let words = normalized_title.split_whitespace().collect::<Vec<_>>();
                let compact = format!("S{season}");
                let padded = format!("S{season:02}");
                let number = season.to_string();
                let padded_number = format!("{season:02}");
                words.iter().any(|part| *part == compact || *part == padded)
                    || words.windows(2).any(|parts| {
                        parts[0] == "SEASON" && (parts[1] == number || parts[1] == padded_number)
                    })
            }
        };
        let quality_preference = preference(&normalized_title, &query.preferred_qualities);
        let language_preference = preference(&normalized_title, &query.preferred_languages);
        let codec_preference = preference(&normalized_title, &query.preferred_codecs);
        let release_group_preference = query
            .preferred_release_groups
            .iter()
            .position(|group| {
                raw.sub_group
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(group))
                    || contains_token(&normalized_title, group)
            })
            .map_or(0, |index| query.preferred_release_groups.len() - index);
        Some(Self {
            identity: ProwlarrIdentity {
                result_id,
                indexer_id: raw.indexer_id,
                guid,
            },
            indexer: raw.indexer,
            title,
            size_bytes: raw.size,
            seeders,
            release_group: raw.sub_group,
            source: ReleaseSource {
                info_hash: raw.info_hash,
                magnet_url: raw.magnet_url,
                download_url: raw.download_url,
            },
            ranking: RankingScore {
                exact_title,
                exact_season,
                quality_preference,
                language_preference,
                seeders,
                size_bytes: raw.size,
                codec_preference,
                release_group_preference,
            },
        })
    }
}

fn stable_result_id(guid: &str) -> i32 {
    let digest = Sha1::digest(guid.as_bytes());
    let value = i32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) & i32::MAX;
    value.max(1)
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct RankingKey {
    exact_title: Reverse<bool>,
    exact_season: Reverse<bool>,
    quality_preference: Reverse<usize>,
    language_preference: Reverse<usize>,
    seeders: Reverse<i32>,
    size_bytes: Reverse<u64>,
    codec_preference: Reverse<usize>,
    release_group_preference: Reverse<usize>,
    indexer_id: i32,
    guid: String,
    result_id: i32,
}

fn ranking_key(result: &ProwlarrResult) -> RankingKey {
    RankingKey {
        exact_title: Reverse(result.ranking.exact_title),
        exact_season: Reverse(result.ranking.exact_season),
        quality_preference: Reverse(result.ranking.quality_preference),
        language_preference: Reverse(result.ranking.language_preference),
        seeders: Reverse(result.ranking.seeders),
        size_bytes: Reverse(result.ranking.size_bytes),
        codec_preference: Reverse(result.ranking.codec_preference),
        release_group_preference: Reverse(result.ranking.release_group_preference),
        indexer_id: result.identity.indexer_id,
        guid: result.identity.guid.clone(),
        result_id: result.identity.result_id,
    }
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn preference(title: &str, preferences: &[String]) -> usize {
    preferences
        .iter()
        .position(|value| contains_token(title, value))
        .map_or(0, |index| preferences.len() - index)
}

fn contains_token(title: &str, token: &str) -> bool {
    let token = normalize(token);
    title
        .split_whitespace()
        .any(|part| part == token || part.contains(&token))
}
