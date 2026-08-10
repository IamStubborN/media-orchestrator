use std::{fmt, time::Duration};

use media_contract::{
    BestPageDto, BestRankingDto, DiscoverPageDto, GenreDto, GenreListDto, MediaDetailsDto,
    PremiereFeedDto, PremieresPageDto, SimilarPageDto, TrendingCategoryDto, TrendingItemDto,
    TrendingMediaTypeDto, TrendingPageDto, UpcomingEpisodeDto,
};
use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, de::DeserializeOwned};
use url::Url;

const MAX_RESULTS: usize = 10;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum TmdbErrorCode {
    Configuration,
    InvalidRequest,
    Transport,
    Unauthorized,
    ProviderResponse,
}

#[derive(Debug, thiserror::Error)]
pub enum TmdbError {
    #[error("TMDB configuration is invalid")]
    Configuration,
    #[error("TMDB request is invalid")]
    InvalidRequest,
    #[error("TMDB request failed")]
    Transport,
    #[error("TMDB authentication failed")]
    Unauthorized,
    #[error("TMDB returned an invalid response")]
    ProviderResponse,
}

impl TmdbError {
    #[must_use]
    pub const fn code(&self) -> TmdbErrorCode {
        match self {
            Self::Configuration => TmdbErrorCode::Configuration,
            Self::InvalidRequest => TmdbErrorCode::InvalidRequest,
            Self::Transport => TmdbErrorCode::Transport,
            Self::Unauthorized => TmdbErrorCode::Unauthorized,
            Self::ProviderResponse => TmdbErrorCode::ProviderResponse,
        }
    }
}

#[derive(Clone)]
pub struct TmdbConfig {
    base_url: Url,
    api_key: SecretString,
    language: String,
    timeout: Duration,
}

impl TmdbConfig {
    pub fn new(
        mut base_url: Url,
        api_key: SecretString,
        language: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, TmdbError> {
        let language = language.into();
        if base_url.cannot_be_a_base()
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || api_key.expose_secret().trim().is_empty()
            || language.trim().is_empty()
            || timeout.is_zero()
        {
            return Err(TmdbError::Configuration);
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            base_url,
            api_key,
            language,
            timeout,
        })
    }
}

impl fmt::Debug for TmdbConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TmdbConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("language", &self.language)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Clone)]
pub struct TmdbClient {
    client: reqwest::Client,
    config: TmdbConfig,
}

