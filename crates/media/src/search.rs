use std::{collections::BTreeMap, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use media_api::{ChoiceSetSelection, SearchError, SearchService};
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
    PortError, Provider, ProviderAvailability, ReleaseIdentity, ReleaseMetadataPort,
    ReleaseMetadataResult, ReleasePrecision, ReleaseQuery, ReleaseSource, RunnerLifecycleState,
    RunnerLifecycleStore, ScheduledEpisode, TrackedEpisodeDownloadPort, TrackingSubscription,
    UserId,
};
use sha2::{Digest as _, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use unicode_normalization::UnicodeNormalization;

const SEARCH_TTL: time::Duration = time::Duration::hours(24);
const REZKA_CATALOG_CONTINUATION_PREFIX: &str = "catalog:";
const REZKA_SESSION_ATTEMPTS: usize = 3;
const REZKA_SESSION_RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct ProviderPage {
    pub results: Vec<ProviderResult>,
    pub provider_continuation: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VerifiedSeriesIdentity {
    pub tmdb_id: u64,
    pub canonical_title: String,
    pub legacy_path_titles: Vec<String>,
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
                        (if translation.seasons.is_empty() {
                            &availability.seasons
                        } else {
                            &translation.seasons
                        })
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
    /// All title aliases used by a tracked-episode availability probe. Plain
    /// user searches leave this empty; choice-set refreshes replay every alias
    /// instead of only the localized display title.
    pub query_aliases: Vec<String>,
}

#[async_trait::async_trait]
pub trait SearchPersistence: Send + Sync {
    async fn insert_session(&self, session: StoredSearchSession) -> Result<(), SearchError>;
    async fn session_for_owner(
        &self,
        id: &str,
        owner: UserId,
    ) -> Result<StoredSearchSession, SearchError>;
    /// Resolve a tracked-episode session for a visible user. Generic search
    /// sessions remain owner-only; storage may authorize this narrow path for
    /// family tracking subscriptions.
    async fn session_for_user(
        &self,
        id: &str,
        user: UserId,
    ) -> Result<StoredSearchSession, SearchError> {
        self.session_for_owner(id, user).await
    }
    async fn update_session(&self, session: StoredSearchSession) -> Result<(), SearchError>;
    async fn update_session_for_user(
        &self,
        session: StoredSearchSession,
        user: UserId,
    ) -> Result<(), SearchError> {
        if session.owner != user {
            return Err(SearchError::Forbidden);
        }
        self.update_session(session).await
    }
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

    async fn verify_series_identity(
        &self,
        _selected: &SearchResultDto,
        requested: Option<media_contract::SeriesGroupIdentityDto>,
    ) -> Result<Option<VerifiedSeriesIdentity>, SearchError> {
        if requested.is_some() {
            Err(SearchError::InvalidRequest)
        } else {
            Ok(None)
        }
    }
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
    async fn resolved_release_metadata(
        &self,
        tracking: &TrackingSubscription,
    ) -> Result<Option<(ReleaseIdentity, String)>, PortError> {
        if tracking.release_identity().is_none() {
            return Ok(None);
        }
        Ok(self
            .resolve_release_match(tracking, tracking.title(), None, None, true)
            .await
            .and_then(|(identity, poster_url)| poster_url.map(|poster| (identity, poster))))
    }

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
                    series_group: tracking.release_identity().map(series_group_identity),
                    preferred_qualities: Vec::new(),
                    preferred_languages: Vec::new(),
                    preferred_codecs: Vec::new(),
                    preferred_release_groups: Vec::new(),
                },
                None,
            )
            .await
            .map_err(|_| PortError::Infrastructure)?;
        let mut candidates = page
            .results
            .into_iter()
            .filter(|result| match (&result.public, &result.private) {
                (SearchResultDto::Rezka { title, .. }, PrivateResult::Rezka { title_id, .. }) => {
                    tracking.download().map_or_else(
                        || title.trim().eq_ignore_ascii_case(tracking.title().trim()),
                        |download| download.provider_media_ref() == title_id.to_string(),
                    )
                }
                _ => false,
            })
            .collect::<Vec<_>>();
        let (result, pre_resolved_match) = if tracking.download().is_some() {
            (candidates.pop(), None)
        } else {
            let mut resolved = Vec::new();
            for candidate in candidates {
                let SearchResultDto::Rezka {
                    title,
                    original_title,
                    year,
                    ..
                } = &candidate.public
                else {
                    continue;
                };
                if let Some(release_match) = self
                    .resolve_release_match(
                        tracking,
                        title,
                        original_title.as_deref(),
                        year.map(i32::from),
                        false,
                    )
                    .await
                {
                    resolved.push((candidate, release_match));
                }
            }
            if resolved.len() != 1 {
                return Err(PortError::Conflict);
            }
            let (candidate, release_match) = resolved.pop().expect("length checked");
            (Some(candidate), Some(release_match))
        };
        let Some(ProviderResult {
            public:
                SearchResultDto::Rezka {
                    title,
                    original_title,
                    year,
                    translations,
                    thumbnail_url,
                    ..
                },
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
        let release_match = match pre_resolved_match {
            Some(release_match) => Some(release_match),
            None => {
                self.resolve_release_match(
                    tracking,
                    &title,
                    original_title.as_deref(),
                    year.map(i32::from),
                    true,
                )
                .await
            }
        };
        let poster_url = release_match
            .as_ref()
            .and_then(|(_, poster_url)| poster_url.clone())
            .or(thumbnail_url);
        let mut discovery = EpisodeDiscovery::new(episodes, title, original_title)
            .map(|discovery| discovery.with_poster_url(poster_url))
            .map_err(|_| PortError::Conflict)?;
        if let Some((identity, _)) = release_match {
            discovery = discovery.with_release_identity(identity);
        }
        Ok(discovery)
    }
}

pub struct ProviderEpisodeAvailability {
    provider: Arc<dyn SearchProvider>,
    persistence: Arc<dyn SearchPersistence>,
}

impl ProviderEpisodeAvailability {
    #[must_use]
    pub fn new(provider: Arc<dyn SearchProvider>, persistence: Arc<dyn SearchPersistence>) -> Self {
        Self {
            provider,
            persistence,
        }
    }

    async fn persist_choice_session(
        &self,
        request: StartSearchRequest,
        query_aliases: Vec<String>,
        owner: UserId,
        choice_set_id: &str,
        source: ProviderDto,
        results: Vec<ProviderResult>,
    ) -> Result<usize, PortError> {
        let source_name = match source {
            ProviderDto::Rezka => "rezka",
            ProviderDto::Prowlarr => "prowlarr",
        };
        let result_count = results.len();
        let id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("choice:{choice_set_id}:{source_name}").as_bytes(),
        );
        let session = StoredSearchSession {
            id: id.to_string(),
            owner,
            request,
            expires_at: OffsetDateTime::now_utc() + time::Duration::hours(24),
            results,
            provider_continuation: None,
            query_aliases,
        };
        match self.persistence.insert_session(session.clone()).await {
            Ok(()) => {}
            Err(SearchError::Conflict) => self
                .persistence
                .update_session(session)
                .await
                .map_err(|_| PortError::Infrastructure)?,
            Err(_) => return Err(PortError::Infrastructure),
        }
        Ok(result_count)
    }

    async fn persist_empty_choice_session(
        &self,
        request: &EpisodeAvailabilityRequest<'_>,
        source: ProviderDto,
    ) -> Result<(), PortError> {
        self.persist_choice_session(
            choice_session_request(request, source)?,
            match source {
                ProviderDto::Rezka => rezka_availability_titles(request),
                ProviderDto::Prowlarr => prowlarr_availability_titles(request),
            },
            request.tracking().owner_id(),
            &Self::choice_set_id(request),
            source,
            Vec::new(),
        )
        .await
        .map(|_| ())
    }

    fn choice_set_id(request: &EpisodeAvailabilityRequest<'_>) -> String {
        media_core::episode_choice_set_id(
            request.tracking().id(),
            request.episode().season(),
            request.episode().episode(),
        )
    }

    async fn rezka_availability(
        &self,
        request: &EpisodeAvailabilityRequest<'_>,
    ) -> Result<(ProviderAvailability, usize), PortError> {
        let titles = rezka_availability_titles(request);
        let mut failed = false;
        let mut candidates = Vec::new();
        for query in &titles {
            let page = self
                .provider
                .search(
                    &StartSearchRequest {
                        scope: media_contract::SearchScopeDto {
                            platform: "system".to_owned(),
                            chat_id: format!("tracking:{}", request.tracking().id()),
                            thread_id: Some(format!(
                                "episode:{}:{}",
                                request.episode().season(),
                                request.episode().episode()
                            )),
                        },
                        source: ProviderDto::Rezka,
                        query: query.clone(),
                        media_kind: Some(MediaKindDto::Series),
                        season: None,
                        series_group: request
                            .tracking()
                            .release_identity()
                            .map(series_group_identity),
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
            for result in page.results.iter() {
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
                    continue;
                };
                let matching_title = titles.iter().any(|candidate| {
                    title.trim().eq_ignore_ascii_case(candidate.trim())
                        || original_title.as_deref().is_some_and(|original| {
                            original.trim().eq_ignore_ascii_case(candidate.trim())
                        })
                });
                if matching_title
                    && translation_episodes.values().any(|seasons| {
                        seasons.iter().any(|(season, episodes)| {
                            *season == request.episode().season()
                                && episodes.contains(&request.episode().episode())
                        })
                    })
                {
                    candidates.push(result.clone());
                }
            }
        }
        candidates.sort_by(|left, right| left.public.result_id().cmp(right.public.result_id()));
        candidates.dedup_by(|left, right| left.public.result_id() == right.public.result_id());
        if !candidates.is_empty() {
            let count = self
                .persist_choice_session(
                    StartSearchRequest {
                        scope: media_contract::SearchScopeDto {
                            platform: "system".to_owned(),
                            chat_id: format!("tracking:{}", request.tracking().id()),
                            thread_id: Some(format!(
                                "episode:{}:{}",
                                request.episode().season(),
                                request.episode().episode()
                            )),
                        },
                        source: ProviderDto::Rezka,
                        query: request.tracking().title().to_owned(),
                        media_kind: Some(MediaKindDto::Series),
                        season: None,
                        series_group: request
                            .tracking()
                            .release_identity()
                            .map(series_group_identity),
                        preferred_qualities: Vec::new(),
                        preferred_languages: Vec::new(),
                        preferred_codecs: Vec::new(),
                        preferred_release_groups: Vec::new(),
                    },
                    titles.clone(),
                    request.tracking().owner_id(),
                    &Self::choice_set_id(request),
                    ProviderDto::Rezka,
                    candidates,
                )
                .await?;
            Ok((ProviderAvailability::Available, count))
        } else if failed {
            self.persist_empty_choice_session(request, ProviderDto::Rezka)
                .await?;
            Ok((ProviderAvailability::Unknown, 0))
        } else {
            self.persist_empty_choice_session(request, ProviderDto::Rezka)
                .await?;
            Ok((ProviderAvailability::Unavailable, 0))
        }
    }

    async fn prowlarr_availability(
        &self,
        request: &EpisodeAvailabilityRequest<'_>,
    ) -> Result<(ProviderAvailability, usize), PortError> {
        let title = prowlarr_availability_title(request);
        let page = self
            .provider
            .search(
                &StartSearchRequest {
                    scope: media_contract::SearchScopeDto {
                        platform: "system".to_owned(),
                        chat_id: format!("tracking:{}", request.tracking().id()),
                        thread_id: Some(format!(
                            "episode:{}:{}",
                            request.episode().season(),
                            request.episode().episode()
                        )),
                    },
                    source: ProviderDto::Prowlarr,
                    query: title.to_owned(),
                    media_kind: Some(MediaKindDto::Series),
                    season: Some(
                        u16::try_from(request.episode().season())
                            .map_err(|_| PortError::Conflict)?,
                    ),
                    series_group: request
                        .tracking()
                        .release_identity()
                        .map(series_group_identity),
                    preferred_qualities: Vec::new(),
                    preferred_languages: Vec::new(),
                    preferred_codecs: Vec::new(),
                    preferred_release_groups: Vec::new(),
                },
                None,
            )
            .await;
        let Ok(page) = page else {
            self.persist_empty_choice_session(request, ProviderDto::Prowlarr)
                .await?;
            return Ok((ProviderAvailability::Unknown, 0));
        };
        let mut candidates = page
            .results
            .into_iter()
            .filter(|result| {
                let SearchResultDto::Prowlarr { title: release, .. } = &result.public else {
                    return false;
                };
                media_integrations::series_title_matches(release, title)
                    && media_integrations::title_contains_episode(
                        release,
                        request.episode().season(),
                        request.episode().episode(),
                    )
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.public.result_id().cmp(right.public.result_id()));
        candidates.dedup_by(|left, right| left.public.result_id() == right.public.result_id());
        if candidates.is_empty() {
            self.persist_empty_choice_session(request, ProviderDto::Prowlarr)
                .await?;
            return Ok((ProviderAvailability::Unavailable, 0));
        }
        let count = self
            .persist_choice_session(
                StartSearchRequest {
                    scope: media_contract::SearchScopeDto {
                        platform: "system".to_owned(),
                        chat_id: format!("tracking:{}", request.tracking().id()),
                        thread_id: Some(format!(
                            "episode:{}:{}",
                            request.episode().season(),
                            request.episode().episode()
                        )),
                    },
                    source: ProviderDto::Prowlarr,
                    query: request.tracking().title().to_owned(),
                    media_kind: Some(MediaKindDto::Series),
                    season: Some(
                        u16::try_from(request.episode().season())
                            .map_err(|_| PortError::Conflict)?,
                    ),
                    series_group: request
                        .tracking()
                        .release_identity()
                        .map(series_group_identity),
                    preferred_qualities: Vec::new(),
                    preferred_languages: Vec::new(),
                    preferred_codecs: Vec::new(),
                    preferred_release_groups: Vec::new(),
                },
                prowlarr_availability_titles(request),
                request.tracking().owner_id(),
                &Self::choice_set_id(request),
                ProviderDto::Prowlarr,
                candidates,
            )
            .await?;
        Ok((ProviderAvailability::Available, count))
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
        let (rezka, rezka_count) = rezka?;
        let (prowlarr, prowlarr_count) = prowlarr?;
        Ok(EpisodeAvailability::new(rezka, prowlarr)
            .with_counts(rezka_count as u32, prowlarr_count as u32))
    }
}

fn choice_session_request(
    request: &EpisodeAvailabilityRequest<'_>,
    source: ProviderDto,
) -> Result<StartSearchRequest, PortError> {
    Ok(StartSearchRequest {
        scope: media_contract::SearchScopeDto {
            platform: "system".to_owned(),
            chat_id: format!("tracking:{}", request.tracking().id()),
            thread_id: Some(format!(
                "episode:{}:{}",
                request.episode().season(),
                request.episode().episode()
            )),
        },
        source,
        query: request.tracking().title().to_owned(),
        media_kind: Some(MediaKindDto::Series),
        season: match source {
            ProviderDto::Rezka => None,
            ProviderDto::Prowlarr => {
                Some(u16::try_from(request.episode().season()).map_err(|_| PortError::Conflict)?)
            }
        },
        series_group: request
            .tracking()
            .release_identity()
            .map(series_group_identity),
        preferred_qualities: Vec::new(),
        preferred_languages: Vec::new(),
        preferred_codecs: Vec::new(),
        preferred_release_groups: Vec::new(),
    })
}

fn rezka_availability_titles(request: &EpisodeAvailabilityRequest<'_>) -> Vec<String> {
    with_terminal_series_root_aliases(distinct_titles([
        Some(request.tracking().title()),
        Some(request.discovery().release_title()),
        request.discovery().original_release_title(),
    ]))
}

fn prowlarr_availability_titles(request: &EpisodeAvailabilityRequest<'_>) -> Vec<String> {
    with_terminal_series_root_aliases(distinct_titles([
        request.discovery().original_release_title(),
        Some(request.discovery().release_title()),
        Some(request.tracking().title()),
    ]))
}

fn prowlarr_availability_title<'a>(request: &'a EpisodeAvailabilityRequest<'a>) -> &'a str {
    request
        .discovery()
        .original_release_title()
        .unwrap_or_else(|| request.discovery().release_title())
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

fn with_terminal_series_root_aliases(mut titles: Vec<String>) -> Vec<String> {
    let originals = titles.clone();
    for title in originals {
        let root = normalize_terminal_series_title(&title);
        if !root.eq_ignore_ascii_case(&title)
            && !titles
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&root))
        {
            titles.push(root);
        }
    }
    titles
}

fn normalize_terminal_series_title(value: &str) -> String {
    if rezka_series_marker(value).is_some() {
        later_season_release_title(value)
    } else {
        value.trim().to_owned()
    }
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
                    series_group: tracking.release_identity().map(series_group_identity),
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
        let verified = self
            .provider
            .verify_series_identity(
                &result.public,
                tracking.release_identity().map(series_group_identity),
            )
            .await
            .map_err(|_| PortError::Infrastructure)?;
        let series_group =
            verified
                .as_ref()
                .map(|identity| media_contract::SeriesGroupIdentityDto {
                    source: media_contract::SeriesGroupSourceDto::Tmdb,
                    source_id: identity.tmdb_id,
                });
        let mut execution = execution(
            result,
            &request,
            Some(MediaKindDto::Series),
            None,
            Some(verified.as_ref().map_or(tracking.title(), |identity| {
                identity.canonical_title.as_str()
            })),
            series_group,
            verified
                .as_ref()
                .map_or(&[][..], |identity| identity.legacy_path_titles.as_slice()),
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
    async fn resolve_release_match(
        &self,
        tracking: &TrackingSubscription,
        title: &str,
        original_title: Option<&str>,
        year: Option<i32>,
        constrain_to_persisted_identity: bool,
    ) -> Option<(ReleaseIdentity, Option<String>)> {
        let release = self.release.as_ref()?;
        let later_season = rezka_series_marker(title)
            .or_else(|| original_title.and_then(rezka_series_marker))
            .is_some_and(|season| season > 1);
        let release_title = if later_season {
            later_season_release_title(title)
        } else {
            title.to_owned()
        };
        let release_original_title = original_title.map(|value| {
            if later_season {
                later_season_release_title(value)
            } else {
                value.to_owned()
            }
        });
        let mut query = ReleaseQuery::new(
            release_title,
            release_original_title,
            if later_season { None } else { year },
        )
        .ok()?;
        if constrain_to_persisted_identity && let Some(identity) = tracking.release_identity() {
            query = query.with_source_id(identity.source_id()).ok()?;
        }
        let ReleaseMetadataResult::Matched { source, show, .. } =
            release.query(&query).await.ok()?
        else {
            return None;
        };
        if source != ReleaseSource::Tvmaze.as_str() {
            return None;
        }
        let identity = ReleaseIdentity::new(ReleaseSource::Tvmaze, show.source_id).ok()?;
        if tracking
            .release_identity()
            .is_some_and(|existing| existing != identity)
        {
            return None;
        }
        Some((identity, show.poster_url))
    }

    async fn release_episodes(
        &self,
        tracking: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        let release = self.release.as_ref().ok_or(PortError::Infrastructure)?;
        let mut query = ReleaseQuery::new(
            normalize_terminal_series_title(tracking.title()),
            None,
            None,
        )
        .map_err(|_| PortError::Conflict)?;
        if let Some(identity) = tracking.release_identity() {
            match identity.source() {
                media_core::ReleaseSource::Tvmaze => {
                    query = query
                        .with_source_id(identity.source_id())
                        .map_err(|_| PortError::Conflict)?;
                }
            }
        }
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
        let poster_url = show.poster_url.clone();
        let identity = ReleaseIdentity::new(ReleaseSource::Tvmaze, show.source_id)
            .map_err(|_| PortError::Conflict)?;
        EpisodeDiscovery::new(episodes, show.title, show.original_title)
            .map(|discovery| {
                discovery
                    .with_poster_url(poster_url)
                    .with_release_identity(identity)
            })
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
    tmdb: Option<std::sync::Arc<media_integrations::tmdb::TmdbClient>>,
    tvmaze: Option<std::sync::Arc<media_integrations::tvmaze::TvmazeClient>>,
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
            tmdb: None,
            tvmaze: None,
        }
    }

    #[must_use]
    pub fn with_tmdb(
        mut self,
        tmdb: Option<std::sync::Arc<media_integrations::tmdb::TmdbClient>>,
    ) -> Self {
        self.tmdb = tmdb;
        self
    }

    #[must_use]
    pub fn with_tvmaze(
        mut self,
        tvmaze: std::sync::Arc<media_integrations::tvmaze::TvmazeClient>,
    ) -> Self {
        self.tvmaze = Some(tvmaze);
        self
    }

    async fn search_rezka(
        &self,
        request: &StartSearchRequest,
        continuation: Option<&str>,
    ) -> Result<ProviderPage, SearchError> {
        let state = self.rezka.as_ref().ok_or(SearchError::Provider)?;
        let mut state = state.lock().await;
        let prepared = &mut *state;
        // The lock covers load, challenge solving, validation, and atomic save. It is acquired on a
        // blocking worker so inter-process contention never stalls an async executor thread.
        let _session_guard = prepared
            .acquire_session_lock()
            .await
            .map_err(|_| SearchError::RezkaDiagnostic(Self::session_store_diagnostic()))?;
        prepared
            .reload_session()
            .map_err(|_| SearchError::RezkaDiagnostic(Self::session_store_diagnostic()))?;
        for attempt in 1..=REZKA_SESSION_ATTEMPTS {
            let result = prepared.client.ensure_session(&prepared.probe).await;
            match result {
                Ok(_) => break,
                Err(error)
                    if retryable_rezka_session_error(error.code())
                        && attempt < REZKA_SESSION_ATTEMPTS =>
                {
                    tracing::warn!(
                        stage = "anubis",
                        attempt,
                        error_code = ?error.code(),
                        "temporary Rezka challenge failure; retrying with a fresh clearance"
                    );
                    tokio::time::sleep(REZKA_SESSION_RETRY_DELAY).await;
                    prepared.reload_session().map_err(|_| {
                        SearchError::RezkaDiagnostic(Self::session_store_diagnostic())
                    })?;
                }
                Err(error) => {
                    tracing::warn!(stage = "session", error_code = ?error.code(), error = %error, "Rezka search failed");
                    return Err(Self::rezka_search_error(error));
                }
            }
        }
        let snapshot = prepared
            .client
            .export_session()
            .map_err(|error| {
                tracing::warn!(stage = "session_export", error_code = ?error.code(), error = %error, "Rezka search failed");
                Self::rezka_search_error(error)
            })?;
        prepared
            .store
            .save(&snapshot)
            .map_err(|_| SearchError::RezkaDiagnostic(Self::session_store_diagnostic()))?;
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
            Self::rezka_search_error(error)
        })?;
        let entries = page.entries();
        if offset > entries.len() {
            return Err(SearchError::InvalidRequest);
        }
        let mut results = Vec::new();
        let mut cursor = offset;
        let mut title_failures = 0_usize;
        let mut usable_titles = 0_usize;
        let mut last_title_diagnostic = None;
        'entries: while cursor < entries.len() && results.len() < MAX_SEARCH_RESULTS_PER_PAGE {
            let entry = &entries[cursor];
            cursor += 1;
            let details = match prepared.client.title(entry.locator()).await {
                Ok(details) => details,
                Err(error) if skippable_title_error(error.code()) => {
                    title_failures += 1;
                    last_title_diagnostic = Some(Self::rezka_search_diagnostic(&error));
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
                    return Err(Self::rezka_search_error(error));
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
            let mut translations = details
                .translations()
                .iter()
                .filter(|translation| rezka_translation_available(translation.is_premium()))
                .map(|translation| media_contract::RezkaTranslationDto {
                    id: translation.id().get(),
                    name: translation.name().to_owned(),
                    premium: translation.is_premium(),
                    director: translation.is_director(),
                    camrip: translation.is_camrip(),
                    has_ads: translation.has_ads(),
                    seasons: Vec::new(),
                })
                .collect::<Vec<_>>();
            let mut by_translation = BTreeMap::new();
            let mut labels_by_translation = BTreeMap::new();
            if media_kind == MediaKindDto::Series {
                let selections = details
                    .translations()
                    .iter()
                    .filter(|translation| rezka_translation_available(translation.is_premium()))
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
                        Self::rezka_search_error(error)
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
                                        .collect::<Vec<_>>(),
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
                translations.retain_mut(|translation| {
                    let Some(seasons) = by_translation.get(&translation.id) else {
                        return false;
                    };
                    translation.seasons = seasons
                        .iter()
                        .map(|(season, episodes)| media_contract::SeasonAvailabilityDto {
                            season: *season,
                            episodes: episodes.clone(),
                        })
                        .collect();
                    translation
                        .seasons
                        .iter()
                        .any(|season| !season.episodes.is_empty())
                });
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
            // Do not collapse an all-invalid page into the generic provider
            // error: callers still need to distinguish a genuine parser
            // failure from a transport/provider rejection, without receiving
            // any provider body or challenge payload.
            return Err(SearchError::RezkaDiagnostic(
                last_title_diagnostic
                    .unwrap_or(media_contract::RezkaDiagnosticCategoryDto::RezkaParserInvalid),
            ));
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
        let snapshot = prepared.client.export_session().map_err(|error| {
            tracing::warn!(
                stage = "session_export",
                error_code = ?error.code(),
                error = %error,
                "Rezka search failed"
            );
            Self::rezka_search_error(error)
        })?;
        prepared
            .store
            .save(&snapshot)
            .map_err(|_| SearchError::RezkaDiagnostic(Self::session_store_diagnostic()))?;
        Ok(ProviderPage {
            results,
            provider_continuation,
        })
    }

    fn rezka_search_error(error: rezka_client::RezkaError) -> SearchError {
        SearchError::RezkaDiagnostic(Self::rezka_search_diagnostic(&error))
    }

    fn rezka_search_diagnostic(
        error: &rezka_client::RezkaError,
    ) -> media_contract::RezkaDiagnosticCategoryDto {
        match error.diagnostic_category() {
            rezka_client::RezkaDiagnosticCategory::RezkaReachable => {
                media_contract::RezkaDiagnosticCategoryDto::RezkaReachable
            }
            rezka_client::RezkaDiagnosticCategory::AnubisChallengeRequired => {
                media_contract::RezkaDiagnosticCategoryDto::AnubisChallengeRequired
            }
            rezka_client::RezkaDiagnosticCategory::AnubisChallengeFailed => {
                media_contract::RezkaDiagnosticCategoryDto::AnubisChallengeFailed
            }
            rezka_client::RezkaDiagnosticCategory::RezkaProviderRejected => {
                media_contract::RezkaDiagnosticCategoryDto::RezkaProviderRejected
            }
            rezka_client::RezkaDiagnosticCategory::RezkaParserInvalid => {
                media_contract::RezkaDiagnosticCategoryDto::RezkaParserInvalid
            }
            rezka_client::RezkaDiagnosticCategory::SessionStoreError => {
                media_contract::RezkaDiagnosticCategoryDto::SessionStoreError
            }
        }
    }

    const fn session_store_diagnostic() -> media_contract::RezkaDiagnosticCategoryDto {
        media_contract::RezkaDiagnosticCategoryDto::SessionStoreError
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
        let thumbnail_url = if let Some(tmdb) = &self.tmdb {
            let media_type = match request.media_kind {
                Some(MediaKindDto::Series) => media_contract::TrendingMediaTypeDto::Tv,
                _ => media_contract::TrendingMediaTypeDto::Movie,
            };
            tmdb.find(&request.query, media_type)
                .await
                .ok()
                .flatten()
                .and_then(|item| item.poster_url)
        } else {
            None
        };
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
                    thumbnail_url: thumbnail_url.clone(),
                    website_url: result.website_url,
                    indexer: result.indexer,
                    size_bytes: result.size_bytes,
                    seeders: result.seeders,
                    leechers: result.leechers,
                    published_at: result.published_at,
                    age_days: result.age_days,
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

fn retryable_rezka_session_error(code: rezka_client::RezkaErrorCode) -> bool {
    matches!(
        code,
        rezka_client::RezkaErrorCode::Transport
            | rezka_client::RezkaErrorCode::ChallengeFailed
            | rezka_client::RezkaErrorCode::AnubisTimeout
            | rezka_client::RezkaErrorCode::AnubisRejected
    )
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

    async fn verify_series_identity(
        &self,
        selected: &SearchResultDto,
        requested: Option<media_contract::SeriesGroupIdentityDto>,
    ) -> Result<Option<VerifiedSeriesIdentity>, SearchError> {
        let identity = selected_series_identity(selected)?;
        let Some(tmdb) = self.tmdb.as_ref() else {
            return if requested.is_some() {
                Err(SearchError::ProviderUnavailable)
            } else {
                Ok(None)
            };
        };
        let media_type = media_contract::TrendingMediaTypeDto::Tv;
        let item = match requested {
            Some(media_contract::SeriesGroupIdentityDto {
                source: media_contract::SeriesGroupSourceDto::Tmdb,
                source_id,
            }) => {
                let details = tmdb
                    .details(source_id, media_type)
                    .await
                    .map_err(|_| SearchError::ProviderUnavailable)?;
                if !selected_matches_tmdb(
                    &identity.aliases,
                    identity.year,
                    identity.later_season,
                    &details.title,
                    details.original_title.as_deref(),
                    details.year,
                ) {
                    return Err(SearchError::InvalidRequest);
                }
                if identity.later_season {
                    let mut exact_matches = Vec::new();
                    for alias in &identity.aliases {
                        exact_matches.extend(
                            tmdb.find_all(alias, media_type)
                                .await
                                .map_err(|_| SearchError::ProviderUnavailable)?,
                        );
                    }
                    exact_matches.sort_by_key(|candidate| candidate.tmdb_id);
                    exact_matches.dedup_by_key(|candidate| candidate.tmdb_id);
                    if exact_matches.len() != 1 || exact_matches[0].tmdb_id != details.tmdb_id {
                        return Err(SearchError::InvalidRequest);
                    }
                }
                return Ok(Some(VerifiedSeriesIdentity {
                    tmdb_id: details.tmdb_id,
                    canonical_title: details.title,
                    legacy_path_titles: vec![format!("rezka-series-tmdb-{}", details.tmdb_id)],
                }));
            }
            Some(media_contract::SeriesGroupIdentityDto {
                source: media_contract::SeriesGroupSourceDto::Tvmaze,
                source_id,
            }) => {
                let tvmaze = self
                    .tvmaze
                    .as_ref()
                    .ok_or(SearchError::ProviderUnavailable)?;
                let show = tvmaze
                    .show_identity(source_id)
                    .await
                    .map_err(|_| SearchError::ProviderUnavailable)?;
                let validates_candidate = |item: &media_contract::TrendingItemDto| {
                    selected_matches_tmdb(
                        &identity.aliases,
                        identity.year,
                        identity.later_season,
                        &item.title,
                        item.original_title.as_deref(),
                        item.year,
                    ) && titles_and_year_match(
                        &show.title,
                        show.year,
                        &item.title,
                        item.original_title.as_deref(),
                        item.year,
                    )
                };
                let item = if let Some(tvdb_id) = show.tvdb_id {
                    let tvdb_candidate = tmdb
                        .find_tv_by_external_id(&tvdb_id.to_string(), "tvdb_id")
                        .await
                        .map_err(|_| SearchError::ProviderUnavailable)?;
                    match tvdb_candidate.filter(|item| validates_candidate(item)) {
                        Some(item) => item,
                        None => {
                            let Some(imdb_id) = show.imdb_id.as_deref() else {
                                return Err(SearchError::InvalidRequest);
                            };
                            tmdb.find_tv_by_external_id(imdb_id, "imdb_id")
                                .await
                                .map_err(|_| SearchError::ProviderUnavailable)?
                                .filter(|item| validates_candidate(item))
                                .ok_or(SearchError::InvalidRequest)?
                        }
                    }
                } else {
                    let Some(imdb_id) = show.imdb_id.as_deref() else {
                        return Err(SearchError::InvalidRequest);
                    };
                    tmdb.find_tv_by_external_id(imdb_id, "imdb_id")
                        .await
                        .map_err(|_| SearchError::ProviderUnavailable)?
                        .filter(|item| validates_candidate(item))
                        .ok_or(SearchError::InvalidRequest)?
                };
                return Ok(Some(VerifiedSeriesIdentity {
                    tmdb_id: item.tmdb_id,
                    canonical_title: item.title,
                    legacy_path_titles: vec![
                        format!("tvmaze-{source_id}"),
                        format!("rezka-series-tvmaze-{source_id}"),
                        format!("rezka-series-tmdb-{}", item.tmdb_id),
                    ],
                }));
            }
            None => {
                let mut matches = Vec::new();
                for alias in &identity.aliases {
                    let candidates = match tmdb.find_all(alias, media_type).await {
                        Ok(candidates) => candidates,
                        Err(_) => return Ok(None),
                    };
                    matches.extend(candidates.into_iter().filter(|item| {
                        selected_matches_tmdb(
                            &identity.aliases,
                            identity.year,
                            identity.later_season,
                            &item.title,
                            item.original_title.as_deref(),
                            item.year,
                        )
                    }));
                }
                matches.sort_by_key(|item| item.tmdb_id);
                matches.dedup_by_key(|item| item.tmdb_id);
                if matches.len() != 1 {
                    return Ok(None);
                }
                matches.pop()
            }
        };
        Ok(item.map(|item| VerifiedSeriesIdentity {
            tmdb_id: item.tmdb_id,
            canonical_title: item.title,
            legacy_path_titles: vec![format!("rezka-series-tmdb-{}", item.tmdb_id)],
        }))
    }
}

struct SelectedSeriesIdentity {
    aliases: Vec<String>,
    year: Option<u16>,
    later_season: bool,
}

fn selected_series_identity(
    selected: &SearchResultDto,
) -> Result<SelectedSeriesIdentity, SearchError> {
    let SearchResultDto::Rezka {
        title,
        original_title,
        year,
        media_kind: MediaKindDto::Series,
        ..
    } = selected
    else {
        return Err(SearchError::InvalidRequest);
    };
    let marker = rezka_series_marker(title)
        .or_else(|| original_title.as_deref().and_then(rezka_series_marker));
    let later_season = marker.is_some_and(|season| season > 1);
    let mut aliases = title
        .split(" / ")
        .chain(original_title.as_deref())
        .map(strip_rezka_series_marker)
        .flat_map(|title| {
            let root = later_season
                .then(|| title.split_once(':').map(|(root, _)| root.trim()))
                .flatten();
            std::iter::once(title).chain(root)
        })
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    aliases.sort_by_key(|title| canonical_title_key(title));
    aliases.dedup_by(|left, right| canonical_title_key(left) == canonical_title_key(right));
    if aliases.is_empty() {
        return Err(SearchError::InvalidRequest);
    }
    Ok(SelectedSeriesIdentity {
        aliases,
        year: *year,
        later_season,
    })
}

fn strip_rezka_series_marker(value: &str) -> &str {
    let value = value.trim();
    rezka_series_marker(value)
        .and_then(|_| value.rsplit_once(" [").map(|(title, _)| title))
        .unwrap_or(value)
}

fn later_season_release_title(value: &str) -> String {
    strip_rezka_series_marker(value)
        .split_once(':')
        .map_or_else(|| strip_rezka_series_marker(value), |(root, _)| root.trim())
        .to_owned()
}

fn rezka_series_marker(value: &str) -> Option<u32> {
    let (_, marker) = value.trim().strip_suffix(']')?.rsplit_once(" [")?;
    let season = marker
        .strip_prefix("ТВ-")
        .or_else(|| marker.strip_prefix("TV-"))?;
    season.parse().ok().filter(|season| *season > 0)
}

fn selected_matches_tmdb(
    selected_aliases: &[String],
    selected_year: Option<u16>,
    later_season: bool,
    tmdb_title: &str,
    tmdb_original_title: Option<&str>,
    tmdb_year: Option<u16>,
) -> bool {
    (later_season || (selected_year.is_some() && selected_year == tmdb_year))
        && selected_aliases.iter().any(|selected| {
            canonical_title_key(selected) == canonical_title_key(tmdb_title)
                || tmdb_original_title.is_some_and(|original| {
                    canonical_title_key(selected) == canonical_title_key(original)
                })
        })
}

fn titles_and_year_match(
    source_title: &str,
    source_year: Option<u16>,
    tmdb_title: &str,
    tmdb_original_title: Option<&str>,
    tmdb_year: Option<u16>,
) -> bool {
    source_year.is_some()
        && source_year == tmdb_year
        && (canonical_title_key(source_title) == canonical_title_key(tmdb_title)
            || tmdb_original_title.is_some_and(|title| {
                canonical_title_key(source_title) == canonical_title_key(title)
            }))
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
        let (request, results, provider_continuation, query_aliases) =
            decode_session_payload(record.payload)?;
        Ok(StoredSearchSession {
            id: record.id.to_string(),
            owner: record.owner,
            request,
            expires_at: record.expires_at,
            results,
            provider_continuation,
            query_aliases,
        })
    }

    async fn session_for_user(
        &self,
        id: &str,
        user: UserId,
    ) -> Result<StoredSearchSession, SearchError> {
        let id = uuid::Uuid::parse_str(id).map_err(|_| SearchError::InvalidRequest)?;
        let record = self
            .repository
            .session_for_user(id, user)
            .await
            .map_err(storage_error)?
            .ok_or(SearchError::NotFound)?;
        let (request, results, provider_continuation, query_aliases) =
            decode_session_payload(record.payload)?;
        Ok(StoredSearchSession {
            id: record.id.to_string(),
            owner: record.owner,
            request,
            expires_at: record.expires_at,
            results,
            provider_continuation,
            query_aliases,
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

    async fn update_session_for_user(
        &self,
        session: StoredSearchSession,
        user: UserId,
    ) -> Result<(), SearchError> {
        let id = uuid::Uuid::parse_str(&session.id).map_err(|_| SearchError::Infrastructure)?;
        let payload = encode_session_payload(&session)?;
        self.repository
            .update_session_for_user(
                media_storage::SearchSessionRecord {
                    id,
                    owner: session.owner,
                    payload,
                    expires_at: session.expires_at,
                },
                user,
            )
            .await
            .map_err(|error| match error {
                PortError::Conflict => SearchError::Forbidden,
                PortError::Infrastructure => SearchError::Infrastructure,
            })
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
        "query_aliases": session.query_aliases,
    }))
}

type DecodedSearchSession = (
    StartSearchRequest,
    Vec<ProviderResult>,
    Option<String>,
    Vec<String>,
);

fn decode_session_payload(value: serde_json::Value) -> Result<DecodedSearchSession, SearchError> {
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
    let query_aliases = object
        .get("query_aliases")
        .cloned()
        .map(serde_json::from_value::<Vec<String>>)
        .transpose()
        .map_err(|_| SearchError::Infrastructure)?
        .unwrap_or_default();
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
    Ok((request, results, provider_continuation, query_aliases))
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
    lifecycle: Option<Arc<dyn RunnerLifecycleStore>>,
}

impl DurableSearchService {
    fn choice_selection_ref(choice_set: uuid::Uuid, source: &str) -> String {
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("selection:{choice_set}:{source}").as_bytes(),
        )
        .to_string()
    }

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
            lifecycle: None,
        }
    }

    #[must_use]
    pub fn with_identity(mut self, identity: Arc<dyn IdentityStore>) -> Self {
        self.identity = Some(identity);
        self
    }

    #[must_use]
    pub fn with_lifecycle(mut self, lifecycle: Arc<dyn RunnerLifecycleStore>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    async fn ensure_rezka_search_allowed(&self, source: ProviderDto) -> Result<(), SearchError> {
        if source != ProviderDto::Rezka {
            return Ok(());
        }
        let Some(lifecycle) = self.lifecycle.as_ref() else {
            return Ok(());
        };
        let status = lifecycle
            .get()
            .await
            .map_err(|_| SearchError::Infrastructure)?;
        if status.state == RunnerLifecycleState::Rotating {
            return Err(SearchError::VpnRotationRequired);
        }
        Ok(())
    }

    fn validate_request(request: &StartSearchRequest) -> Result<(), SearchError> {
        if request.query.trim().is_empty()
            || request.query.len() > 512
            || request.query.chars().any(char::is_control)
            || request.series_group.is_some_and(|group| {
                group.source_id == 0 || request.media_kind != Some(MediaKindDto::Series)
            })
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
        if offset == session.results.len()
            && !(offset == 0 && session.provider_continuation.is_none())
        {
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

    /// Load the two durable sessions that form a tracked-episode choice set.
    /// `session_for_owner` is deliberately used for every lookup: the stable
    /// choice-set UUID is only an opaque locator and never an authorization
    /// credential by itself.
    pub async fn choice_set(
        &self,
        owner: UserId,
        choice_set_id: &str,
    ) -> Result<serde_json::Value, SearchError> {
        let choice_set =
            uuid::Uuid::parse_str(choice_set_id).map_err(|_| SearchError::InvalidRequest)?;
        if choice_set.to_string() != choice_set_id.to_ascii_lowercase() {
            return Err(SearchError::InvalidRequest);
        }

        let mut sources = serde_json::Map::new();
        let mut expires_at = None::<OffsetDateTime>;
        let mut query = None::<String>;
        let mut season = None::<u16>;
        let mut media_kind = None::<MediaKindDto>;
        let mut expired_sources = Vec::new();
        let mut missing_sources = Vec::new();
        let mut found_session = false;
        for (source_name, source) in [
            ("rezka", ProviderDto::Rezka),
            ("prowlarr", ProviderDto::Prowlarr),
        ] {
            let session_id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("choice:{choice_set}:{source_name}").as_bytes(),
            )
            .to_string();
            let session = match self.persistence.session_for_user(&session_id, owner).await {
                Ok(session) => {
                    found_session = true;
                    session
                }
                Err(SearchError::NotFound) => {
                    missing_sources.push(source_name);
                    continue;
                }
                Err(error) => return Err(error),
            };
            expires_at = Some(expires_at.map_or(session.expires_at, |existing| {
                existing.min(session.expires_at)
            }));
            query.get_or_insert_with(|| session.request.query.clone());
            season.get_or_insert(session.request.season.unwrap_or_default());
            media_kind.get_or_insert(session.request.media_kind.unwrap_or(MediaKindDto::Series));
            if session.results.is_empty() {
                missing_sources.push(source_name);
                continue;
            }
            if session.expires_at <= OffsetDateTime::now_utc() {
                expired_sources.push(source_name);
                continue;
            }
            let session_expiry = session
                .expires_at
                .format(&Rfc3339)
                .map_err(|_| SearchError::Infrastructure)?;
            let results = session
                .results
                .iter()
                .map(|result| {
                    serde_json::to_value(&result.public).map_err(|_| SearchError::Infrastructure)
                })
                .collect::<Result<Vec<_>, _>>()?;
            sources.insert(
                source_name.to_owned(),
                serde_json::json!({
                    "selection_ref": Self::choice_selection_ref(choice_set, source_name),
                    "source": source,
                    "expires_at": session_expiry,
                    "results": results,
                }),
            );
        }
        if !found_session {
            return Err(SearchError::NotFound);
        }
        let expires_at = expires_at
            .ok_or(SearchError::NotFound)?
            .format(&Rfc3339)
            .map_err(|_| SearchError::Infrastructure)?;
        let rezka_count = sources
            .get("rezka")
            .and_then(|value| value.get("results"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        let prowlarr_count = sources
            .get("prowlarr")
            .and_then(|value| value.get("results"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        Ok(serde_json::json!({
            "choice_set_id": choice_set_id,
            "expires_at": expires_at,
            "query": query,
            "media_kind": media_kind,
            "season": season,
            "rezka_count": rezka_count,
            "prowlarr_count": prowlarr_count,
            "status": if expired_sources.is_empty() && missing_sources.is_empty() {
                "ready"
            } else {
                "refresh_required"
            },
            "expired_sources": expired_sources,
            "missing_sources": missing_sources,
            "sources": sources,
        }))
    }

    /// Refresh expired provider sessions for a tracked episode. This is the
    /// only path allowed to search providers from the Telegram choice flow;
    /// fresh sessions are always served by [`Self::choice_set`] without a
    /// provider call.
    pub async fn refresh_choice_set(
        &self,
        owner: UserId,
        choice_set_id: &str,
    ) -> Result<serde_json::Value, SearchError> {
        let choice_set =
            uuid::Uuid::parse_str(choice_set_id).map_err(|_| SearchError::InvalidRequest)?;
        if choice_set.to_string() != choice_set_id.to_ascii_lowercase() {
            return Err(SearchError::InvalidRequest);
        }
        let now = OffsetDateTime::now_utc();
        for source_name in ["rezka", "prowlarr"] {
            let session_id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("choice:{choice_set}:{source_name}").as_bytes(),
            )
            .to_string();
            let Ok(mut session) = self.persistence.session_for_user(&session_id, owner).await
            else {
                continue;
            };
            // Empty sessions are durable placeholders for a source which was
            // unavailable during the notification probe. They must be
            // refreshable even while their metadata TTL is still current.
            if session.expires_at > now && !session.results.is_empty() {
                continue;
            }
            let Some(thread_id) = session.request.scope.thread_id.as_deref() else {
                continue;
            };
            let Some((season, episode)) = thread_id
                .strip_prefix("episode:")
                .and_then(|value| value.split_once(':'))
                .and_then(|(season, episode)| {
                    Some((season.parse::<u32>().ok()?, episode.parse::<u32>().ok()?))
                })
            else {
                continue;
            };
            // Refresh is a partial-provider operation: while the dedicated
            // Rezka VPN is rotating, leave that source stale and continue
            // refreshing healthy providers such as Prowlarr.
            if source_name == "rezka"
                && self
                    .ensure_rezka_search_allowed(ProviderDto::Rezka)
                    .await
                    .is_err()
            {
                continue;
            }
            // A refresh repeats the canonical title and aliases retained in
            // the previous public results. Search every provider page and
            // apply the exact episode filter to each candidate; stale release
            // ranges such as S03E01-02 must never satisfy S03E03.
            let mut queries = session.query_aliases.clone();
            queries.push(session.request.query.clone());
            for result in &session.results {
                match &result.public {
                    SearchResultDto::Rezka {
                        title,
                        original_title,
                        ..
                    } => {
                        queries.push(title.clone());
                        if let Some(original_title) = original_title {
                            queries.push(original_title.clone());
                        }
                    }
                    SearchResultDto::Prowlarr { title, .. } => queries.push(title.clone()),
                }
            }
            let originals = queries.clone();
            queries.extend(
                originals
                    .iter()
                    .map(|query| normalize_terminal_series_title(query)),
            );
            queries.retain(|query| !query.trim().is_empty());
            queries.sort_by_key(|query| query.to_ascii_lowercase());
            queries.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

            let mut results = Vec::new();
            let mut successful_query = false;
            for query in &queries {
                let mut continuation = None;
                for _ in 0..8 {
                    let mut request = session.request.clone();
                    request.query = query.clone();
                    let page = match self
                        .provider
                        .search(&request, continuation.as_deref())
                        .await
                    {
                        Ok(page) => {
                            successful_query = true;
                            page
                        }
                        Err(_) => {
                            // One failed alias/page must not prevent the
                            // other aliases or provider from refreshing.
                            break;
                        }
                    };
                    results.extend(page.results.into_iter().filter(|result| {
                        if source_name == "rezka" {
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
                            let title_match = queries.iter().any(|candidate| {
                                title.eq_ignore_ascii_case(candidate)
                                    || original_title
                                        .as_deref()
                                        .is_some_and(|value| value.eq_ignore_ascii_case(candidate))
                            });
                            title_match
                                && translation_episodes.values().any(|seasons| {
                                    seasons.iter().any(|(candidate_season, episodes)| {
                                        *candidate_season == season && episodes.contains(&episode)
                                    })
                                })
                        } else {
                            let SearchResultDto::Prowlarr { title, .. } = &result.public else {
                                return false;
                            };
                            queries.iter().any(|candidate| {
                                media_integrations::series_title_matches(title, candidate)
                            }) && media_integrations::title_contains_episode(title, season, episode)
                        }
                    }));
                    continuation = page.provider_continuation;
                    if continuation.is_none() {
                        break;
                    }
                }
            }
            if !successful_query {
                // Keep the previous private snapshot and its expiry. The
                // caller receives refresh_required and can retry later,
                // while a healthy source still remains usable.
                continue;
            }
            results.sort_by(|left, right| left.public.result_id().cmp(right.public.result_id()));
            results.dedup_by(|left, right| left.public.result_id() == right.public.result_id());
            session.results = results;
            session.provider_continuation = None;
            session.expires_at = now + SEARCH_TTL;
            self.persistence
                .update_session_for_user(session, owner)
                .await?;
        }
        self.choice_set(owner, choice_set_id).await
    }

    async fn select_from_session(
        &self,
        owner: UserId,
        operation: OperationKey,
        request: SelectResultRequest,
        session: StoredSearchSession,
    ) -> Result<JobDto, SearchError> {
        if session.expires_at <= OffsetDateTime::now_utc() {
            return Err(SearchError::NotFound);
        }
        let result = session
            .results
            .iter()
            .find(|result| result.public.result_id() == request.result_id)
            .ok_or(SearchError::NotFound)?;
        let verified = match &result.public {
            SearchResultDto::Rezka {
                media_kind: MediaKindDto::Series,
                ..
            } => {
                self.provider
                    .verify_series_identity(&result.public, session.request.series_group)
                    .await?
            }
            _ => None,
        };
        let series_group =
            verified
                .as_ref()
                .map(|identity| media_contract::SeriesGroupIdentityDto {
                    source: media_contract::SeriesGroupSourceDto::Tmdb,
                    source_id: identity.tmdb_id,
                });
        let library_title_hint = verified
            .as_ref()
            .map_or(session.request.query.as_str(), |identity| {
                identity.canonical_title.as_str()
            });
        let mut execution = execution(
            result,
            &request,
            session.request.media_kind,
            session.request.season,
            Some(library_title_hint),
            series_group,
            verified
                .as_ref()
                .map_or(&[][..], |identity| identity.legacy_path_titles.as_slice()),
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
}

#[async_trait::async_trait]
impl SearchService for DurableSearchService {
    async fn start(
        &self,
        owner: UserId,
        request: StartSearchRequest,
    ) -> Result<SearchPageDto, SearchError> {
        Self::validate_request(&request)?;
        self.ensure_rezka_search_allowed(request.source).await?;
        let provider_page = self.provider.search(&request, None).await?;
        let session = StoredSearchSession {
            id: uuid::Uuid::new_v4().to_string(),
            owner,
            request,
            expires_at: OffsetDateTime::now_utc() + SEARCH_TTL,
            results: provider_page.results,
            provider_continuation: provider_page.provider_continuation,
            query_aliases: Vec::new(),
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
        self.ensure_rezka_search_allowed(session.request.source)
            .await?;
        self.page(session, offset).await
    }

    async fn choice_set(
        &self,
        owner: UserId,
        choice_set_id: &str,
    ) -> Result<serde_json::Value, SearchError> {
        self.choice_set(owner, choice_set_id).await
    }

    async fn refresh_choice_set(
        &self,
        owner: UserId,
        choice_set_id: &str,
    ) -> Result<serde_json::Value, SearchError> {
        self.refresh_choice_set(owner, choice_set_id).await
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
        };
        self.start(
            owner,
            StartSearchRequest {
                scope: request.scope,
                source,
                query,
                media_kind: Some(media_kind),
                season,
                series_group: None,
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
        self.select_from_session(owner, operation, request, session)
            .await
    }

    async fn select_choice_set(
        &self,
        owner: UserId,
        request: ChoiceSetSelection,
    ) -> Result<JobDto, SearchError> {
        let ChoiceSetSelection {
            operation,
            choice_set_id,
            source,
            result_id,
            translation_id,
            season,
            episode,
        } = request;
        let choice_set =
            uuid::Uuid::parse_str(&choice_set_id).map_err(|_| SearchError::InvalidRequest)?;
        if choice_set.to_string() != choice_set_id.to_ascii_lowercase() {
            return Err(SearchError::InvalidRequest);
        }
        let source_name = match source {
            ProviderDto::Rezka => "rezka",
            ProviderDto::Prowlarr => "prowlarr",
        };
        let session_id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("choice:{choice_set}:{source_name}").as_bytes(),
        )
        .to_string();
        let session = self
            .persistence
            .session_for_user(&session_id, owner)
            .await?;
        if session.request.source != source {
            return Err(SearchError::Forbidden);
        }
        let (expected_season, expected_episode) = session
            .request
            .scope
            .thread_id
            .as_deref()
            .and_then(|thread_id| thread_id.strip_prefix("episode:"))
            .and_then(|coordinates| coordinates.split_once(':'))
            .and_then(|(season, episode)| {
                Some((season.parse::<u32>().ok()?, episode.parse::<u32>().ok()?))
            })
            .filter(|(_, episode)| *episode > 0)
            .ok_or(SearchError::InvalidRequest)?;
        if season.is_some_and(|value| value != expected_season)
            || episode.is_some_and(|value| value != expected_episode)
        {
            return Err(SearchError::InvalidRequest);
        }
        let request = SelectResultRequest {
            session_id,
            result_id,
            translation_id,
            // Coordinates always come from the persisted tracked-episode
            // session. Caller omission is safe, while mismatches are rejected
            // instead of silently widening this into a season download.
            season: Some(expected_season),
            episode: Some(expected_episode),
            // This scope is private to the durable tracked-episode session;
            // the dedicated method intentionally does not accept caller scope.
            scope: session.request.scope.clone(),
        };
        self.select_from_session(owner, operation, request, session)
            .await
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
    library_title_hint: Option<&str>,
    series_group: Option<media_contract::SeriesGroupIdentityDto>,
    library_path_aliases: &[String],
) -> Result<ExecutionSelectionDto, SearchError> {
    match (&result.public, &result.private) {
        (
            SearchResultDto::Prowlarr {
                title,
                thumbnail_url,
                ..
            },
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
                library_title: library_title_hint.map(str::to_owned),
                thumbnail_url: thumbnail_url.clone(),
                title: title.clone(),
            })
        }
        (
            SearchResultDto::Rezka {
                title,
                year,
                media_kind,
                translations,
                thumbnail_url,
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
            if translation.premium {
                return Err(SearchError::InvalidRequest);
            }
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
                    (Some(season), None) => {
                        if !translation_episodes
                            .get(&translation_id)
                            .is_some_and(|seasons| {
                                seasons.iter().any(|(candidate, episodes)| {
                                    *candidate == season && !episodes.is_empty()
                                })
                            })
                        {
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
                    (Some(selected_season), None) => translation_episodes
                        .get(&translation_id)
                        .into_iter()
                        .flatten()
                        .filter(|(season, _)| *season == selected_season)
                        .flat_map(|(season, episodes)| {
                            episodes
                                .iter()
                                .map(|episode| media_contract::EpisodeSnapshotDto {
                                    season: *season,
                                    episode: *episode,
                                })
                        })
                        .collect(),
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
            let library_title = if series_group
                .is_some_and(|group| group.source == media_contract::SeriesGroupSourceDto::Tmdb)
            {
                library_title_hint
                    .map(str::trim)
                    .filter(|hint| !hint.is_empty())
                    .map(|hint| hint.nfc().collect())
            } else {
                library_title_hint
                    .and_then(|hint| canonical_rezka_library_title(hint, title, *media_kind))
            };
            let mut library_path_aliases = library_path_aliases.to_vec();
            if *media_kind == MediaKindDto::Series && series_group.is_some() {
                library_path_aliases.push(format!("rezka-{title_id}"));
                library_path_aliases.push(legacy_rezka_safe_name(title));
                library_path_aliases.sort();
                library_path_aliases.dedup();
            }
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
                library_title: library_title.clone(),
                library_path_title: Some(match media_kind {
                    MediaKindDto::Movie => format!("rezka-{title_id}"),
                    MediaKindDto::Series => canonical_rezka_library_path_title(
                        library_title.as_deref().unwrap_or(title),
                        *title_id,
                        series_group,
                    ),
                }),
                library_path_aliases,
                thumbnail_url: thumbnail_url.clone(),
                title: title.clone(),
            })
        }
        _ => Err(SearchError::Infrastructure),
    }
}

fn canonical_rezka_library_title(
    query: &str,
    provider_title: &str,
    media_kind: MediaKindDto,
) -> Option<String> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let normalized_query = canonical_title_key(query);
    let aliases = provider_title
        .split(" / ")
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .collect::<Vec<_>>();
    if let Some(alias) = aliases
        .iter()
        .find(|alias| canonical_title_key(alias) == normalized_query)
    {
        return Some(alias.nfc().collect());
    }
    if media_kind != MediaKindDto::Series {
        return None;
    }
    aliases.iter().find_map(|alias| {
        let without_marker = alias
            .rsplit_once(" [")
            .map_or(*alias, |(title, _)| title)
            .trim();
        [
            without_marker,
            without_marker
                .split_once(':')
                .map_or(without_marker, |(root, _)| root),
        ]
        .into_iter()
        .find(|candidate| canonical_title_key(candidate) == normalized_query)
        .map(|candidate| candidate.nfc().collect())
    })
}

fn series_group_identity(
    identity: media_core::ReleaseIdentity,
) -> media_contract::SeriesGroupIdentityDto {
    let source = match identity.source() {
        media_core::ReleaseSource::Tvmaze => media_contract::SeriesGroupSourceDto::Tvmaze,
    };
    media_contract::SeriesGroupIdentityDto {
        source,
        source_id: identity.source_id(),
    }
}

fn canonical_rezka_library_path_title(
    library_title: &str,
    title_id: u64,
    series_group: Option<media_contract::SeriesGroupIdentityDto>,
) -> String {
    let title: String = library_title.trim().nfc().collect();
    match series_group {
        Some(media_contract::SeriesGroupIdentityDto {
            source: media_contract::SeriesGroupSourceDto::Tmdb,
            source_id,
        }) => format!("{title} {{tmdb-{source_id}}}"),
        Some(media_contract::SeriesGroupIdentityDto {
            source: media_contract::SeriesGroupSourceDto::Tvmaze,
            source_id,
        }) => format!("tvmaze-{source_id}"),
        None => format!("rezka-{title_id}"),
    }
}

fn canonical_title_key(value: &str) -> String {
    value
        .trim()
        .nfc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfc()
        .collect()
}

fn legacy_rezka_safe_name(value: &str) -> String {
    let value = value
        .nfc()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, ' ' | '-' | '_') {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        "media".to_owned()
    } else {
        value
    }
}

fn ambiguous_episode_label(label: &str) -> bool {
    let normalized = label.to_lowercase();
    ["ova", "oad", "ona", "special", "спец", "экстра"]
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn rezka_translation_available(translation_is_premium: bool) -> bool {
    !translation_is_premium
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
        lifecycle_cycle: job.lifecycle_cycle(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConcreteSearchProvider, SearchProvider, ambiguous_episode_label,
        canonical_rezka_library_path_title, canonical_rezka_library_title,
        retryable_rezka_session_error, rezka_translation_available, selected_matches_tmdb,
        selected_series_identity, skippable_title_error,
    };
    use media_contract::{MediaKindDto, SearchResultDto};

    #[test]
    fn selected_rezka_title_marker_and_year_bind_verified_tmdb_identity() {
        let selected = SearchResultDto::Rezka {
            result_id: "sugar-2024".to_owned(),
            title: "Шугар [ТВ-1]".to_owned(),
            original_title: Some("Sugar".to_owned()),
            year: Some(2024),
            media_kind: MediaKindDto::Series,
            thumbnail_url: None,
            translations: Vec::new(),
            availability: None,
        };
        let identity = selected_series_identity(&selected).unwrap();

        assert_eq!(identity.aliases, vec!["Sugar", "Шугар"]);
        assert!(selected_matches_tmdb(
            &identity.aliases,
            identity.year,
            false,
            "Шугар",
            Some("Sugar"),
            Some(2024),
        ));
        assert!(!selected_matches_tmdb(
            &identity.aliases,
            identity.year,
            false,
            "Шугар",
            Some("Sugar"),
            Some(2016),
        ));
        assert!(selected_matches_tmdb(
            &["Cafe\u{301}".to_owned()],
            Some(2026),
            false,
            "Café",
            None,
            Some(2026),
        ));
    }

    #[test]
    fn later_season_marker_allows_only_the_exact_root_without_remake_year_binding() {
        for (title, expected_root) in [
            (
                "Магия и мускулы: Экзамен на звание Вестника Бога [ТВ-2]",
                "Магия и мускулы",
            ),
            ("Slow Horses: Season Four [TV-4]", "Slow Horses"),
        ] {
            let selected = SearchResultDto::Rezka {
                result_id: "later-season".to_owned(),
                title: title.to_owned(),
                original_title: None,
                year: Some(2026),
                media_kind: MediaKindDto::Series,
                thumbnail_url: None,
                translations: Vec::new(),
                availability: None,
            };
            let identity = selected_series_identity(&selected).unwrap();
            assert!(identity.later_season);
            assert!(identity.aliases.iter().any(|alias| alias == expected_root));
            assert!(selected_matches_tmdb(
                &identity.aliases,
                identity.year,
                identity.later_season,
                expected_root,
                None,
                Some(2023),
            ));
            assert!(!selected_matches_tmdb(
                &identity.aliases,
                identity.year,
                identity.later_season,
                "Unrelated Show",
                None,
                Some(2023),
            ));
        }

        let plain = SearchResultDto::Rezka {
            result_id: "plain-subtitle".to_owned(),
            title: "Sugar: Dark Season".to_owned(),
            original_title: None,
            year: Some(2024),
            media_kind: MediaKindDto::Series,
            thumbnail_url: None,
            translations: Vec::new(),
            availability: None,
        };
        let identity = selected_series_identity(&plain).unwrap();
        assert!(!identity.later_season);
        assert_eq!(identity.aliases, ["Sugar: Dark Season"]);
    }

    #[tokio::test]
    async fn implicit_identity_outage_falls_back_but_explicit_claim_fails_closed() {
        let selected = SearchResultDto::Rezka {
            result_id: "stable-rezka-42".to_owned(),
            title: "Sugar".to_owned(),
            original_title: None,
            year: Some(2024),
            media_kind: MediaKindDto::Series,
            thumbnail_url: None,
            translations: Vec::new(),
            availability: None,
        };
        let provider = ConcreteSearchProvider::new(None, None);

        assert_eq!(
            provider.verify_series_identity(&selected, None).await,
            Ok(None),
        );
        assert_eq!(
            provider
                .verify_series_identity(
                    &selected,
                    Some(media_contract::SeriesGroupIdentityDto {
                        source: media_contract::SeriesGroupSourceDto::Tmdb,
                        source_id: 123,
                    }),
                )
                .await,
            Err(media_api::SearchError::ProviderUnavailable),
        );
    }

    #[test]
    fn season_release_titles_share_an_exact_base_query_as_the_plex_title() {
        assert_eq!(
            canonical_rezka_library_title(
                "мАгИя И мУсКуЛы",
                "Магия и мускулы [ТВ-1]",
                MediaKindDto::Series,
            ),
            Some("Магия и мускулы".to_owned()),
        );
        assert_eq!(
            canonical_rezka_library_title(
                "Магия и мускулы",
                "Магия и мускулы [ТВ-1]",
                MediaKindDto::Series,
            ),
            Some("Магия и мускулы".to_owned()),
        );
        assert_eq!(
            canonical_rezka_library_title(
                "Магия и мускулы",
                "Магия и мускулы: Экзамен на звание Вестника Бога [ТВ-2]",
                MediaKindDto::Series,
            ),
            Some("Магия и мускулы".to_owned()),
        );
        assert_eq!(
            canonical_rezka_library_title("Магия", "Магия и мускулы [ТВ-1]", MediaKindDto::Series,),
            None,
        );
    }

    #[test]
    fn exact_rezka_movie_alias_is_a_safe_library_title() {
        let provider =
            "Аватар Аанг: Последний маг воздуха / Легенда об Аанге: Последний маг воздуха";
        assert_eq!(
            canonical_rezka_library_title(
                "  легенда ОБ аанге: последний маг воздуха ",
                provider,
                MediaKindDto::Movie,
            ),
            Some("Легенда об Аанге: Последний маг воздуха".to_owned()),
        );
        assert_eq!(
            canonical_rezka_library_title("Легенда об Аанге", provider, MediaKindDto::Movie,),
            None,
        );
    }

    #[test]
    fn exact_rezka_alias_match_uses_unicode_canonical_normalization() {
        let decomposed_cafe = "Cafe\u{301}";
        assert_eq!(
            canonical_rezka_library_title(
                decomposed_cafe,
                "Café / Другой фильм",
                MediaKindDto::Movie,
            ),
            Some("Café".to_owned()),
        );
        let decomposed_cyrillic = "и\u{306}ога";
        assert_eq!(
            canonical_rezka_library_title(decomposed_cyrillic, "Фильм / Йога", MediaKindDto::Movie,),
            Some("Йога".to_owned()),
        );
    }

    #[test]
    fn unverified_rezka_path_uses_only_the_stable_source_identity() {
        assert_eq!(
            canonical_rezka_library_path_title("Cafe\u{301}", 90825, None),
            "rezka-90825"
        );
    }

    #[test]
    fn rezka_series_path_title_groups_only_by_explicit_stable_identity() {
        use media_contract::{SeriesGroupIdentityDto, SeriesGroupSourceDto};

        assert_eq!(
            canonical_rezka_library_path_title("Магия и мускулы", 101, None),
            "rezka-101"
        );
        let group = Some(SeriesGroupIdentityDto {
            source: SeriesGroupSourceDto::Tmdb,
            source_id: 94997,
        });
        assert_eq!(
            canonical_rezka_library_path_title("Магия и мускулы", 101, group),
            "Магия и мускулы {tmdb-94997}",
        );
        assert_eq!(
            canonical_rezka_library_path_title("Магия и мускулы", 202, group),
            "Магия и мускулы {tmdb-94997}",
        );
        let group = Some(SeriesGroupIdentityDto {
            source: SeriesGroupSourceDto::Tvmaze,
            source_id: 88,
        });
        assert_eq!(
            canonical_rezka_library_path_title("Магия и мускулы", 101, group),
            "tvmaze-88",
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
    fn transient_transport_and_anubis_failures_retry_rezka_session() {
        for retryable in [
            rezka_client::RezkaErrorCode::Transport,
            rezka_client::RezkaErrorCode::ChallengeFailed,
            rezka_client::RezkaErrorCode::AnubisTimeout,
            rezka_client::RezkaErrorCode::AnubisRejected,
        ] {
            assert!(retryable_rezka_session_error(retryable));
        }
        for terminal in [
            rezka_client::RezkaErrorCode::AuthenticationRequired,
            rezka_client::RezkaErrorCode::AuthenticationFailed,
            rezka_client::RezkaErrorCode::AnubisUnsupportedAlgorithm,
            rezka_client::RezkaErrorCode::AnubisExcessiveDifficulty,
            rezka_client::RezkaErrorCode::RateLimited,
            rezka_client::RezkaErrorCode::ProviderResponseInvalid,
        ] {
            assert!(!retryable_rezka_session_error(terminal));
        }
    }

    #[test]
    fn premium_translations_are_hidden_for_non_premium_accounts() {
        assert!(rezka_translation_available(false));
        assert!(!rezka_translation_available(true));
    }
}
