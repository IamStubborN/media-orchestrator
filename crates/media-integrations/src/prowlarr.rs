use std::{cmp::Reverse, collections::HashSet, fmt, io::Cursor, time::Duration};

use futures_util::StreamExt;
use futures_util::future::join_all;
use quick_xml::{
    Reader, XmlVersion,
    encoding::Decoder,
    events::{BytesStart, Event},
};
use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use url::Url;

use crate::prowlarr_episode::{EpisodeCoverage, series_title_matches};

pub const RESULTS_PER_PAGE: u32 = 5;
/// Upper bound on the candidate set fetched from Prowlarr in a single request.
/// The whole set is ranked locally and paginated into `RESULTS_PER_PAGE` pages,
/// so this also bounds how deep pagination can reach.
const CANDIDATE_LIMIT: u32 = 100;
const MAX_TORRENT_BYTES: usize = 8 * 1024 * 1024;
/// Upper bound on the search response body. `CANDIDATE_LIMIT` rich results stay
/// well under this, but a hostile or misbehaving Prowlarr must not be able to
/// stream an unbounded payload into memory before deserialization.
const MAX_SEARCH_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_BENCODE_DEPTH: usize = 128;
const DEFAULT_UNAVAILABLE_RETRY_DELAY: Duration = Duration::from_secs(2);
const ALL_INDEXERS_UNAVAILABLE_MESSAGE: &str =
    "Search failed due to all selected indexers being unavailable";

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProwlarrErrorCode {
    Configuration,
    InvalidRequest,
    Transport,
    Unauthorized,
    TemporarilyUnavailable,
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
    #[error("Prowlarr indexers are temporarily unavailable")]
    TemporarilyUnavailable,
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
            Self::TemporarilyUnavailable => ProwlarrErrorCode::TemporarilyUnavailable,
            Self::ProviderResponse { .. } => ProwlarrErrorCode::ProviderResponse,
        }
    }
}

#[derive(Clone)]
pub struct ProwlarrConfig {
    base_url: Url,
    api_key: SecretString,
    timeout: Duration,
    unavailable_retry_delay: Duration,
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
            unavailable_retry_delay: DEFAULT_UNAVAILABLE_RETRY_DELAY,
        })
    }

    #[must_use]
    pub fn with_unavailable_retry_delay(mut self, delay: Duration) -> Self {
        self.unavailable_retry_delay = delay;
        self
    }
}

impl fmt::Debug for ProwlarrConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProwlarrConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .field("unavailable_retry_delay", &self.unavailable_retry_delay)
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EpisodeAvailabilityQuery {
    pub titles: Vec<String>,
    pub season: u32,
    pub episode: u32,
}