impl TmdbClient {
    pub fn new(config: TmdbConfig) -> Result<Self, TmdbError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| TmdbError::Configuration)?;
        Ok(Self { client, config })
    }

    pub async fn trending(
        &self,
        category: TrendingCategoryDto,
        page: u32,
    ) -> Result<TrendingPageDto, TmdbError> {
        if page == 0 {
            return Err(TmdbError::InvalidRequest);
        }
        let category_path = match category {
            TrendingCategoryDto::All => "all",
            TrendingCategoryDto::Movie => "movie",
            TrendingCategoryDto::Tv => "tv",
        };
        let endpoint = self
            .config
            .base_url
            .join(&format!("trending/{category_path}/week"))
            .map_err(|_| TmdbError::Configuration)?;
        let query = vec![("page", provider_page(page).to_string())];
        let payload: TrendingResponse = self.get_json(endpoint, &query).await?;
        let results = payload
            .results
            .into_iter()
            .skip(ui_page_offset(page))
            .take(MAX_RESULTS)
            .filter_map(map_item)
            .collect();
        Ok(TrendingPageDto {
            source: "tmdb".to_owned(),
            window: "week".to_owned(),
            category,
            page,
            total_pages: ui_total_pages(payload.total_results),
            total_results: payload.total_results,
            results,
        })
    }

    pub async fn best(
        &self,
        media_type: TrendingMediaTypeDto,
        ranking: BestRankingDto,
        page: u32,
    ) -> Result<BestPageDto, TmdbError> {
        if page == 0 {
            return Err(TmdbError::InvalidRequest);
        }
        let endpoint = self
            .config
            .base_url
            .join(&format!(
                "{}/{}",
                media_type_path(media_type),
                match ranking {
                    BestRankingDto::TopRated => "top_rated",
                    BestRankingDto::Popular => "popular",
                }
            ))
            .map_err(|_| TmdbError::Configuration)?;
        let payload: TrendingResponse = self
            .get_json(endpoint, &[("page", provider_page(page).to_string())])
            .await?;
        Ok(BestPageDto {
            source: "tmdb".to_owned(),
            media_type,
            ranking,
            page,
            total_pages: ui_total_pages(payload.total_results),
            total_results: payload.total_results,
            results: map_typed_results(payload.results, media_type, page),
        })
    }

    pub async fn premieres(
        &self,
        media_type: TrendingMediaTypeDto,
        feed: PremiereFeedDto,
        page: u32,
    ) -> Result<PremieresPageDto, TmdbError> {
        if page == 0 {
            return Err(TmdbError::InvalidRequest);
        }
        let feed_path = match (media_type, feed) {
            (TrendingMediaTypeDto::Movie, PremiereFeedDto::NowPlaying) => "now_playing",
            (TrendingMediaTypeDto::Movie, PremiereFeedDto::Upcoming) => "upcoming",
            (TrendingMediaTypeDto::Tv, PremiereFeedDto::OnTheAir) => "on_the_air",
            (TrendingMediaTypeDto::Tv, PremiereFeedDto::AiringToday) => "airing_today",
            _ => return Err(TmdbError::InvalidRequest),
        };
        let endpoint = self
            .config
            .base_url
            .join(&format!("{}/{feed_path}", media_type_path(media_type)))
            .map_err(|_| TmdbError::Configuration)?;
        let payload: TrendingResponse = self
            .get_json(endpoint, &[("page", provider_page(page).to_string())])
            .await?;
        Ok(PremieresPageDto {
            source: "tmdb".to_owned(),
            media_type,
            feed,
            page,
            total_pages: ui_total_pages(payload.total_results),
            total_results: payload.total_results,
            results: map_typed_results(payload.results, media_type, page),
        })
    }

    pub async fn genres(
        &self,
        media_type: TrendingMediaTypeDto,
    ) -> Result<GenreListDto, TmdbError> {
        let endpoint = self
            .config
            .base_url
            .join(&format!("genre/{}/list", media_type_path(media_type)))
            .map_err(|_| TmdbError::Configuration)?;
        let payload: GenreListResponse = self.get_json(endpoint, &[]).await?;
        let genres = payload
            .genres
            .into_iter()
            .filter_map(|genre| {
                let name = non_empty(Some(genre.name))?;
                (genre.id > 0).then_some(GenreDto { id: genre.id, name })
            })
            .collect();
        Ok(GenreListDto {
            source: "tmdb".to_owned(),
            media_type,
            genres,
        })
    }

    pub async fn discover(
        &self,
        media_type: TrendingMediaTypeDto,
        genre_id: u64,
        page: u32,
    ) -> Result<DiscoverPageDto, TmdbError> {
        if genre_id == 0 || page == 0 {
            return Err(TmdbError::InvalidRequest);
        }
        let endpoint = self
            .config
            .base_url
            .join(&format!("discover/{}", media_type_path(media_type)))
            .map_err(|_| TmdbError::Configuration)?;
        let payload: TrendingResponse = self
            .get_json(
                endpoint,
                &[
                    ("with_genres", genre_id.to_string()),
                    ("sort_by", "popularity.desc".to_owned()),
                    ("page", provider_page(page).to_string()),
                ],
            )
            .await?;
        Ok(DiscoverPageDto {
            source: "tmdb".to_owned(),
            media_type,
            genre_id,
            page,
            total_pages: ui_total_pages(payload.total_results),
            total_results: payload.total_results,
            results: map_typed_results(payload.results, media_type, page),
        })
    }

    pub async fn find(
        &self,
        query: &str,
        media_type: TrendingMediaTypeDto,
    ) -> Result<Option<TrendingItemDto>, TmdbError> {
        if query.trim().is_empty() {
            return Err(TmdbError::InvalidRequest);
        }
        let kind = match media_type {
            TrendingMediaTypeDto::Movie => "movie",
            TrendingMediaTypeDto::Tv => "tv",
        };
        let endpoint = self
            .config
            .base_url
            .join(&format!("search/{kind}"))
            .map_err(|_| TmdbError::Configuration)?;
        let payload: TrendingResponse = self
            .get_json(
                endpoint,
                &[("query", query.trim().to_owned()), ("page", "1".to_owned())],
            )
            .await?;
        Ok(payload
            .results
            .into_iter()
            .filter_map(|item| map_item_for_type(item, media_type))
            .find(|item| tmdb_title_matches(query, item)))
    }

    pub async fn details(
        &self,
        tmdb_id: u64,
        media_type: TrendingMediaTypeDto,
    ) -> Result<MediaDetailsDto, TmdbError> {
        let endpoint = media_endpoint(&self.config.base_url, tmdb_id, media_type, "")?;
        let query = vec![("append_to_response", "external_ids,videos".to_owned())];
        let payload: TmdbDetailsResponse = self.get_json(endpoint, &query).await?;
        map_details(payload, media_type)
    }

    pub async fn similar(
        &self,
        tmdb_id: u64,
        media_type: TrendingMediaTypeDto,
        page: u32,
    ) -> Result<SimilarPageDto, TmdbError> {
        if page == 0 {
            return Err(TmdbError::InvalidRequest);
        }
        let endpoint = media_endpoint(
            &self.config.base_url,
            tmdb_id,
            media_type,
            "/recommendations",
        )?;
        let query = vec![("page", provider_page(page).to_string())];
        let payload: TrendingResponse = self.get_json(endpoint, &query).await?;
        let results = payload
            .results
            .into_iter()
            .skip(ui_page_offset(page))
            .take(MAX_RESULTS)
            .filter_map(|item| map_item_for_type(item, media_type))
            .collect();
        Ok(SimilarPageDto {
            source: "tmdb".to_owned(),
            tmdb_id,
            media_type,
            page,
            total_pages: ui_total_pages(payload.total_results),
            total_results: payload.total_results,
            results,
        })
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        endpoint: Url,
        extra_query: &[(&str, String)],
    ) -> Result<T, TmdbError> {
        let response = self
            .client
            .get(endpoint)
            .query(&[
                ("api_key", self.config.api_key.expose_secret()),
                ("language", self.config.language.as_str()),
            ])
            .query(extra_query)
            .send()
            .await
            .map_err(|_| TmdbError::Transport)?;
        if response.status() == StatusCode::UNAUTHORIZED
            || response.status() == StatusCode::FORBIDDEN
        {
            return Err(TmdbError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(TmdbError::ProviderResponse);
        }
        response
            .json::<T>()
            .await
            .map_err(|_| TmdbError::ProviderResponse)
    }
}

