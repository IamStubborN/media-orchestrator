use std::{collections::HashSet, time::Duration};

use futures_util::StreamExt;
use media_core::{
    ReleaseCandidate, ReleaseLifecycle, ReleaseMetadataPort, ReleaseMetadataResult,
    ReleasePrecision, ReleaseQuery, ReleaseQueryError, ScheduledEpisode, select_release_candidate,
};
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::time::sleep;
use url::Url;

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RETRIES: u8 = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum TvmazeError {
    #[error("TVmaze configuration is invalid")]
    Configuration,
    #[error("TVmaze request failed")]
    Transport,
    #[error("TVmaze returned an invalid response")]
    ProviderResponse,
    #[error("TVmaze response exceeded the configured limit")]
    ResponseTooLarge,
}

pub struct TvmazeConfig {
    base_url: Url,
    timeout: Duration,
    user_agent: String,
    max_retries: u8,
}

impl TvmazeConfig {
    pub fn new(
        mut base_url: Url,
        timeout: Duration,
        user_agent: String,
        max_retries: u8,
    ) -> Result<Self, TvmazeError> {
        if base_url.cannot_be_a_base()
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || timeout.is_zero()
            || user_agent.trim().is_empty()
            || max_retries > MAX_RETRIES
        {
            return Err(TvmazeError::Configuration);
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            base_url,
            timeout,
            user_agent,
            max_retries,
        })
    }
}

pub struct TvmazeClient {
    client: reqwest::Client,
    base_url: Url,
    max_retries: u8,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TvmazeShowIdentity {
    pub source_id: u64,
    pub title: String,
    pub year: Option<u16>,
    pub tvdb_id: Option<u64>,
    pub imdb_id: Option<String>,
}

impl TvmazeClient {
    pub fn new(config: TvmazeConfig) -> Result<Self, TvmazeError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(config.user_agent)
            .build()
            .map_err(|_| TvmazeError::Configuration)?;
        Ok(Self {
            client,
            base_url: config.base_url,
            max_retries: config.max_retries,
        })
    }

    async fn search(&self, title: &str) -> Result<Vec<SearchResult>, TvmazeError> {
        let mut endpoint = self
            .base_url
            .join("search/shows")
            .map_err(|_| TvmazeError::Configuration)?;
        endpoint.query_pairs_mut().append_pair("q", title);
        self.get_json(self.client.get(endpoint)).await
    }

    async fn show(&self, show_id: u64) -> Result<ShowDto, TvmazeError> {
        let endpoint = self
            .base_url
            .join(&format!("shows/{show_id}"))
            .map_err(|_| TvmazeError::Configuration)?;
        self.get_json(self.client.get(endpoint)).await
    }

    pub async fn show_identity(&self, show_id: u64) -> Result<TvmazeShowIdentity, TvmazeError> {
        let show = self.show(show_id).await?;
        Ok(TvmazeShowIdentity {
            source_id: show.id,
            title: show.name,
            year: show
                .premiered
                .as_deref()
                .and_then(|value| value.get(..4))
                .and_then(|value| value.parse().ok()),
            tvdb_id: show.externals.thetvdb.filter(|id| *id > 0),
            imdb_id: show.externals.imdb.filter(|id| !id.trim().is_empty()),
        })
    }