impl EpisodeAvailabilityQuery {
    pub fn new(titles: Vec<String>, season: u32, episode: u32) -> Result<Self, ProwlarrError> {
        let mut seen = HashSet::new();
        let titles = titles
            .into_iter()
            .filter_map(|title| {
                let title = title.trim().to_owned();
                if title.is_empty() {
                    return None;
                }
                let normalized = title.to_lowercase();
                seen.insert(normalized).then_some(title)
            })
            .collect::<Vec<_>>();
        if titles.is_empty() || season == 0 || episode == 0 {
            return Err(ProwlarrError::InvalidRequest {
                message: "episode availability query is incomplete",
            });
        }
        Ok(Self {
            titles,
            season,
            episode,
        })
    }
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

#[derive(Clone)]
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
        let mut results = match self.fetch_search_results(&request).await {
            Err(ProwlarrError::TemporarilyUnavailable) => {
                tokio::time::sleep(self.config.unavailable_retry_delay).await;
                self.recheck_indexers().await;
                self.fetch_search_results(&request).await?
            }
            result => result?,
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

    async fn fetch_search_results(
        &self,
        request: &SearchPageRequest,
    ) -> Result<Vec<ProwlarrResult>, ProwlarrError> {
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
            .append_pair("limit", &CANDIDATE_LIMIT.to_string())
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
        let body = read_capped(response, MAX_SEARCH_RESPONSE_BYTES)
            .await
            .ok_or(ProwlarrError::ProviderResponse { status })?;
        if !status.is_success() {
            return if provider_reports_all_indexers_unavailable(&body) {
                Err(ProwlarrError::TemporarilyUnavailable)
            } else {
                Err(ProwlarrError::ProviderResponse { status })
            };
        }
        let raw: Vec<serde_json::Value> = serde_json::from_slice(&body)
            .map_err(|_| ProwlarrError::ProviderResponse { status })?;
        Ok(raw
            .into_iter()
            .filter_map(|release| serde_json::from_value::<RawRelease>(release).ok())
            .filter(|release| release.protocol == "torrent")
            .filter_map(|release| ProwlarrResult::from_raw(release, &request.session.query))
            .collect())
    }

    async fn recheck_indexers(&self) {
        let Ok(endpoint) = self.config.base_url.join("api/v1/indexer/testall") else {
            return;
        };
        let _ = self
            .client
            .post(endpoint)
            .header("X-Api-Key", self.config.api_key.expose_secret())
            .send()
            .await;
    }

    pub async fn episode_available(
        &self,
        query: &EpisodeAvailabilityQuery,
    ) -> Result<bool, ProwlarrError> {
        let indexers = self.enabled_torrent_indexers().await?;
        if indexers.is_empty() {
            return Err(ProwlarrError::ProviderResponse {
                status: StatusCode::OK,
            });
        }

        let mut first_error = None;
        for title in &query.titles {
            let outcomes = join_all(indexers.iter().map(|indexer_id| {
                self.search_indexer_episode(*indexer_id, title, query.season, query.episode)
            }))
            .await;
            for outcome in outcomes {
                match outcome {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(error) if first_error.is_none() => first_error = Some(error),
                    Err(_) => {}
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(false),
        }
    }

    async fn enabled_torrent_indexers(&self) -> Result<Vec<i32>, ProwlarrError> {
        let endpoint = self.config.base_url.join("api/v1/indexer").map_err(|_| {
            ProwlarrError::Configuration {
                message: "indexer endpoint could not be constructed",
            }
        })?;
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
        let body = read_capped(response, MAX_SEARCH_RESPONSE_BYTES)
            .await
            .ok_or(ProwlarrError::ProviderResponse { status })?;
        let indexers = serde_json::from_slice::<Vec<RawIndexer>>(&body)
            .map_err(|_| ProwlarrError::ProviderResponse { status })?;
        Ok(indexers
            .into_iter()
            .filter(|indexer| indexer.enable && indexer.protocol == "torrent")
            .map(|indexer| indexer.id)
            .collect())
    }

    async fn search_indexer_episode(
        &self,
        indexer_id: i32,
        title: &str,
        season: u32,
        episode: u32,
    ) -> Result<bool, ProwlarrError> {
        let mut endpoint = self
            .config
            .base_url
            .join(&format!("api/v1/indexer/{indexer_id}/newznab"))
            .map_err(|_| ProwlarrError::Configuration {
                message: "episode search endpoint could not be constructed",
            })?;
        endpoint
            .query_pairs_mut()
            .append_pair("t", "tvsearch")
            .append_pair("q", title)
            .append_pair("season", &season.to_string())
            .append_pair("ep", &episode.to_string());
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
        let body = read_capped(response, MAX_SEARCH_RESPONSE_BYTES)
            .await
            .ok_or(ProwlarrError::ProviderResponse { status })?;
        xml_has_matching_episode(&body, title, season, episode)
            .ok_or(ProwlarrError::ProviderResponse { status })
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
        if !response.status().is_success() {
            return None;
        }
        let body = read_capped(response, MAX_TORRENT_BYTES).await?;
        torrent_info_hash(&body)
    }
}

fn xml_has_matching_episode(
    body: &[u8],
    query_title: &str,
    season: u32,
    episode: u32,
) -> Option<bool> {
    let mut reader = Reader::from_reader(Cursor::new(body));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut in_item = false;
    let mut in_title = false;
    let mut in_link = false;
    let mut item_title = String::new();
    let mut has_download = false;
    let mut coordinates = NewznabCoordinates::default();
    loop {
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Start(event) if event.name().as_ref() == b"item" => {
                in_item = true;
                item_title.clear();
                has_download = false;
                coordinates = NewznabCoordinates::default();
            }
            Event::End(event) if event.name().as_ref() == b"item" => {
                if has_download
                    && item_confirms_episode(
                        &item_title,
                        query_title,
                        &coordinates,
                        season,
                        episode,
                    )
                {
                    return Some(true);
                }
                in_item = false;
                in_title = false;
                in_link = false;
            }
            Event::Start(event) if in_item && event.name().as_ref() == b"title" => in_title = true,
            Event::End(event) if event.name().as_ref() == b"title" => in_title = false,
            Event::Start(event) if in_item && event.name().as_ref() == b"link" => in_link = true,
            Event::End(event) if event.name().as_ref() == b"link" => in_link = false,
            Event::Empty(event) if in_item && is_extension_attribute(event.name().as_ref()) => {
                if let Some((kind, value)) = coordinate_attribute(&event, reader.decoder()) {
                    coordinates.insert(kind, value);
                }
            }
            Event::Empty(event) if in_item && event.name().as_ref() == b"enclosure" => {
                for attribute in event.attributes() {
                    let attribute = attribute.ok()?;
                    if attribute.key.as_ref() == b"url"
                        && !attribute
                            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                            .ok()?
                            .trim()
                            .is_empty()
                    {
                        has_download = true;
                    }
                }
            }
            Event::Text(text) if in_item && (in_title || in_link) => {
                let value = text.decode().ok()?;
                if in_title {
                    item_title.push_str(&value);
                }
                if in_link && !value.trim().is_empty() {
                    has_download = true;
                }
            }
            Event::CData(text) if in_item && in_title => {
                item_title.push_str(&text.decode().ok()?);
            }
            Event::Eof => return Some(false),
            _ => {}
        }
        buffer.clear();
    }
}

fn item_confirms_episode(
    item_title: &str,
    query_title: &str,
    coordinates: &NewznabCoordinates,
    season: u32,
    episode: u32,
) -> bool {
    if !series_title_matches(item_title, query_title) {
        return false;
    }

    let title_coverage = EpisodeCoverage::parse(item_title);
    let title_has_coordinates = !title_coverage.is_empty();
    let title_confirms = title_coverage
        .iter()
        .any(|coverage| coverage.contains(season, episode));
    let attributes_complete = coordinates.is_complete();
    let attributes_confirm = coordinates.contains(season, episode);

    if title_has_coordinates && !title_confirms {
        return false;
    }
    if attributes_complete && !attributes_confirm {
        return false;
    }

    title_confirms || attributes_confirm
}

#[derive(Debug, Copy, Clone)]
enum CoordinateAttribute {
    Season,
    Episode,
}

#[derive(Debug, Default)]
struct NewznabCoordinates {
    seasons: Vec<u32>,
    episodes: Vec<u32>,
}

impl NewznabCoordinates {
    fn insert(&mut self, kind: CoordinateAttribute, value: u32) {
        let values = match kind {
            CoordinateAttribute::Season => &mut self.seasons,
            CoordinateAttribute::Episode => &mut self.episodes,
        };
        if value > 0 && !values.contains(&value) {
            values.push(value);
        }
    }

    fn is_complete(&self) -> bool {
        !self.seasons.is_empty() && !self.episodes.is_empty()
    }

    fn contains(&self, season: u32, episode: u32) -> bool {
        self.is_complete() && self.seasons.contains(&season) && self.episodes.contains(&episode)
    }
}

fn is_extension_attribute(name: &[u8]) -> bool {
    name == b"attr" || name.ends_with(b":attr")
}

fn coordinate_attribute(
    event: &BytesStart<'_>,
    decoder: Decoder,
) -> Option<(CoordinateAttribute, u32)> {
    let mut name = None;
    let mut value = None;
    for attribute in event.attributes().flatten() {
        let decoded = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .ok()?;
        match attribute.key.as_ref() {
            b"name" => name = Some(decoded.into_owned()),
            b"value" => value = Some(decoded.into_owned()),
            _ => {}
        }
    }
    let kind = match name?.trim().to_ascii_lowercase().as_str() {
        "season" => CoordinateAttribute::Season,
        "episode" => CoordinateAttribute::Episode,
        _ => return None,
    };
    let value = value?.trim().parse::<u32>().ok()?;
    Some((kind, value))
}

/// Reads a response body into memory, rejecting anything larger than `cap`. The
/// advertised `Content-Length` short-circuits an oversized body, and the stream
/// is also measured chunk by chunk so a response that lies about (or omits) its
/// length cannot exhaust memory.
pub(crate) async fn read_capped(response: reqwest::Response, cap: usize) -> Option<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > cap as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if body.len().saturating_add(chunk.len()) > cap {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

fn provider_reports_all_indexers_unavailable(body: &[u8]) -> bool {
    fn contains_message(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Array(values) => values.iter().any(contains_message),
            serde_json::Value::Object(fields) => {
                fields
                    .get("errorMessage")
                    .and_then(serde_json::Value::as_str)
                    == Some(ALL_INDEXERS_UNAVAILABLE_MESSAGE)
                    || fields.values().any(contains_message)
            }
            _ => false,
        }
    }

    serde_json::from_slice(body)
        .ok()
        .is_some_and(|value| contains_message(&value))
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

pub(crate) fn torrent_info_hash(value: &[u8]) -> Option<String> {
    torrent_info_bytes(value).map(|info| hex::encode(Sha1::digest(info)))
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIndexer {
    id: i32,
    enable: bool,
    protocol: String,
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