fn tmdb_title_matches(query: &str, item: &TrendingItemDto) -> bool {
    let query = normalize_title(query);
    normalize_title(&item.title) == query
        || item
            .original_title
            .as_deref()
            .is_some_and(|title| normalize_title(title) == query)
}

fn normalize_title(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_lowercase().collect::<String>()
            } else {
                " ".to_owned()
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl fmt::Debug for TmdbClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TmdbClient")
            .field("client", &"[REDACTED]")
            .field("config", &self.config)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct TrendingResponse {
    total_results: u32,
    results: Vec<TrendingResult>,
}

#[derive(Debug, Deserialize)]
struct TrendingResult {
    id: u64,
    media_type: Option<String>,
    title: Option<String>,
    original_title: Option<String>,
    name: Option<String>,
    original_name: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    vote_average: Option<f32>,
    poster_path: Option<String>,
    overview: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GenreListResponse {
    genres: Vec<GenreResult>,
}

#[derive(Debug, Deserialize)]
struct GenreResult {
    id: u64,
    name: String,
}

#[derive(Debug, Deserialize)]
struct TmdbDetailsResponse {
    id: u64,
    title: Option<String>,
    original_title: Option<String>,
    name: Option<String>,
    original_name: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    vote_average: Option<f32>,
    poster_path: Option<String>,
    overview: Option<String>,
    #[serde(default)]
    production_countries: Vec<TmdbCountry>,
    #[serde(default)]
    origin_country: Vec<String>,
    #[serde(default)]
    genres: Vec<TmdbGenre>,
    status: Option<String>,
    number_of_seasons: Option<u16>,
    number_of_episodes: Option<u32>,
    next_episode_to_air: Option<TmdbEpisode>,
    external_ids: Option<TmdbExternalIds>,
    videos: Option<TmdbVideos>,
}

#[derive(Debug, Deserialize)]
struct TmdbEpisode {
    season_number: Option<u16>,
    episode_number: Option<u16>,
    air_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TmdbCountry {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TmdbGenre {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TmdbExternalIds {
    imdb_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TmdbVideos {
    #[serde(default)]
    results: Vec<TmdbVideo>,
}

#[derive(Debug, Deserialize)]
struct TmdbVideo {
    key: Option<String>,
    site: Option<String>,
    #[serde(rename = "type")]
    video_type: Option<String>,
    official: Option<bool>,
}

fn media_endpoint(
    base_url: &Url,
    tmdb_id: u64,
    media_type: TrendingMediaTypeDto,
    suffix: &str,
) -> Result<Url, TmdbError> {
    if tmdb_id == 0 {
        return Err(TmdbError::InvalidRequest);
    }
    let kind = match media_type {
        TrendingMediaTypeDto::Movie => "movie",
        TrendingMediaTypeDto::Tv => "tv",
    };
    base_url
        .join(&format!("{kind}/{tmdb_id}{suffix}"))
        .map_err(|_| TmdbError::Configuration)
}

const fn media_type_path(media_type: TrendingMediaTypeDto) -> &'static str {
    match media_type {
        TrendingMediaTypeDto::Movie => "movie",
        TrendingMediaTypeDto::Tv => "tv",
    }
}

fn poster_url(path: Option<String>) -> Option<String> {
    let path = path?.trim().to_owned();
    if path.is_empty()
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.contains(['?', '#'])
        || path
            .chars()
            .any(|character| character.is_ascii_whitespace() || character.is_control())
    {
        return None;
    }
    Some(format!("https://image.tmdb.org/t/p/w780{path}"))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn map_item(item: TrendingResult) -> Option<TrendingItemDto> {
    let media_type = match item.media_type.as_deref()? {
        "movie" => TrendingMediaTypeDto::Movie,
        "tv" => TrendingMediaTypeDto::Tv,
        _ => return None,
    };
    map_item_for_type(item, media_type)
}

fn map_item_for_type(
    item: TrendingResult,
    media_type: TrendingMediaTypeDto,
) -> Option<TrendingItemDto> {
    if item.id == 0 {
        return None;
    }
    let (title, original_title, date) = match media_type {
        TrendingMediaTypeDto::Movie => (item.title?, item.original_title, item.release_date),
        TrendingMediaTypeDto::Tv => (item.name?, item.original_name, item.first_air_date),
    };
    if title.trim().is_empty() {
        return None;
    }
    let original_title = original_title.filter(|value| value != &title && !value.trim().is_empty());
    Some(TrendingItemDto {
        tmdb_id: item.id,
        media_type,
        title,
        original_title,
        year: date.and_then(|value| value.get(..4)?.parse().ok()),
        rating: item.vote_average.map(|value| (value * 10.0).round() / 10.0),
        poster_url: poster_url(item.poster_path),
        overview: non_empty(item.overview),
    })
}

fn map_typed_results(
    results: Vec<TrendingResult>,
    media_type: TrendingMediaTypeDto,
    page: u32,
) -> Vec<TrendingItemDto> {
    results
        .into_iter()
        .skip(ui_page_offset(page))
        .take(MAX_RESULTS)
        .filter_map(|item| map_item_for_type(item, media_type))
        .collect()
}

const fn provider_page(ui_page: u32) -> u32 {
    (ui_page - 1) / 2 + 1
}

const fn ui_page_offset(ui_page: u32) -> usize {
    if ui_page.is_multiple_of(2) {
        MAX_RESULTS
    } else {
        0
    }
}

const fn ui_total_pages(total_results: u32) -> u32 {
    total_results.div_ceil(MAX_RESULTS as u32)
}

fn map_details(
    item: TmdbDetailsResponse,
    media_type: TrendingMediaTypeDto,
) -> Result<MediaDetailsDto, TmdbError> {
    let (title, original_title, date) = match media_type {
        TrendingMediaTypeDto::Movie => (
            non_empty(item.title),
            non_empty(item.original_title),
            non_empty(item.release_date),
        ),
        TrendingMediaTypeDto::Tv => (
            non_empty(item.name),
            non_empty(item.original_name),
            non_empty(item.first_air_date),
        ),
    };
    let title = title.ok_or(TmdbError::ProviderResponse)?;
    let mut countries = item
        .production_countries
        .into_iter()
        .filter_map(|country| non_empty(country.name))
        .collect::<Vec<_>>();
    if countries.is_empty() {
        countries = item
            .origin_country
            .into_iter()
            .filter_map(|country| non_empty(Some(country)))
            .collect();
    }
    deduplicate(&mut countries);
    let mut genres = item
        .genres
        .into_iter()
        .filter_map(|genre| non_empty(genre.name))
        .collect::<Vec<_>>();
    deduplicate(&mut genres);
    let tmdb_url = (item.id > 0).then(|| {
        let kind = match media_type {
            TrendingMediaTypeDto::Movie => "movie",
            TrendingMediaTypeDto::Tv => "tv",
        };
        format!("https://www.themoviedb.org/{kind}/{}", item.id)
    });
    let imdb_url = item.external_ids.and_then(|ids| safe_imdb_url(ids.imdb_id));
    let trailer_url = item.videos.and_then(safe_trailer_url);
    let is_tv = media_type == TrendingMediaTypeDto::Tv;
    let next_episode = is_tv
        .then_some(item.next_episode_to_air)
        .flatten()
        .and_then(map_upcoming_episode);
    Ok(MediaDetailsDto {
        tmdb_id: item.id,
        media_type,
        title,
        original_title,
        release_date: date.clone(),
        year: date.and_then(|value| year(&value)),
        rating: item.vote_average.map(round_rating),
        poster_url: poster_url(item.poster_path),
        overview: non_empty(item.overview),
        countries,
        genres,
        status: non_empty(item.status),
        season_count: is_tv.then_some(item.number_of_seasons).flatten(),
        episode_count: is_tv.then_some(item.number_of_episodes).flatten(),
        next_episode,
        tmdb_url,
        imdb_url,
        trailer_url,
    })
}

fn map_upcoming_episode(item: TmdbEpisode) -> Option<UpcomingEpisodeDto> {
    let season = item.season_number?;
    let episode = item.episode_number?;
    let air_date = non_empty(item.air_date)?;
    if episode == 0 || !valid_iso_date(&air_date) {
        return None;
    }
    Some(UpcomingEpisodeDto {
        season,
        episode,
        air_date,
    })
}

fn valid_iso_date(value: &str) -> bool {
    let Ok(format) = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]") else {
        return false;
    };
    time::Date::parse(value, &format).is_ok()
}

fn year(value: &str) -> Option<u16> {
    value.get(..4)?.parse().ok()
}

fn round_rating(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

fn deduplicate(values: &mut Vec<String>) {
    let mut unique = Vec::with_capacity(values.len());
    for value in values.drain(..) {
        if !unique.iter().any(|existing| existing == &value) {
            unique.push(value);
        }
    }
    *values = unique;
}

fn safe_imdb_url(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_owned();
    if value.len() < 3
        || value.len() > 20
        || !value.starts_with("tt")
        || !value[2..].bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(format!("https://www.imdb.com/title/{value}/"))
}

fn safe_trailer_url(videos: TmdbVideos) -> Option<String> {
    let video = videos
        .results
        .into_iter()
        .filter(|video| {
            video.site.as_deref() == Some("YouTube")
                && video.video_type.as_deref() == Some("Trailer")
        })
        .filter_map(|video| {
            let key = video.key?.trim().to_owned();
            let safe = !key.is_empty()
                && key.len() <= 64
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte));
            safe.then_some((video.official.unwrap_or(false), key))
        })
        .max_by_key(|(official, _)| *official)?;
    Some(format!("https://www.youtube.com/watch?v={}", video.1))
}
