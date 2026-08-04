use std::{collections::BTreeMap, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use media_api::{SearchError, SearchService};
use media_contract::{
    AlternativeSearchRequest, ContinueSearchRequest, EpisodeMappingActionDto,
    ExecutionSelectionDto, JobDto, JobStateDto, MAX_SEARCH_RESULTS_PER_PAGE, MediaKindDto,
    NotifyScopeDto, ProviderDto, ResolveEpisodeMappingRequest, SearchPageDto, SearchResultDto,
    SelectResultRequest, StartSearchRequest,
};
use media_core::{
    EpisodeAvailability, EpisodeAvailabilityPort, EpisodeAvailabilityRequest, EpisodeDiscovery,
    EpisodeDiscoveryPort, EpisodeMappingConfirmation, EpisodeSnapshot, IdentityStore, Job,
    JobApplication, JobId, JobState, NeedsActionReason, NewJobCommand, NotifyScope, OperationKey,
    PortError, Provider, ProviderAvailability, ReleaseMetadataPort, ReleaseMetadataResult,
    ReleasePrecision, ReleaseQuery, ScheduledEpisode, TrackedEpisodeDownloadPort,
    TrackingSubscription, UserId,
};
use sha2::{Digest as _, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const SEARCH_TTL: time::Duration = time::Duration::hours(24);
const REZKA_CATALOG_CONTINUATION_PREFIX: &str = "catalog:";
const REZKA_AUTH_ATTEMPTS: usize = 3;
const REZKA_AUTH_RETRY_DELAY: Duration = Duration::from_millis(500);

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
        translation_episode_labels: BTreeMap<u64, Vec<ProviderEpisodeLabel>>,
    },
    Prowlarr {
        source_identity: String,
        info_hash: String,
        uri: String,
    },
}