    async fn episodes(&self, show_id: u64) -> Result<Vec<EpisodeDto>, TvmazeError> {
        let mut endpoint = self
            .base_url
            .join(&format!("shows/{show_id}/episodes"))
            .map_err(|_| TvmazeError::Configuration)?;
        endpoint.query_pairs_mut().append_pair("specials", "1");
        self.get_json(self.client.get(endpoint)).await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, TvmazeError> {
        let mut attempt = 0_u8;
        loop {
            let response = request
                .try_clone()
                .ok_or(TvmazeError::Configuration)?
                .send()
                .await
                .map_err(|_| TvmazeError::Transport)?;
            if response.status().is_success() {
                let body = read_capped(response).await?;
                return serde_json::from_slice(&body).map_err(|_| TvmazeError::ProviderResponse);
            }
            if attempt >= self.max_retries || !retryable(response.status()) {
                return Err(TvmazeError::ProviderResponse);
            }
            let delay = retry_delay(&response, attempt);
            attempt += 1;
            sleep(delay).await;
        }
    }
}

#[async_trait::async_trait]
impl ReleaseMetadataPort for TvmazeClient {
    async fn query(
        &self,
        query: &ReleaseQuery,
    ) -> Result<ReleaseMetadataResult, ReleaseQueryError> {
        let fetched = OffsetDateTime::now_utc();
        let fetched_at = fetched
            .format(&Rfc3339)
            .map_err(|_| ReleaseQueryError::Provider)?;
        let show = if let Some(source_id) = query.source_id {
            self.show(source_id)
                .await
                .map_err(|_| ReleaseQueryError::Provider)?
                .into_candidate(fetched.date())
        } else {
            let mut results = self
                .search(&query.title)
                .await
                .map_err(|_| ReleaseQueryError::Provider)?;
            if let Some(original_title) = query.original_title.as_deref()
                && !original_title.eq_ignore_ascii_case(&query.title)
            {
                results.extend(
                    self.search(original_title)
                        .await
                        .map_err(|_| ReleaseQueryError::Provider)?,
                );
            }
            let mut seen = HashSet::new();
            let candidates = results
                .into_iter()
                .filter(|result| seen.insert(result.show.id))
                .map(|result| result.show.into_candidate(fetched.date()))
                .collect::<Vec<_>>();

            let Some(selected) = select_release_candidate(query, &candidates) else {
                return Ok(ReleaseMetadataResult::ChoiceNeeded {
                    source: "tvmaze".to_owned(),
                    fetched_at,
                    candidates,
                });
            };
            candidates[selected].clone()
        };
        let lifecycle = show.lifecycle;
        let episodes = self
            .episodes(show.source_id)
            .await
            .map_err(|_| ReleaseQueryError::Provider)?;
        let mut schedule = episodes
            .into_iter()
            .filter_map(EpisodeDto::into_episode)
            .collect::<Vec<_>>();
        schedule.sort_by(|left, right| {
            left.air_at
                .cmp(&right.air_at)
                .then(left.source_id.cmp(&right.source_id))
        });
        let released_episodes = schedule
            .iter()
            .filter(|episode| has_aired(episode, fetched))
            .count() as u32;
        let next_episode = schedule
            .iter()
            .find(|episode| !has_aired(episode, fetched))
            .cloned();
        let precision = next_episode
            .as_ref()
            .map_or(ReleasePrecision::Unknown, |episode| episode.precision);

        Ok(ReleaseMetadataResult::Matched {
            source: "tvmaze".to_owned(),
            fetched_at,
            show,
            precision,
            lifecycle,
            released_episodes,
            expected_episodes: Some(schedule.len() as u32),
            next_episode,
            schedule,
        })
    }
}

fn retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retry_delay(response: &Response, attempt: u8) -> Duration {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_millis(100 * 2_u64.pow(u32::from(attempt))))
        .min(MAX_RETRY_DELAY)
}

async fn read_capped(response: Response) -> Result<Vec<u8>, TvmazeError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(TvmazeError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| TvmazeError::Transport)?;
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(TvmazeError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn has_aired(episode: &ScheduledEpisode, now: OffsetDateTime) -> bool {
    let Some(value) = episode.air_at.as_deref() else {
        return false;
    };
    match episode.precision {
        ReleasePrecision::DateTime => {
            OffsetDateTime::parse(value, &Rfc3339).is_ok_and(|air_at| air_at <= now)
        }
        ReleasePrecision::Date => value <= now.date().to_string().as_str(),
        ReleasePrecision::Unknown => false,
    }
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    show: ShowDto,
}

#[derive(Debug, Deserialize)]
struct ShowDto {
    id: u64,
    name: String,
    premiered: Option<String>,
    status: Option<String>,
    image: Option<ShowImageDto>,
    #[serde(default)]
    externals: ShowExternalsDto,
}

#[derive(Debug, Default, Deserialize)]
struct ShowExternalsDto {
    thetvdb: Option<u64>,
    imdb: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShowImageDto {
    medium: Option<String>,
    original: Option<String>,
}

impl ShowDto {
    fn into_candidate(self, today: time::Date) -> ReleaseCandidate {
        let year = self
            .premiered
            .as_deref()
            .and_then(|value| value.get(..4))
            .and_then(|value| value.parse().ok());
        let lifecycle = match self.status.as_deref() {
            Some("Running" | "To Be Determined") => ReleaseLifecycle::Ongoing,
            Some("Ended") => ReleaseLifecycle::Ended,
            Some("In Development") => ReleaseLifecycle::Upcoming,
            _ if self
                .premiered
                .as_deref()
                .is_some_and(|date| date > today.to_string().as_str()) =>
            {
                ReleaseLifecycle::Upcoming
            }
            _ => ReleaseLifecycle::Unknown,
        };
        ReleaseCandidate {
            source_id: self.id,
            title: self.name,
            original_title: None,
            year,
            poster_url: self.image.and_then(|image| image.original.or(image.medium)),
            lifecycle,
        }
    }
}

#[derive(Debug, Deserialize)]
struct EpisodeDto {
    id: u64,
    name: String,
    season: u32,
    number: Option<u32>,
    airdate: Option<String>,
    airstamp: Option<String>,
}

impl EpisodeDto {
    fn into_episode(self) -> Option<ScheduledEpisode> {
        let episode = self.number?;
        let (air_at, precision) = match (self.airstamp, self.airdate) {
            (Some(value), _) => (Some(value), ReleasePrecision::DateTime),
            (None, Some(value)) => (Some(value), ReleasePrecision::Date),
            (None, None) => (None, ReleasePrecision::Unknown),
        };
        Some(ScheduledEpisode {
            source_id: self.id,
            season: self.season,
            episode,
            title: self.name,
            air_at,
            precision,
        })
    }
}
