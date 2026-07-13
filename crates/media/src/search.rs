use std::{collections::BTreeMap, sync::Arc};

use media_api::{SearchError, SearchService};
use media_contract::{
    ContinueSearchRequest, ExecutionSelectionDto, JobDto, JobStateDto, MAX_SEARCH_RESULTS_PER_PAGE,
    MediaKindDto, NotifyScopeDto, ProviderDto, SearchPageDto, SearchResultDto, SelectResultRequest,
    StartSearchRequest,
};
use media_core::{
    EpisodeDiscoveryPort, EpisodeSnapshot, Job, JobApplication, JobState, NewJobCommand,
    NotifyScope, OperationKey, PortError, Provider, TrackingSubscription, UserId,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const SEARCH_TTL: time::Duration = time::Duration::hours(24);

#[derive(Debug, Clone)]
pub struct ProviderPage {
    pub results: Vec<ProviderResult>,
    pub provider_continuation: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderResult {
    pub public: SearchResultDto,
    private: PrivateResult,
}

#[derive(Debug, Clone)]
enum PrivateResult {
    Rezka {
        locator: String,
        title_id: u64,
        translation_episodes: BTreeMap<u64, Vec<(u32, Vec<u32>)>>,
    },
    Prowlarr {
        source_identity: String,
        info_hash: String,
        uri: String,
    },
}

impl ProviderResult {
    #[must_use]
    pub fn rezka(public: SearchResultDto, locator: String, title_id: u64) -> Self {
        let translation_episodes = match &public {
            SearchResultDto::Rezka {
                translations,
                availability: Some(availability),
                ..
            } => translations
                .iter()
                .map(|translation| {
                    (
                        translation.id,
                        availability
                            .seasons
                            .iter()
                            .map(|season| (season.season, season.episodes.clone()))
                            .collect(),
                    )
                })
                .collect(),
            _ => BTreeMap::new(),
        };
        Self {
            public,
            private: PrivateResult::Rezka {
                locator,
                title_id,
                translation_episodes,
            },
        }
    }

    fn rezka_with_availability(
        public: SearchResultDto,
        locator: String,
        title_id: u64,
        translation_episodes: BTreeMap<u64, Vec<(u32, Vec<u32>)>>,
    ) -> Self {
        Self {
            public,
            private: PrivateResult::Rezka {
                locator,
                title_id,
                translation_episodes,
            },
        }
    }

    #[must_use]
    pub fn prowlarr(
        public: SearchResultDto,
        source_identity: String,
        info_hash: String,
        uri: String,
    ) -> Self {
        Self {
            public,
            private: PrivateResult::Prowlarr {
                source_identity,
                info_hash,
                uri,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoredSearchSession {
    pub id: String,
    pub owner: UserId,
    pub request: StartSearchRequest,
    pub expires_at: OffsetDateTime,
    pub results: Vec<ProviderResult>,
    pub provider_continuation: Option<String>,
}

#[async_trait::async_trait]
pub trait SearchPersistence: Send + Sync {
    async fn insert_session(&self, session: StoredSearchSession) -> Result<(), SearchError>;
    async fn session_for_owner(
        &self,
        id: &str,
        owner: UserId,
    ) -> Result<StoredSearchSession, SearchError>;
    async fn update_session(&self, session: StoredSearchSession) -> Result<(), SearchError>;
    async fn insert_execution(
        &self,
        result_ref: String,
        execution: ExecutionSelectionDto,
    ) -> Result<(), SearchError>;
    async fn execution_for(&self, result_ref: &str) -> Result<ExecutionSelectionDto, SearchError>;
}

#[async_trait::async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(
        &self,
        request: &StartSearchRequest,
        continuation: Option<&str>,
    ) -> Result<ProviderPage, SearchError>;
}

pub struct ProviderEpisodeDiscovery {
    provider: Arc<dyn SearchProvider>,
}

impl ProviderEpisodeDiscovery {
    #[must_use]
    pub fn new(provider: Arc<dyn SearchProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for ProviderEpisodeDiscovery {
    async fn available_episodes(
        &self,
        tracking: &TrackingSubscription,
    ) -> Result<Vec<EpisodeSnapshot>, PortError> {
        let page = self
            .provider
            .search(
                &StartSearchRequest {
                    scope: media_contract::SearchScopeDto {
                        platform: "system".to_owned(),
                        chat_id: "tracking".to_owned(),
                        thread_id: None,
                    },
                    source: ProviderDto::Rezka,
                    query: tracking.title().to_owned(),
                    media_kind: None,
                    season: None,
                    preferred_qualities: Vec::new(),
                    preferred_languages: Vec::new(),
                    preferred_codecs: Vec::new(),
                    preferred_release_groups: Vec::new(),
                },
                None,
            )
            .await
            .map_err(|_| PortError::Infrastructure)?;
        let result = page.results.into_iter().find(|result| {
            matches!(
                &result.public,
                SearchResultDto::Rezka { title, .. }
                    if title.trim().eq_ignore_ascii_case(tracking.title().trim())
            )
        });
        let Some(ProviderResult {
            public: SearchResultDto::Rezka { translations, .. },
            private:
                PrivateResult::Rezka {
                    translation_episodes,
                    ..
                },
        }) = result
        else {
            return Err(PortError::Infrastructure);
        };
        let translation_id = translations
            .iter()
            .find(|translation| {
                translation
                    .name
                    .trim()
                    .eq_ignore_ascii_case(tracking.translation().trim())
            })
            .map(|translation| translation.id)
            .ok_or(PortError::Infrastructure)?;
        let mut episodes = translation_episodes
            .get(&translation_id)
            .ok_or(PortError::Infrastructure)?
            .iter()
            .flat_map(|(season, episodes)| {
                episodes
                    .iter()
                    .filter_map(|episode| EpisodeSnapshot::new(*season, *episode).ok())
            })
            .collect::<Vec<_>>();
        episodes.sort_unstable();
        episodes.dedup();
        Ok(episodes)
    }
}

pub struct ConcreteSearchProvider {
    rezka: Option<tokio::sync::Mutex<crate::composition::PreparedRunnerSession>>,
    prowlarr: Option<media_integrations::prowlarr::ProwlarrClient>,
}

impl ConcreteSearchProvider {
    #[must_use]
    pub fn new(
        rezka: Option<crate::composition::PreparedRunnerSession>,
        prowlarr: Option<media_integrations::prowlarr::ProwlarrClient>,
    ) -> Self {
        Self {
            rezka: rezka.map(tokio::sync::Mutex::new),
            prowlarr,
        }
    }

    async fn search_rezka(
        &self,
        request: &StartSearchRequest,
        continuation: Option<&str>,
    ) -> Result<ProviderPage, SearchError> {
        let state = self.rezka.as_ref().ok_or(SearchError::Provider)?;
        let mut state = state.lock().await;
        let prepared = &mut *state;
        prepared
            .reload_session()
            .map_err(|_| SearchError::Infrastructure)?;
        prepared
            .client
            .ensure_authenticated(
                prepared
                    .credentials
                    .as_ref()
                    .ok_or(SearchError::Infrastructure)?,
                &prepared.probe,
            )
            .await
            .map_err(|error| {
                tracing::warn!(stage = "authentication", error_code = ?error.code(), error = %error, "Rezka search failed");
                SearchError::Provider
            })?;
        let snapshot = prepared
            .client
            .export_session()
            .map_err(|error| {
                tracing::warn!(stage = "session_export", error_code = ?error.code(), error = %error, "Rezka search failed");
                SearchError::Provider
            })?;
        prepared
            .store
            .save(&snapshot)
            .map_err(|_| SearchError::Infrastructure)?;
        let offset = continuation.map_or(Ok(0), |value| {
            value
                .strip_prefix("quick:")
                .ok_or(SearchError::InvalidRequest)?
                .parse::<usize>()
                .map_err(|_| SearchError::InvalidRequest)
        })?;
        let query = rezka_client::QuickSearchQuery::new(&request.query)
            .map_err(|_| SearchError::InvalidRequest)?;
        let entries = prepared.client.quick_search(&query).await.map_err(|error| {
            tracing::warn!(stage = "quick_search", error_code = ?error.code(), error = %error, "Rezka search failed");
            SearchError::Provider
        })?;
        if offset > entries.len() {
            return Err(SearchError::InvalidRequest);
        }
        let mut results = Vec::new();
        let mut cursor = offset;
        while cursor < entries.len() && results.len() < MAX_SEARCH_RESULTS_PER_PAGE {
            let entry = &entries[cursor];
            cursor += 1;
            let details = match prepared.client.title(entry.locator()).await {
                Ok(details) => details,
                Err(error)
                    if matches!(
                        error.code(),
                        rezka_client::RezkaErrorCode::ProviderResponseInvalid
                            | rezka_client::RezkaErrorCode::TitleNotFound
                    ) =>
                {
                    tracing::warn!(
                        stage = "title",
                        error_code = ?error.code(),
                        error = %error,
                        "skipping unusable Rezka search result"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(stage = "title", error_code = ?error.code(), error = %error, "Rezka search failed");
                    return Err(SearchError::Provider);
                }
            };
            let media_kind = match details.kind() {
                rezka_client::RezkaMediaKind::Movie => MediaKindDto::Movie,
                rezka_client::RezkaMediaKind::Series => MediaKindDto::Series,
            };
            if request
                .media_kind
                .is_some_and(|requested| requested != media_kind)
            {
                continue;
            }
            let translations = details
                .translations()
                .iter()
                .map(|translation| media_contract::RezkaTranslationDto {
                    id: translation.id().get(),
                    name: translation.name().to_owned(),
                    premium: translation.is_premium(),
                    director: translation.is_director(),
                    camrip: translation.is_camrip(),
                    has_ads: translation.has_ads(),
                })
                .collect::<Vec<_>>();
            let mut by_translation = BTreeMap::new();
            if media_kind == MediaKindDto::Series {
                let selections = details
                    .translations()
                    .iter()
                    .map(|translation| {
                        (
                            translation.id().get(),
                            details.select_translation(translation.key()),
                        )
                    })
                    .collect::<Vec<_>>();
                for (translation_id, selection) in selections {
                    let availability = prepared
                        .client
                        .series_availability(&selection.map_err(|_| SearchError::Provider)?)
                        .await
                        .map_err(|error| {
                            tracing::warn!(stage = "availability", error_code = ?error.code(), error = %error, "Rezka search failed");
                            SearchError::Provider
                        })?;
                    by_translation.insert(
                        translation_id,
                        availability
                            .seasons()
                            .iter()
                            .map(|season| {
                                (
                                    season.number(),
                                    season
                                        .episodes()
                                        .iter()
                                        .map(|episode| episode.number())
                                        .collect(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    );
                }
            }
            let union = union_availability(&by_translation);
            let availability = (media_kind == MediaKindDto::Series).then(|| {
                let latest = union.iter().rev().find_map(|(season, episodes)| {
                    episodes
                        .iter()
                        .max()
                        .copied()
                        .map(|episode| (*season, episode))
                });
                let lifecycle = details.series_lifecycle_status();
                let incomplete = lifecycle != rezka_client::SeriesLifecycleStatus::Completed;
                media_contract::SeriesAvailabilityDto {
                    lifecycle_status: match lifecycle {
                        rezka_client::SeriesLifecycleStatus::Completed => {
                            media_contract::SeriesLifecycleStatusDto::Completed
                        }
                        rezka_client::SeriesLifecycleStatus::Ongoing => {
                            media_contract::SeriesLifecycleStatusDto::Ongoing
                        }
                        rezka_client::SeriesLifecycleStatus::Unknown => {
                            media_contract::SeriesLifecycleStatusDto::Unknown
                        }
                    },
                    incomplete,
                    seasons: union
                        .into_iter()
                        .map(|(season, episodes)| media_contract::SeasonAvailabilityDto {
                            season,
                            episodes,
                        })
                        .collect(),
                    tracking_prompt: (lifecycle == rezka_client::SeriesLifecycleStatus::Ongoing)
                        .then_some(latest)
                        .flatten()
                        .map(
                            |(latest_season, latest_episode)| media_contract::TrackingPromptDto {
                                title: details.title().to_owned(),
                                latest_season,
                                latest_episode,
                            },
                        ),
                }
            });
            let public = SearchResultDto::Rezka {
                result_id: format!("rezka:{}", details.id().get()),
                title: details.title().to_owned(),
                original_title: details.original_title().map(str::to_owned),
                year: details.release_year(),
                media_kind,
                thumbnail_url: details
                    .thumbnail()
                    .map(|image| image.url().as_str().to_owned()),
                translations,
                availability,
            };
            results.push(ProviderResult::rezka_with_availability(
                public,
                details.locator().as_str().to_owned(),
                details.id().get(),
                by_translation,
            ));
        }
        let provider_continuation = (cursor < entries.len()).then(|| format!("quick:{cursor}"));
        Ok(ProviderPage {
            results,
            provider_continuation,
        })
    }

    async fn search_prowlarr(
        &self,
        request: &StartSearchRequest,
        continuation: Option<&str>,
    ) -> Result<ProviderPage, SearchError> {
        let client = self.prowlarr.as_ref().ok_or(SearchError::Provider)?;
        let kind = match (request.media_kind, request.season) {
            (Some(MediaKindDto::Movie), None) => media_integrations::prowlarr::MediaKind::Movie,
            (Some(MediaKindDto::Series), Some(season)) => {
                media_integrations::prowlarr::MediaKind::Series { season }
            }
            _ => return Err(SearchError::InvalidRequest),
        };
        let query = media_integrations::prowlarr::MediaQuery {
            title: request.query.clone(),
            kind,
            preferred_qualities: request.preferred_qualities.clone(),
            preferred_languages: request.preferred_languages.clone(),
            preferred_codecs: request.preferred_codecs.clone(),
            preferred_release_groups: request.preferred_release_groups.clone(),
        };
        let session = media_integrations::prowlarr::SearchSession::new(
            uuid::Uuid::new_v4().to_string(),
            query,
        )
        .map_err(|_| SearchError::InvalidRequest)?;
        let offset = continuation.map_or(Ok(0), |value| {
            value
                .parse::<u32>()
                .map_err(|_| SearchError::InvalidRequest)
        })?;
        let page = client
            .search(
                media_integrations::prowlarr::SearchPageRequest::new(session, offset)
                    .map_err(|_| SearchError::InvalidRequest)?,
            )
            .await
            .map_err(|error| {
                tracing::warn!(error_code = ?error.code(), "Prowlarr search failed");
                SearchError::Provider
            })?;
        let provider_continuation = page
            .continuation
            .as_ref()
            .map(|next| next.offset.to_string());
        let results = page
            .results
            .into_iter()
            .filter_map(|result| {
                let info_hash = result.source.info_hash?;
                let uri = result.source.magnet_url.or(result.source.download_url)?;
                let result_id = format!(
                    "prowlarr:{}:{}",
                    result.identity.indexer_id, result.identity.result_id
                );
                let source_identity = format!(
                    "{}:{}:{}",
                    result.identity.indexer_id, result.identity.guid, result.identity.result_id
                );
                let public = SearchResultDto::Prowlarr {
                    result_id,
                    title: result.title.clone(),
                    indexer: result.indexer,
                    size_bytes: result.size_bytes,
                    seeders: result.seeders,
                    release_group: result.release_group,
                    ranking: media_contract::ProwlarrRankingDto {
                        exact_title: result.ranking.exact_title,
                        exact_season: result.ranking.exact_season,
                        quality_preference: result.ranking.quality_preference,
                        language_preference: result.ranking.language_preference,
                        seeders: result.ranking.seeders,
                        size_bytes: result.ranking.size_bytes,
                        codec_preference: result.ranking.codec_preference,
                        release_group_preference: result.ranking.release_group_preference,
                    },
                };
                Some(ProviderResult::prowlarr(
                    public,
                    source_identity,
                    info_hash,
                    uri,
                ))
            })
            .collect();
        Ok(ProviderPage {
            results,
            provider_continuation,
        })
    }
}

#[async_trait::async_trait]
impl SearchProvider for ConcreteSearchProvider {
    async fn search(
        &self,
        request: &StartSearchRequest,
        continuation: Option<&str>,
    ) -> Result<ProviderPage, SearchError> {
        match request.source {
            ProviderDto::Rezka => self.search_rezka(request, continuation).await,
            ProviderDto::Prowlarr => self.search_prowlarr(request, continuation).await,
        }
    }
}

fn union_availability(
    by_translation: &BTreeMap<u64, Vec<(u32, Vec<u32>)>>,
) -> BTreeMap<u32, Vec<u32>> {
    let mut union = BTreeMap::<u32, Vec<u32>>::new();
    for seasons in by_translation.values() {
        for (season, episodes) in seasons {
            let values = union.entry(*season).or_default();
            values.extend(episodes);
            values.sort_unstable();
            values.dedup();
        }
    }
    union
}

#[derive(Clone)]
pub struct StorageSearchPersistence {
    repository: media_storage::SeaOrmSearchRepository,
}

impl StorageSearchPersistence {
    #[must_use]
    pub fn new(repository: media_storage::SeaOrmSearchRepository) -> Self {
        Self { repository }
    }
}

#[async_trait::async_trait]
impl SearchPersistence for StorageSearchPersistence {
    async fn insert_session(&self, session: StoredSearchSession) -> Result<(), SearchError> {
        let id = uuid::Uuid::parse_str(&session.id).map_err(|_| SearchError::Infrastructure)?;
        let payload = encode_session_payload(&session)?;
        self.repository
            .insert_session(media_storage::SearchSessionRecord {
                id,
                owner: session.owner,
                payload,
                expires_at: session.expires_at,
            })
            .await
            .map_err(storage_error)
    }

    async fn session_for_owner(
        &self,
        id: &str,
        owner: UserId,
    ) -> Result<StoredSearchSession, SearchError> {
        let id = uuid::Uuid::parse_str(id).map_err(|_| SearchError::InvalidRequest)?;
        let record = self
            .repository
            .session_for_owner(id, owner)
            .await
            .map_err(storage_error)?
            .ok_or(SearchError::NotFound)?;
        let (request, results, provider_continuation) = decode_session_payload(record.payload)?;
        Ok(StoredSearchSession {
            id: record.id.to_string(),
            owner: record.owner,
            request,
            expires_at: record.expires_at,
            results,
            provider_continuation,
        })
    }

    async fn update_session(&self, session: StoredSearchSession) -> Result<(), SearchError> {
        let id = uuid::Uuid::parse_str(&session.id).map_err(|_| SearchError::Infrastructure)?;
        let payload = encode_session_payload(&session)?;
        self.repository
            .update_session(media_storage::SearchSessionRecord {
                id,
                owner: session.owner,
                payload,
                expires_at: session.expires_at,
            })
            .await
            .map_err(storage_error)
    }

    async fn insert_execution(
        &self,
        result_ref: String,
        execution: ExecutionSelectionDto,
    ) -> Result<(), SearchError> {
        let payload = serde_json::to_value(execution).map_err(|_| SearchError::Infrastructure)?;
        self.repository
            .insert_execution(&result_ref, payload)
            .await
            .map_err(storage_error)
    }

    async fn execution_for(&self, result_ref: &str) -> Result<ExecutionSelectionDto, SearchError> {
        let payload = self
            .repository
            .execution_for(result_ref)
            .await
            .map_err(storage_error)?
            .ok_or(SearchError::NotFound)?;
        serde_json::from_value(payload).map_err(|_| SearchError::Infrastructure)
    }
}

fn encode_session_payload(session: &StoredSearchSession) -> Result<serde_json::Value, SearchError> {
    let results = session
        .results
        .iter()
        .map(|result| {
            let private = match &result.private {
                PrivateResult::Rezka {
                    locator,
                    title_id,
                    translation_episodes,
                } => serde_json::json!({
                    "kind": "rezka",
                    "locator": locator,
                    "title_id": title_id,
                    "translation_episodes": translation_episodes,
                }),
                PrivateResult::Prowlarr {
                    source_identity,
                    info_hash,
                    uri,
                } => serde_json::json!({
                    "kind": "prowlarr",
                    "source_identity": source_identity,
                    "info_hash": info_hash,
                    "uri": uri,
                }),
            };
            Ok(serde_json::json!({
                "public": serde_json::to_value(&result.public)
                    .map_err(|_| SearchError::Infrastructure)?,
                "private": private,
            }))
        })
        .collect::<Result<Vec<_>, SearchError>>()?;
    Ok(serde_json::json!({
        "request": serde_json::to_value(&session.request)
            .map_err(|_| SearchError::Infrastructure)?,
        "results": results,
        "provider_continuation": session.provider_continuation,
    }))
}

fn decode_session_payload(
    value: serde_json::Value,
) -> Result<(StartSearchRequest, Vec<ProviderResult>, Option<String>), SearchError> {
    let object = value.as_object().ok_or(SearchError::Infrastructure)?;
    let request = serde_json::from_value(
        object
            .get("request")
            .cloned()
            .ok_or(SearchError::Infrastructure)?,
    )
    .map_err(|_| SearchError::Infrastructure)?;
    let provider_continuation = object
        .get("provider_continuation")
        .cloned()
        .map(serde_json::from_value::<Option<String>>)
        .transpose()
        .map_err(|_| SearchError::Infrastructure)?
        .flatten();
    let values = object
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or(SearchError::Infrastructure)?;
    let mut results = Vec::with_capacity(values.len());
    for value in values {
        let value = value.as_object().ok_or(SearchError::Infrastructure)?;
        let public = serde_json::from_value(
            value
                .get("public")
                .cloned()
                .ok_or(SearchError::Infrastructure)?,
        )
        .map_err(|_| SearchError::Infrastructure)?;
        let private = value
            .get("private")
            .and_then(serde_json::Value::as_object)
            .ok_or(SearchError::Infrastructure)?;
        let string = |name| {
            private
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or(SearchError::Infrastructure)
        };
        let private = match private.get("kind").and_then(serde_json::Value::as_str) {
            Some("rezka") => PrivateResult::Rezka {
                locator: string("locator")?,
                title_id: private
                    .get("title_id")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or(SearchError::Infrastructure)?,
                translation_episodes: serde_json::from_value(
                    private
                        .get("translation_episodes")
                        .cloned()
                        .ok_or(SearchError::Infrastructure)?,
                )
                .map_err(|_| SearchError::Infrastructure)?,
            },
            Some("prowlarr") => PrivateResult::Prowlarr {
                source_identity: string("source_identity")?,
                info_hash: string("info_hash")?,
                uri: string("uri")?,
            },
            _ => return Err(SearchError::Infrastructure),
        };
        results.push(ProviderResult { public, private });
    }
    Ok((request, results, provider_continuation))
}

fn storage_error(error: media_core::PortError) -> SearchError {
    match error {
        media_core::PortError::Conflict => SearchError::Conflict,
        media_core::PortError::Infrastructure => SearchError::Infrastructure,
    }
}

pub struct DurableSearchService {
    persistence: Arc<dyn SearchPersistence>,
    provider: Arc<dyn SearchProvider>,
    jobs: Arc<JobApplication>,
}

impl DurableSearchService {
    #[must_use]
    pub fn new(
        persistence: Arc<dyn SearchPersistence>,
        provider: Arc<dyn SearchProvider>,
        jobs: Arc<JobApplication>,
    ) -> Self {
        Self {
            persistence,
            provider,
            jobs,
        }
    }

    fn validate_request(request: &StartSearchRequest) -> Result<(), SearchError> {
        if request.query.trim().is_empty()
            || request.query.len() > 512
            || request.query.chars().any(char::is_control)
            || (request.source == ProviderDto::Rezka && request.season.is_some())
            || (request.source == ProviderDto::Prowlarr
                && !matches!(
                    (request.media_kind, request.season),
                    (Some(MediaKindDto::Movie), None) | (Some(MediaKindDto::Series), Some(_))
                ))
            || request.season == Some(0)
        {
            return Err(SearchError::InvalidRequest);
        }
        Ok(())
    }

    async fn page(
        &self,
        mut session: StoredSearchSession,
        offset: usize,
    ) -> Result<SearchPageDto, SearchError> {
        if session.expires_at <= OffsetDateTime::now_utc() {
            return Err(SearchError::NotFound);
        }
        if offset > session.results.len() {
            return Err(SearchError::InvalidRequest);
        }
        if offset == session.results.len() {
            let continuation = session
                .provider_continuation
                .clone()
                .ok_or(SearchError::NotFound)?;
            let next = self
                .provider
                .search(&session.request, Some(&continuation))
                .await?;
            if next.results.is_empty() {
                return Err(SearchError::NotFound);
            }
            session.results.extend(next.results);
            session.provider_continuation = next.provider_continuation;
            self.persistence.update_session(session.clone()).await?;
        }
        let end = (offset + MAX_SEARCH_RESULTS_PER_PAGE).min(session.results.len());
        let continuation = (end < session.results.len() || session.provider_continuation.is_some())
            .then(|| format!("{}:{end}", session.id));
        Ok(SearchPageDto {
            api_version: "v1".to_owned(),
            session_id: session.id,
            source: session.request.source,
            expires_at: session
                .expires_at
                .format(&Rfc3339)
                .map_err(|_| SearchError::Infrastructure)?,
            results: session.results[offset..end]
                .iter()
                .map(|result| result.public.clone())
                .collect(),
            continuation,
        })
    }
}

#[async_trait::async_trait]
impl SearchService for DurableSearchService {
    async fn refresh_rezka_session(
        &self,
        owner: UserId,
        operation: OperationKey,
        request: media_contract::RezkaSessionRefreshRequest,
    ) -> Result<JobDto, SearchError> {
        if request.credential_request_id.trim().is_empty()
            || request.credential_request_id.len() > 256
            || request.credential_request_id.chars().any(char::is_control)
        {
            return Err(SearchError::InvalidRequest);
        }
        let result_ref = format!("selection:session-refresh:{}", uuid::Uuid::new_v4());
        self.persistence
            .insert_execution(
                result_ref.clone(),
                ExecutionSelectionDto::RezkaSessionRefresh {
                    credential_request_id: request.credential_request_id,
                },
            )
            .await?;
        let job = self
            .jobs
            .create_job_for_owner(
                owner,
                operation,
                NewJobCommand {
                    provider: Provider::Rezka,
                    result_ref,
                    notify_scope: NotifyScope::Initiator,
                },
            )
            .await
            .map_err(|error| match error {
                media_core::ApplicationError::Conflict => SearchError::Conflict,
                media_core::ApplicationError::InvalidInput(_) => SearchError::InvalidRequest,
                media_core::ApplicationError::Forbidden => SearchError::Forbidden,
                media_core::ApplicationError::NotFound => SearchError::NotFound,
                media_core::ApplicationError::Infrastructure => SearchError::Infrastructure,
            })?;
        Ok(job_dto(&job))
    }

    async fn start(
        &self,
        owner: UserId,
        request: StartSearchRequest,
    ) -> Result<SearchPageDto, SearchError> {
        Self::validate_request(&request)?;
        let provider_page = self.provider.search(&request, None).await?;
        let session = StoredSearchSession {
            id: uuid::Uuid::new_v4().to_string(),
            owner,
            request,
            expires_at: OffsetDateTime::now_utc() + SEARCH_TTL,
            results: provider_page.results,
            provider_continuation: provider_page.provider_continuation,
        };
        self.persistence.insert_session(session.clone()).await?;
        self.page(session, 0).await
    }

    async fn continue_search(
        &self,
        owner: UserId,
        request: ContinueSearchRequest,
    ) -> Result<SearchPageDto, SearchError> {
        let (session_id, offset) = request
            .continuation
            .rsplit_once(':')
            .ok_or(SearchError::InvalidRequest)?;
        let offset = offset
            .parse::<usize>()
            .map_err(|_| SearchError::InvalidRequest)?;
        if offset == 0 {
            return Err(SearchError::InvalidRequest);
        }
        let session = self
            .persistence
            .session_for_owner(session_id, owner)
            .await?;
        if session.request.scope != request.scope {
            return Err(SearchError::Forbidden);
        }
        self.page(session, offset).await
    }

    async fn select(
        &self,
        owner: UserId,
        operation: OperationKey,
        request: SelectResultRequest,
    ) -> Result<JobDto, SearchError> {
        let session = self
            .persistence
            .session_for_owner(&request.session_id, owner)
            .await?;
        if session.request.scope != request.scope {
            return Err(SearchError::Forbidden);
        }
        if session.expires_at <= OffsetDateTime::now_utc() {
            return Err(SearchError::NotFound);
        }
        let result = session
            .results
            .iter()
            .find(|result| result.public.result_id() == request.result_id)
            .ok_or(SearchError::NotFound)?;
        let execution = execution(
            result,
            &request,
            session.request.media_kind,
            session.request.season,
        )?;
        let result_ref = format!("selection:{}", uuid::Uuid::new_v4());
        self.persistence
            .insert_execution(result_ref.clone(), execution)
            .await?;
        let provider = match session.request.source {
            ProviderDto::Rezka => Provider::Rezka,
            ProviderDto::Prowlarr => Provider::Prowlarr,
        };
        let job = self
            .jobs
            .create_job_for_owner(
                owner,
                operation,
                NewJobCommand {
                    provider,
                    result_ref,
                    notify_scope: NotifyScope::Initiator,
                },
            )
            .await
            .map_err(|error| match error {
                media_core::ApplicationError::Conflict => SearchError::Conflict,
                media_core::ApplicationError::InvalidInput(_) => SearchError::InvalidRequest,
                media_core::ApplicationError::Forbidden => SearchError::Forbidden,
                media_core::ApplicationError::NotFound => SearchError::NotFound,
                media_core::ApplicationError::Infrastructure => SearchError::Infrastructure,
            })?;
        Ok(job_dto(&job))
    }

    async fn execution_for(&self, result_ref: &str) -> Result<ExecutionSelectionDto, SearchError> {
        self.persistence.execution_for(result_ref).await
    }
}

fn execution(
    result: &ProviderResult,
    request: &SelectResultRequest,
    searched_kind: Option<MediaKindDto>,
    searched_season: Option<u16>,
) -> Result<ExecutionSelectionDto, SearchError> {
    match (&result.public, &result.private) {
        (
            SearchResultDto::Prowlarr { title, .. },
            PrivateResult::Prowlarr {
                source_identity,
                info_hash,
                uri,
            },
        ) => {
            if request.translation_id.is_some()
                || request.season.is_some()
                || request.episode.is_some()
            {
                return Err(SearchError::InvalidRequest);
            }
            Ok(ExecutionSelectionDto::Prowlarr {
                source_identity: source_identity.clone(),
                info_hash: info_hash.clone(),
                uri: uri.clone(),
                media_kind: searched_kind.ok_or(SearchError::Infrastructure)?,
                season: searched_season,
                title: title.clone(),
            })
        }
        (
            SearchResultDto::Rezka {
                title,
                media_kind,
                translations,
                ..
            },
            PrivateResult::Rezka {
                locator,
                title_id,
                translation_episodes,
            },
        ) => {
            let translation_id = request.translation_id.ok_or(SearchError::InvalidRequest)?;
            let translation = translations
                .iter()
                .find(|item| item.id == translation_id)
                .ok_or(SearchError::InvalidRequest)?;
            match media_kind {
                MediaKindDto::Movie if request.season.is_some() || request.episode.is_some() => {
                    return Err(SearchError::InvalidRequest);
                }
                MediaKindDto::Series => match (request.season, request.episode) {
                    (Some(season), Some(episode)) => {
                        let available =
                            translation_episodes
                                .get(&translation_id)
                                .is_some_and(|seasons| {
                                    seasons.iter().any(|(candidate, episodes)| {
                                        *candidate == season && episodes.contains(&episode)
                                    })
                                });
                        if !available {
                            return Err(SearchError::InvalidRequest);
                        }
                    }
                    (None, None) => {
                        if !translation_episodes
                            .get(&translation_id)
                            .is_some_and(|seasons| {
                                seasons.iter().any(|(_, episodes)| !episodes.is_empty())
                            })
                        {
                            return Err(SearchError::InvalidRequest);
                        }
                    }
                    _ => return Err(SearchError::InvalidRequest),
                },
                MediaKindDto::Movie => {}
            }
            let episodes = match media_kind {
                MediaKindDto::Movie => Vec::new(),
                MediaKindDto::Series => match (request.season, request.episode) {
                    (Some(season), Some(episode)) => {
                        vec![media_contract::EpisodeSnapshotDto { season, episode }]
                    }
                    (None, None) => translation_episodes
                        .get(&translation_id)
                        .into_iter()
                        .flatten()
                        .flat_map(|(season, episodes)| {
                            episodes
                                .iter()
                                .map(|episode| media_contract::EpisodeSnapshotDto {
                                    season: *season,
                                    episode: *episode,
                                })
                        })
                        .collect(),
                    _ => return Err(SearchError::InvalidRequest),
                },
            };
            Ok(ExecutionSelectionDto::Rezka {
                locator: locator.clone(),
                title_id: *title_id,
                media_kind: *media_kind,
                translation_id,
                director: translation.director,
                camrip: translation.camrip,
                has_ads: translation.has_ads,
                season: request.season,
                episode: request.episode,
                episodes,
                title: title.clone(),
            })
        }
        _ => Err(SearchError::Infrastructure),
    }
}

fn job_dto(job: &Job) -> JobDto {
    JobDto {
        id: media_contract::PublicId::parse(&job.id().to_string())
            .expect("validated job IDs are public UUIDs"),
        provider: match job.provider() {
            Provider::Rezka => ProviderDto::Rezka,
            Provider::Prowlarr => ProviderDto::Prowlarr,
        },
        result_ref: job.result_ref().to_owned(),
        state: match job.state() {
            JobState::Queued => JobStateDto::Queued,
            JobState::Leased => JobStateDto::Leased,
            JobState::Running => JobStateDto::Running,
            JobState::CancelRequested => JobStateDto::CancelRequested,
            JobState::BlockedStorage => JobStateDto::BlockedStorage,
            JobState::Publishing => JobStateDto::Publishing,
            JobState::PlexPending => JobStateDto::PlexPending,
            JobState::NeedsAction => JobStateDto::NeedsAction,
            JobState::Partial => JobStateDto::Partial,
            JobState::Completed => JobStateDto::Completed,
            JobState::Failed => JobStateDto::Failed,
            JobState::Cancelled => JobStateDto::Cancelled,
        },
        needs_action_reason: None,
        notify_scope: NotifyScopeDto::Initiator,
    }
}