type ProviderEpisodeLabel = media_contract::AmbiguousEpisodeDto;

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
                translation_episode_labels: BTreeMap::new(),
            },
        }
    }

    fn rezka_with_availability(
        public: SearchResultDto,
        locator: String,
        title_id: u64,
        translation_episodes: BTreeMap<u64, Vec<(u32, Vec<u32>)>>,
        translation_episode_labels: BTreeMap<u64, Vec<ProviderEpisodeLabel>>,
    ) -> Self {
        Self {
            public,
            private: PrivateResult::Rezka {
                locator,
                title_id,
                translation_episodes,
                translation_episode_labels,
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
    async fn update_execution(
        &self,
        result_ref: &str,
        execution: ExecutionSelectionDto,
    ) -> Result<(), SearchError>;
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
    release: Option<Arc<dyn ReleaseMetadataPort>>,
}

impl ProviderEpisodeDiscovery {
    #[must_use]
    pub fn new(provider: Arc<dyn SearchProvider>) -> Self {
        Self {
            provider,
            release: None,
        }
    }

    #[must_use]
    pub fn with_release(
        provider: Arc<dyn SearchProvider>,
        release: Arc<dyn ReleaseMetadataPort>,
    ) -> Self {
        Self {
            provider,
            release: Some(release),
        }
    }
}

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for ProviderEpisodeDiscovery {
    async fn available_episodes(
        &self,
        tracking: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        if tracking.translation() == "release-calendar" {
            return self.release_episodes(tracking).await;
        }

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
        let result =
            page.results
                .into_iter()
                .find(|result| match (&result.public, &result.private) {
                    (
                        SearchResultDto::Rezka { title, .. },
                        PrivateResult::Rezka { title_id, .. },
                    ) => tracking.download().map_or_else(
                        || title.trim().eq_ignore_ascii_case(tracking.title().trim()),
                        |download| download.provider_media_ref() == title_id.to_string(),
                    ),
                    _ => false,
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
        let translation_id = tracking.download().map_or_else(
            || {
                translations
                    .iter()
                    .find(|translation| {
                        translation
                            .name
                            .trim()
                            .eq_ignore_ascii_case(tracking.translation().trim())
                    })
                    .map(|translation| translation.id)
                    .ok_or(PortError::Infrastructure)
            },
            |download| {
                translations
                    .iter()
                    .any(|translation| translation.id == download.translation_id())
                    .then_some(download.translation_id())
                    .ok_or(PortError::Infrastructure)
            },
        )?;
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
        EpisodeDiscovery::new(episodes, tracking.title().to_owned(), None)
            .map_err(|_| PortError::Conflict)
    }
}

pub struct ProviderEpisodeAvailability {
    provider: Arc<dyn SearchProvider>,
    prowlarr: Option<media_integrations::prowlarr::ProwlarrClient>,
}

impl ProviderEpisodeAvailability {
    #[must_use]
    pub fn new(
        provider: Arc<dyn SearchProvider>,
        prowlarr: Option<media_integrations::prowlarr::ProwlarrClient>,
    ) -> Self {
        Self { provider, prowlarr }
    }

    async fn rezka_availability(
        &self,
        request: &EpisodeAvailabilityRequest<'_>,
    ) -> ProviderAvailability {
        let titles = rezka_availability_titles(request);
        let mut failed = false;
        for query in &titles {
            let page = self
                .provider
                .search(
                    &StartSearchRequest {
                        scope: media_contract::SearchScopeDto {
                            platform: "system".to_owned(),
                            chat_id: "tracking-availability".to_owned(),
                            thread_id: None,
                        },
                        source: ProviderDto::Rezka,
                        query: query.clone(),
                        media_kind: Some(MediaKindDto::Series),
                        season: None,
                        preferred_qualities: Vec::new(),
                        preferred_languages: Vec::new(),
                        preferred_codecs: Vec::new(),
                        preferred_release_groups: Vec::new(),
                    },
                    None,
                )
                .await;
            let Ok(page) = page else {
                failed = true;
                continue;
            };
            if page.results.iter().any(|result| {
                let (
                    SearchResultDto::Rezka {
                        title,
                        original_title,
                        ..
                    },
                    PrivateResult::Rezka {
                        translation_episodes,
                        ..
                    },
                ) = (&result.public, &result.private)
                else {
                    return false;
                };
                let matching_title = titles.iter().any(|candidate| {
                    title.trim().eq_ignore_ascii_case(candidate.trim())
                        || original_title.as_deref().is_some_and(|original| {
                            original.trim().eq_ignore_ascii_case(candidate.trim())
                        })
                });
                matching_title
                    && translation_episodes.values().any(|seasons| {
                        seasons.iter().any(|(season, episodes)| {
                            *season == request.episode().season()
                                && episodes.contains(&request.episode().episode())
                        })
                    })
            }) {
                return ProviderAvailability::Available;
            }
        }
        if failed {
            ProviderAvailability::Unknown
        } else {
            ProviderAvailability::Unavailable
        }
    }

    async fn prowlarr_availability(
        &self,
        request: &EpisodeAvailabilityRequest<'_>,
    ) -> ProviderAvailability {
        let Some(client) = &self.prowlarr else {
            return ProviderAvailability::Unknown;
        };
        let query = media_integrations::prowlarr::EpisodeAvailabilityQuery::new(
            prowlarr_availability_titles(request),
            request.episode().season(),
            request.episode().episode(),
        );
        let Ok(query) = query else {
            return ProviderAvailability::Unknown;
        };
        match client.episode_available(&query).await {
            Ok(true) => ProviderAvailability::Available,
            Ok(false) => ProviderAvailability::Unavailable,
            Err(_) => ProviderAvailability::Unknown,
        }
    }
}

#[async_trait::async_trait]
impl EpisodeAvailabilityPort for ProviderEpisodeAvailability {
    async fn probe(
        &self,
        request: EpisodeAvailabilityRequest<'_>,
    ) -> Result<EpisodeAvailability, PortError> {
        let (rezka, prowlarr) = tokio::join!(
            self.rezka_availability(&request),
            self.prowlarr_availability(&request)
        );
        Ok(EpisodeAvailability::new(rezka, prowlarr))
    }
}

fn rezka_availability_titles(request: &EpisodeAvailabilityRequest<'_>) -> Vec<String> {
    distinct_titles([
        Some(request.tracking().title()),
        Some(request.discovery().release_title()),
        request.discovery().original_release_title(),
    ])
}

fn prowlarr_availability_titles(request: &EpisodeAvailabilityRequest<'_>) -> Vec<String> {
    distinct_titles([
        request.discovery().original_release_title(),
        Some(request.discovery().release_title()),
        Some(request.tracking().title()),
    ])
}

fn distinct_titles<const N: usize>(values: [Option<&str>; N]) -> Vec<String> {
    let mut titles = Vec::new();
    for value in values.into_iter().flatten() {
        let value = value.trim();
        if !value.is_empty()
            && !titles
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(value))
        {
            titles.push(value.to_owned());
        }
    }
    titles
}

pub struct TrackedEpisodeDownloader {
    provider: Arc<dyn SearchProvider>,
    persistence: Arc<dyn SearchPersistence>,
    jobs: Arc<JobApplication>,
    identity: Option<Arc<dyn IdentityStore>>,
}

impl TrackedEpisodeDownloader {
    #[must_use]
    pub fn new(
        provider: Arc<dyn SearchProvider>,
        persistence: Arc<dyn SearchPersistence>,
        jobs: Arc<JobApplication>,
    ) -> Self {
        Self {
            provider,
            persistence,
            jobs,
            identity: None,
        }
    }

    #[must_use]
    pub fn with_identity(mut self, identity: Arc<dyn IdentityStore>) -> Self {
        self.identity = Some(identity);
        self
    }
}

#[async_trait::async_trait]
impl TrackedEpisodeDownloadPort for TrackedEpisodeDownloader {
    async fn enqueue_episode(
        &self,
        tracking: &TrackingSubscription,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        let download = tracking.download().ok_or(PortError::Conflict)?;
        if episode.season() != download.season() {
            return Err(PortError::Conflict);
        }
        let page = self
            .provider
            .search(
                &StartSearchRequest {
                    scope: media_contract::SearchScopeDto {
                        platform: "system".to_owned(),
                        chat_id: "tracking-download".to_owned(),
                        thread_id: None,
                    },
                    source: ProviderDto::Rezka,
                    query: tracking.title().to_owned(),
                    media_kind: Some(MediaKindDto::Series),
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
        let result = page
            .results
            .iter()
            .find(|result| {
                matches!(
                    &result.private,
                    PrivateResult::Rezka { title_id, .. }
                        if download.provider_media_ref() == title_id.to_string()
                )
            })
            .ok_or(PortError::Infrastructure)?;
        let request = SelectResultRequest {
            session_id: "tracking".to_owned(),
            result_id: result.public.result_id().to_owned(),
            translation_id: Some(download.translation_id()),
            season: Some(episode.season()),
            episode: Some(episode.episode()),
            scope: media_contract::SearchScopeDto {
                platform: "system".to_owned(),
                chat_id: "tracking-download".to_owned(),
                thread_id: None,
            },
        };
        let mut execution = execution(
            result,
            &request,
            Some(MediaKindDto::Series),
            None,
            Some(tracking.title()),
        )
        .map_err(|_| PortError::Infrastructure)?;
        if let Some(identity) = self.identity.as_deref() {
            apply_persisted_episode_mappings(identity, &mut execution)
                .await
                .map_err(|_| PortError::Infrastructure)?;
        }
        let result_ref = format!(
            "selection:tracking:{}:{}:{}",
            tracking.id(),
            episode.season(),
            episode.episode()
        );
        self.persistence
            .insert_execution(result_ref.clone(), execution)
            .await
            .map_err(|_| PortError::Infrastructure)?;
        let operation: [u8; 32] = Sha256::digest(
            format!(
                "tracking-download:v1:{}:{}:{}",
                tracking.id(),
                episode.season(),
                episode.episode()
            )
            .as_bytes(),
        )
        .into();
        self.jobs
            .create_job_for_owner(
                tracking.owner_id(),
                OperationKey::from_bytes(operation),
                NewJobCommand {
                    provider: Provider::Rezka,
                    result_ref,
                    notify_scope: NotifyScope::Initiator,
                },
            )
            .await
            .map_err(|_| PortError::Infrastructure)?;
        Ok(())
    }
}

impl ProviderEpisodeDiscovery {
    async fn release_episodes(
        &self,
        tracking: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        let release = self.release.as_ref().ok_or(PortError::Infrastructure)?;
        let query =
            ReleaseQuery::new(tracking.title(), None, None).map_err(|_| PortError::Conflict)?;
        let result = release
            .query(&query)
            .await
            .map_err(|_| PortError::Infrastructure)?;
        let ReleaseMetadataResult::Matched { show, schedule, .. } = result else {
            return Err(PortError::Conflict);
        };
        let now = OffsetDateTime::now_utc();
        let mut episodes = schedule
            .into_iter()
            .filter(|episode| release_episode_has_aired(episode, now))
            .filter_map(|episode| EpisodeSnapshot::new(episode.season, episode.episode).ok())
            .collect::<Vec<_>>();
        episodes.sort_unstable();
        episodes.dedup();
        EpisodeDiscovery::new(episodes, show.title, show.original_title)
            .map_err(|_| PortError::Conflict)
    }
}

fn release_episode_has_aired(episode: &ScheduledEpisode, now: OffsetDateTime) -> bool {
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
        for attempt in 1..=REZKA_AUTH_ATTEMPTS {
            let result = prepared
                .client
                .ensure_authenticated(
                    prepared
                        .credentials
                        .as_ref()
                        .ok_or(SearchError::Infrastructure)?,
                    &prepared.probe,
                )
                .await;
            match result {
                Ok(_) => break,
                Err(error)
                    if retryable_rezka_auth_error(error.code())
                        && attempt < REZKA_AUTH_ATTEMPTS =>
                {
                    tracing::warn!(
                        stage = "authentication",
                        attempt,
                        error_code = ?error.code(),
                        "temporary Rezka authentication failure; retrying with a fresh session"
                    );
                    tokio::time::sleep(REZKA_AUTH_RETRY_DELAY).await;
                    prepared
                        .reload_session()
                        .map_err(|_| SearchError::Infrastructure)?;
                }
                Err(error) => {
                    tracing::warn!(stage = "authentication", error_code = ?error.code(), error = %error, "Rezka search failed");
                    return Err(SearchError::Provider);
                }
            }
        }
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
        let (offset, current_target) = continuation.map_or(Ok((0, None)), |value| {
            decode_rezka_catalog_continuation(value)
        })?;
        let query = rezka_client::CatalogQuery::new(&request.query)
            .map_err(|_| SearchError::InvalidRequest)?;
        let page = match current_target.as_deref() {
            Some(target) => {
                let continuation = rezka_client::CatalogContinuation::new(target, &request.query)
                    .map_err(|_| SearchError::InvalidRequest)?;
                prepared.client.search_next(&continuation).await
            }
            None => prepared.client.search(&query).await,
        }
        .map_err(|error| {
            tracing::warn!(stage = "catalog_search", error_code = ?error.code(), error = %error, "Rezka search failed");
            SearchError::Provider
        })?;
        let entries = page.entries();
        if offset > entries.len() {
            return Err(SearchError::InvalidRequest);
        }
        let mut results = Vec::new();
        let mut cursor = offset;
        let mut title_failures = 0_usize;
        let mut usable_titles = 0_usize;
        'entries: while cursor < entries.len() && results.len() < MAX_SEARCH_RESULTS_PER_PAGE {
            let entry = &entries[cursor];
            cursor += 1;
            let details = match prepared.client.title(entry.locator()).await {
                Ok(details) => details,
                Err(error) if skippable_title_error(error.code()) => {
                    title_failures += 1;
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
            usable_titles += 1;
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
            let mut labels_by_translation = BTreeMap::new();
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
                    let selection = selection.map_err(|error| {
                        tracing::warn!(stage = "translation", error_code = ?error.code(), error = %error, "Rezka search failed");
                        SearchError::Provider
                    })?;
                    let availability = match prepared.client.series_availability(&selection).await {
                        Ok(availability) => availability,
                        Err(error) => {
                            tracing::warn!(
                                stage = "availability",
                                translation_id,
                                error_code = ?error.code(),
                                error = %error,
                                "skipping unavailable Rezka translation"
                            );
                            continue;
                        }
                    };
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
                    labels_by_translation.insert(
                        translation_id,
                        availability
                            .seasons()
                            .iter()
                            .flat_map(|season| {
                                season
                                    .episodes()
                                    .iter()
                                    .map(move |episode| ProviderEpisodeLabel {
                                        provider: media_contract::EpisodeCoordinateDto {
                                            season: season.number(),
                                            episode: episode.number(),
                                        },
                                        label: episode.label().to_owned(),
                                    })
                            })
                            .collect(),
                    );
                }
                if by_translation.is_empty() {
                    tracing::warn!(
                        stage = "availability",
                        title_id = details.id().get(),
                        "skipping Rezka series with no usable translation availability"
                    );
                    continue 'entries;
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
                labels_by_translation,
            ));
        }
        if results.is_empty() && title_failures > 0 && usable_titles == 0 {
            return Err(SearchError::Provider);
        }
        let provider_continuation = if cursor < entries.len() {
            Some(encode_rezka_catalog_continuation(
                cursor,
                current_target.as_deref(),
            ))
        } else {
            page.continuation().map(|continuation| {
                encode_rezka_catalog_continuation(0, Some(continuation.as_str()))
            })
        };
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
                if error.code()
                    == media_integrations::prowlarr::ProwlarrErrorCode::TemporarilyUnavailable
                {
                    SearchError::ProviderUnavailable
                } else {
                    SearchError::Provider
                }
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

fn skippable_title_error(code: rezka_client::RezkaErrorCode) -> bool {
    matches!(
        code,
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
            | rezka_client::RezkaErrorCode::TitleNotFound
            | rezka_client::RezkaErrorCode::Transport
    )
}

fn retryable_rezka_auth_error(code: rezka_client::RezkaErrorCode) -> bool {
    code == rezka_client::RezkaErrorCode::Transport
}

fn encode_rezka_catalog_continuation(offset: usize, target: Option<&str>) -> String {
    let target = target.map_or_else(String::new, |value| URL_SAFE_NO_PAD.encode(value));
    format!("{REZKA_CATALOG_CONTINUATION_PREFIX}{offset}:{target}")
}

fn decode_rezka_catalog_continuation(value: &str) -> Result<(usize, Option<String>), SearchError> {
    let (offset, target) = value
        .strip_prefix(REZKA_CATALOG_CONTINUATION_PREFIX)
        .and_then(|value| value.split_once(':'))
        .ok_or(SearchError::InvalidRequest)?;
    let offset = offset
        .parse::<usize>()
        .map_err(|_| SearchError::InvalidRequest)?;
    let target = if target.is_empty() {
        None
    } else {
        let bytes = URL_SAFE_NO_PAD
            .decode(target)
            .map_err(|_| SearchError::InvalidRequest)?;
        Some(String::from_utf8(bytes).map_err(|_| SearchError::InvalidRequest)?)
    };
    Ok((offset, target))
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

    async fn update_execution(
        &self,
        result_ref: &str,
        execution: ExecutionSelectionDto,
    ) -> Result<(), SearchError> {
        let payload = serde_json::to_value(execution).map_err(|_| SearchError::Infrastructure)?;
        self.repository
            .update_execution(result_ref, payload)
            .await
            .map_err(storage_error)
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
                    translation_episode_labels,
                } => serde_json::json!({
                    "kind": "rezka",
                    "locator": locator,
                    "title_id": title_id,
                    "translation_episodes": translation_episodes,
                    "translation_episode_labels": translation_episode_labels,
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
                translation_episode_labels: private
                    .get("translation_episode_labels")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|_| SearchError::Infrastructure)?
                    .unwrap_or_default(),
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
    identity: Option<Arc<dyn IdentityStore>>,
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
            identity: None,
        }
    }

    #[must_use]
    pub fn with_identity(mut self, identity: Arc<dyn IdentityStore>) -> Self {
        self.identity = Some(identity);
        self
    }

    fn validate_request(request: &StartSearchRequest) -> Result<(), SearchError> {
        if request.query.trim().is_empty()
            || request.query.len() > 512
            || request.query.chars().any(char::is_control)
            || (request.source == ProviderDto::Prowlarr
                && !matches!(
                    (request.media_kind, request.season),
                    (Some(MediaKindDto::Movie), None) | (Some(MediaKindDto::Series), Some(_))
                ))
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

    async fn start_alternative(
        &self,
        owner: UserId,
        job_id: JobId,
        request: AlternativeSearchRequest,
    ) -> Result<SearchPageDto, SearchError> {
        let job = self
            .jobs
            .get_job_for_owner(owner, job_id)
            .await
            .map_err(application_error)?;
        let execution = self.persistence.execution_for(job.result_ref()).await?;
        let (source, query, media_kind, season) = match execution {
            ExecutionSelectionDto::Rezka {
                media_kind,
                season,
                episodes,
                title,
                ..
            } => {
                let season = season
                    .or_else(|| common_episode_season(&episodes))
                    .map(|value| u16::try_from(value).map_err(|_| SearchError::InvalidRequest))
                    .transpose()?;
                if media_kind == MediaKindDto::Series && season.is_none() {
                    return Err(SearchError::InvalidRequest);
                }
                (ProviderDto::Prowlarr, title, media_kind, season)
            }
            ExecutionSelectionDto::Prowlarr {
                media_kind,
                season,
                title,
                ..
            } => (ProviderDto::Rezka, title, media_kind, season),
            ExecutionSelectionDto::RezkaSessionRefresh { .. } => {
                return Err(SearchError::Conflict);
            }
        };
        self.start(
            owner,
            StartSearchRequest {
                scope: request.scope,
                source,
                query,
                media_kind: Some(media_kind),
                season,
                preferred_qualities: Vec::new(),
                preferred_languages: Vec::new(),
                preferred_codecs: Vec::new(),
                preferred_release_groups: Vec::new(),
            },
        )
        .await
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
        let mut execution = execution(
            result,
            &request,
            session.request.media_kind,
            session.request.season,
            Some(&session.request.query),
        )?;
        if let Some(identity) = self.identity.as_deref() {
            apply_persisted_episode_mappings(identity, &mut execution).await?;
        }
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

    async fn episode_mapping_action(
        &self,
        owner: UserId,
        job_id: JobId,
    ) -> Result<EpisodeMappingActionDto, SearchError> {
        let job = self
            .jobs
            .get_job_for_owner(owner, job_id)
            .await
            .map_err(application_error)?;
        if job.state() != JobState::NeedsAction
            || job.needs_action_reason() != Some(NeedsActionReason::IdentityAmbiguous)
        {
            return Err(SearchError::Conflict);
        }
        let execution = self.persistence.execution_for(job.result_ref()).await?;
        mapping_action_dto(job_id, &execution)
    }

    async fn resolve_episode_mapping(
        &self,
        owner: UserId,
        operation: OperationKey,
        job_id: JobId,
        request: ResolveEpisodeMappingRequest,
    ) -> Result<JobDto, SearchError> {
        if request.canonical_episode == 0
            || request.canonical_title.as_ref().is_some_and(|title| {
                title.trim().is_empty() || title.len() > 512 || title.chars().any(char::is_control)
            })
        {
            return Err(SearchError::InvalidRequest);
        }
        let identity = self
            .identity
            .as_deref()
            .ok_or(SearchError::Infrastructure)?;
        let job = self
            .jobs
            .get_job_for_owner(owner, job_id)
            .await
            .map_err(application_error)?;
        if job.state() != JobState::NeedsAction
            || job.needs_action_reason() != Some(NeedsActionReason::IdentityAmbiguous)
        {
            return Err(SearchError::Conflict);
        }
        let mut execution = self.persistence.execution_for(job.result_ref()).await?;
        let action = mapping_action_dto(job_id, &execution)?;
        let ExecutionSelectionDto::Rezka {
            title_id,
            title,
            release_year,
            episode_mappings,
            ambiguous_episodes,
            ..
        } = &mut execution
        else {
            return Err(SearchError::Conflict);
        };
        let canonical = identity
            .confirm_episode_mapping(
                EpisodeMappingConfirmation::new(
                    Provider::Rezka,
                    title_id.to_string(),
                    action.provider.season,
                    action.provider.episode,
                    request
                        .canonical_title
                        .clone()
                        .unwrap_or_else(|| title.clone()),
                    release_year.map(i32::from),
                    request.canonical_season,
                    request.canonical_episode,
                )
                .map_err(|_| SearchError::InvalidRequest)?,
            )
            .await
            .map_err(storage_error)?;
        episode_mappings.retain(|mapping| mapping.provider != action.provider);
        episode_mappings.push(media_contract::EpisodeCoordinateMappingDto {
            provider: action.provider.clone(),
            canonical: media_contract::EpisodeCoordinateDto {
                season: canonical.season(),
                episode: canonical.episode(),
            },
            canonical_title: canonical.media_title().to_owned(),
        });
        ambiguous_episodes.retain(|candidate| candidate.provider != action.provider);
        self.persistence
            .update_execution(job.result_ref(), execution)
            .await?;
        let job = self
            .jobs
            .retry_job_for_owner(owner, operation, job_id)
            .await
            .map_err(application_error)?;
        Ok(job_dto(&job))
    }
}

fn common_episode_season(episodes: &[media_contract::EpisodeSnapshotDto]) -> Option<u32> {
    let season = episodes.first()?.season;
    episodes
        .iter()
        .all(|episode| episode.season == season)
        .then_some(season)
}

fn mapping_action_dto(
    job_id: JobId,
    execution: &ExecutionSelectionDto,
) -> Result<EpisodeMappingActionDto, SearchError> {
    let ExecutionSelectionDto::Rezka {
        title_id,
        title,
        ambiguous_episodes,
        ..
    } = execution
    else {
        return Err(SearchError::Conflict);
    };
    let candidate = ambiguous_episodes.first().ok_or(SearchError::Conflict)?;
    Ok(EpisodeMappingActionDto {
        job_id: media_contract::PublicId::parse(&job_id.to_string())
            .map_err(|_| SearchError::Infrastructure)?,
        title: title.clone(),
        provider_media_ref: title_id.to_string(),
        provider: candidate.provider.clone(),
        label: candidate.label.clone(),
    })
}

fn application_error(error: media_core::ApplicationError) -> SearchError {
    match error {
        media_core::ApplicationError::Conflict => SearchError::Conflict,
        media_core::ApplicationError::InvalidInput(_) => SearchError::InvalidRequest,
        media_core::ApplicationError::Forbidden => SearchError::Forbidden,
        media_core::ApplicationError::NotFound => SearchError::NotFound,
        media_core::ApplicationError::Infrastructure => SearchError::Infrastructure,
    }
}

fn execution(
    result: &ProviderResult,
    request: &SelectResultRequest,
    searched_kind: Option<MediaKindDto>,
    searched_season: Option<u16>,
    series_title_hint: Option<&str>,
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
            if request.translation_id.is_some() {
                return Err(SearchError::InvalidRequest);
            }
            let (season, episode) = match searched_kind {
                Some(MediaKindDto::Movie) => {
                    if request.season.is_some() || request.episode.is_some() {
                        return Err(SearchError::InvalidRequest);
                    }
                    (None, None)
                }
                Some(MediaKindDto::Series) => {
                    if request.episode == Some(0) {
                        return Err(SearchError::InvalidRequest);
                    }
                    let searched_season = searched_season.ok_or(SearchError::Infrastructure)?;
                    let selected_season = request
                        .season
                        .map(|season| {
                            u16::try_from(season).map_err(|_| SearchError::InvalidRequest)
                        })
                        .transpose()?
                        .unwrap_or(searched_season);
                    if selected_season != searched_season {
                        return Err(SearchError::InvalidRequest);
                    }
                    (Some(selected_season), request.episode)
                }
                None => return Err(SearchError::Infrastructure),
            };
            Ok(ExecutionSelectionDto::Prowlarr {
                source_identity: source_identity.clone(),
                info_hash: info_hash.clone(),
                uri: uri.clone(),
                media_kind: searched_kind.ok_or(SearchError::Infrastructure)?,
                season,
                episode,
                title: title.clone(),
            })
        }
        (
            SearchResultDto::Rezka {
                title,
                year,
                media_kind,
                translations,
                ..
            },
            PrivateResult::Rezka {
                locator,
                title_id,
                translation_episodes,
                translation_episode_labels,
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
            let selected_labels = translation_episode_labels
                .get(&translation_id)
                .into_iter()
                .flatten()
                .filter(|candidate| {
                    episodes.iter().any(|episode| {
                        episode.season == candidate.provider.season
                            && episode.episode == candidate.provider.episode
                    })
                });
            let ambiguous_episodes = selected_labels
                .filter(|candidate| {
                    candidate.provider.season != 0
                        && (ambiguous_episode_label(&candidate.label)
                            || ambiguous_episode_label(title))
                })
                .cloned()
                .collect();
            Ok(ExecutionSelectionDto::Rezka {
                locator: locator.clone(),
                title_id: *title_id,
                media_kind: *media_kind,
                translation_id,
                translation: Some(translation.name.clone()),
                director: translation.director,
                camrip: translation.camrip,
                has_ads: translation.has_ads,
                season: request.season,
                episode: request.episode,
                episodes,
                episode_mappings: Vec::new(),
                ambiguous_episodes,
                release_year: *year,
                library_title: (*media_kind == MediaKindDto::Series)
                    .then(|| {
                        series_title_hint
                            .and_then(|hint| canonical_series_library_title(hint, title))
                    })
                    .flatten()
                    .map(str::to_owned),
                title: title.clone(),
            })
        }
        _ => Err(SearchError::Infrastructure),
    }
}

fn canonical_series_library_title<'a>(query: &'a str, provider_title: &str) -> Option<&'a str> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }

    let suffix = provider_title.strip_prefix(query)?;
    (suffix.is_empty() || suffix.starts_with(':') || suffix.starts_with(" [")).then_some(query)
}

fn ambiguous_episode_label(label: &str) -> bool {
    let normalized = label.to_lowercase();
    ["ova", "oad", "ona", "special", "спец", "экстра"]
        .iter()
        .any(|marker| normalized.contains(marker))
}

async fn apply_persisted_episode_mappings(
    identity: &dyn IdentityStore,
    execution: &mut ExecutionSelectionDto,
) -> Result<(), SearchError> {
    let ExecutionSelectionDto::Rezka {
        title_id,
        episodes,
        episode_mappings,
        ambiguous_episodes,
        ..
    } = execution
    else {
        return Ok(());
    };
    let provider_media_ref = title_id.to_string();
    for episode in episodes.iter() {
        let Some(canonical) = identity
            .find_episode_mapping(
                Provider::Rezka,
                &provider_media_ref,
                episode.season,
                episode.episode,
            )
            .await
            .map_err(storage_error)?
        else {
            continue;
        };
        episode_mappings.push(media_contract::EpisodeCoordinateMappingDto {
            provider: media_contract::EpisodeCoordinateDto {
                season: episode.season,
                episode: episode.episode,
            },
            canonical: media_contract::EpisodeCoordinateDto {
                season: canonical.season(),
                episode: canonical.episode(),
            },
            canonical_title: canonical.media_title().to_owned(),
        });
        ambiguous_episodes.retain(|candidate| {
            candidate.provider.season != episode.season
                || candidate.provider.episode != episode.episode
        });
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{
        ambiguous_episode_label, canonical_series_library_title, retryable_rezka_auth_error,
        skippable_title_error,
    };

    #[test]
    fn season_release_titles_share_an_exact_base_query_as_the_plex_title() {
        assert_eq!(
            canonical_series_library_title("Магия и мускулы", "Магия и мускулы [ТВ-1]",),
            Some("Магия и мускулы"),
        );
        assert_eq!(
            canonical_series_library_title(
                "Магия и мускулы",
                "Магия и мускулы: Экзамен на звание Вестника Бога [ТВ-2]",
            ),
            Some("Магия и мускулы"),
        );
        assert_eq!(
            canonical_series_library_title("Магия", "Магия и мускулы [ТВ-1]"),
            None,
        );
    }

    #[test]
    fn only_explicit_special_markers_trigger_manual_episode_mapping() {
        for label in ["OVA 1", "OAD", "Special episode", "Спецвыпуск", "ONA"] {
            assert!(
                ambiguous_episode_label(label),
                "marker not detected: {label}"
            );
        }
        for label in ["Episode 1", "Серия 14", "Final", "Extraordinary"] {
            assert!(!ambiguous_episode_label(label), "false positive: {label}");
        }
    }

    #[test]
    fn one_broken_title_can_be_skipped_without_hiding_authentication_failures() {
        assert!(skippable_title_error(
            rezka_client::RezkaErrorCode::Transport
        ));
        assert!(skippable_title_error(
            rezka_client::RezkaErrorCode::TitleNotFound
        ));
        assert!(!skippable_title_error(
            rezka_client::RezkaErrorCode::AuthenticationRequired
        ));
        assert!(!skippable_title_error(
            rezka_client::RezkaErrorCode::RateLimited
        ));
    }

    #[test]
    fn only_transport_failures_retry_rezka_authentication() {
        assert!(retryable_rezka_auth_error(
            rezka_client::RezkaErrorCode::Transport
        ));
        for terminal in [
            rezka_client::RezkaErrorCode::AuthenticationRequired,
            rezka_client::RezkaErrorCode::AuthenticationFailed,
            rezka_client::RezkaErrorCode::ChallengeFailed,
            rezka_client::RezkaErrorCode::RateLimited,
            rezka_client::RezkaErrorCode::ProviderResponseInvalid,
        ] {
            assert!(!retryable_rezka_auth_error(terminal));
        }
    }
}
