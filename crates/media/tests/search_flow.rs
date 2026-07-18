use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use media::search::{
    DurableSearchService, ProviderPage, ProviderResult, SearchPersistence, SearchProvider,
    StoredSearchSession, TrackedEpisodeDownloader,
};
use media_api::{SearchError, SearchService};
use media_contract::{
    ContinueSearchRequest, MediaKindDto, ProviderDto, ProwlarrRankingDto, RezkaTranslationDto,
    SearchResultDto, SearchScopeDto, SeasonAvailabilityDto, SelectResultRequest,
    SeriesAvailabilityDto, StartSearchRequest, TrackingPromptDto,
};
use media_core::{
    PRIMARY_USER_ID, CanonicalEpisode, CanonicalEpisodeCoordinates, CanonicalMedia, CanonicalSeason,
    EpisodeDiscoveryPort, EpisodeId, EpisodeMappingConfirmation, EpisodeProviderMapping,
    ExternalNamespace, IdentityStore, Job, JobApplication, JobId, JobStore, MediaExternalReference,
    NewJob, NotifyScope, OperationKey, PortError, Provider, QueueStatus, ReleaseCandidate,
    ReleaseLifecycle, ReleaseMetadataPort, ReleaseMetadataResult, ReleasePrecision, ReleaseQuery,
    ReleaseQueryError, ScheduledEpisode, TrackedEpisodeDownloadPort, TrackingDownload, TrackingId,
    TrackingScope, TrackingSubscription, UserId, SECONDARY_USER_ID,
};

#[derive(Default)]
struct MemorySearchPersistence {
    sessions: Mutex<HashMap<String, StoredSearchSession>>,
    executions: Mutex<HashMap<String, media_contract::ExecutionSelectionDto>>,
}

#[tokio::test]
async fn tracking_discovery_uses_the_selected_rezka_translation_snapshot() {
    let public = SearchResultDto::Rezka {
        result_id: "rezka-show".to_owned(),
        title: "Show".to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Series,
        thumbnail_url: None,
        translations: vec![RezkaTranslationDto {
            id: 37,
            name: "Original".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
        }],
        availability: Some(SeriesAvailabilityDto {
            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
            incomplete: true,
            seasons: vec![SeasonAvailabilityDto {
                season: 1,
                episodes: vec![1, 2],
            }],
            tracking_prompt: None,
        }),
    };
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::from([(
            ProviderDto::Rezka,
            vec![ProviderPage {
                results: vec![ProviderResult::rezka(public, "/show.html".to_owned(), 42)],
                provider_continuation: None,
            }],
        )])),
    });
    let discovery = media::search::ProviderEpisodeDiscovery::new(provider);
    let tracking = TrackingSubscription::rehydrate(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Show".to_owned(),
        "Original".to_owned(),
        vec![media_core::EpisodeSnapshot::new(1, 1).unwrap()],
        TrackingScope::Personal,
        None,
    )
    .unwrap();

    assert_eq!(
        discovery.available_episodes(&tracking).await.unwrap(),
        vec![
            media_core::EpisodeSnapshot::new(1, 1).unwrap(),
            media_core::EpisodeSnapshot::new(1, 2).unwrap(),
        ]
    );
}

struct FakeReleaseProvider;

#[async_trait::async_trait]
impl ReleaseMetadataPort for FakeReleaseProvider {
    async fn query(
        &self,
        query: &ReleaseQuery,
    ) -> Result<ReleaseMetadataResult, ReleaseQueryError> {
        assert_eq!(query.title, "Sugar");
        Ok(ReleaseMetadataResult::Matched {
            source: "tvmaze".to_owned(),
            fetched_at: "2026-07-13T14:00:00Z".to_owned(),
            show: ReleaseCandidate {
                source_id: 7,
                title: "Sugar".to_owned(),
                original_title: None,
                year: Some(2024),
                lifecycle: ReleaseLifecycle::Ongoing,
            },
            precision: ReleasePrecision::DateTime,
            lifecycle: ReleaseLifecycle::Ongoing,
            released_episodes: 1,
            expected_episodes: Some(2),
            next_episode: None,
            schedule: vec![
                ScheduledEpisode {
                    source_id: 71,
                    season: 1,
                    episode: 1,
                    title: "Past".to_owned(),
                    air_at: Some("2020-01-01T00:00:00Z".to_owned()),
                    precision: ReleasePrecision::DateTime,
                },
                ScheduledEpisode {
                    source_id: 72,
                    season: 1,
                    episode: 2,
                    title: "Future".to_owned(),
                    air_at: Some("2099-01-01T00:00:00Z".to_owned()),
                    precision: ReleasePrecision::DateTime,
                },
            ],
        })
    }
}

#[tokio::test]
async fn calendar_tracking_is_independent_of_download_providers() {
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::new()),
    });
    let discovery = media::search::ProviderEpisodeDiscovery::with_release(
        provider,
        Arc::new(FakeReleaseProvider),
    );
    let tracking = TrackingSubscription::rehydrate(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Sugar".to_owned(),
        "release-calendar".to_owned(),
        vec![media_core::EpisodeSnapshot::new(1, 1).unwrap()],
        TrackingScope::Personal,
        None,
    )
    .unwrap();

    assert_eq!(
        discovery.available_episodes(&tracking).await.unwrap(),
        vec![media_core::EpisodeSnapshot::new(1, 1).unwrap()]
    );
}

#[tokio::test]
async fn tracked_episode_download_creates_one_exact_rezka_episode_execution_for_the_owner() {
    let public = SearchResultDto::Rezka {
        result_id: "rezka:42".to_owned(),
        title: "Blades of the Guardians S2".to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Series,
        thumbnail_url: None,
        translations: vec![RezkaTranslationDto {
            id: 19,
            name: "Studio Dub".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
        }],
        availability: Some(SeriesAvailabilityDto {
            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
            incomplete: true,
            seasons: vec![SeasonAvailabilityDto {
                season: 2,
                episodes: vec![7, 8],
            }],
            tracking_prompt: None,
        }),
    };
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::from([(
            ProviderDto::Rezka,
            vec![ProviderPage {
                results: vec![ProviderResult::rezka(public, "/show.html".to_owned(), 42)],
                provider_continuation: None,
            }],
        )])),
    });
    let persistence = Arc::new(MemorySearchPersistence::default());
    let jobs = Arc::new(MemoryJobStore::default());
    let downloader = TrackedEpisodeDownloader::new(
        provider,
        persistence.clone(),
        Arc::new(JobApplication::new(jobs.clone())),
    );
    let tracking = TrackingSubscription::rehydrate(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Blades of the Guardians S2".to_owned(),
        "Studio Dub".to_owned(),
        vec![media_core::EpisodeSnapshot::new(2, 7).unwrap()],
        TrackingScope::Personal,
        Some(TrackingDownload::new("42".to_owned(), 19, 2).unwrap()),
    )
    .unwrap();

    downloader
        .enqueue_episode(&tracking, media_core::EpisodeSnapshot::new(2, 8).unwrap())
        .await
        .unwrap();

    let jobs = jobs.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].owner_id(), PRIMARY_USER_ID);
    assert_eq!(jobs[0].provider(), Provider::Rezka);
    assert_eq!(jobs[0].notify_scope(), NotifyScope::Initiator);
    let execution = persistence
        .executions
        .lock()
        .unwrap()
        .get(jobs[0].result_ref())
        .cloned()
        .unwrap();
    assert!(matches!(
        execution,
        media_contract::ExecutionSelectionDto::Rezka {
            title_id: 42,
            translation_id: 19,
            season: Some(2),
            episode: Some(8),
            ..
        }
    ));
}

#[async_trait::async_trait]
impl SearchPersistence for MemorySearchPersistence {
    async fn insert_session(&self, session: StoredSearchSession) -> Result<(), SearchError> {
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), session);
        Ok(())
    }

    async fn session_for_owner(
        &self,
        id: &str,
        owner: UserId,
    ) -> Result<StoredSearchSession, SearchError> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .filter(|session| session.owner == owner)
            .cloned()
            .ok_or(SearchError::NotFound)
    }

    async fn update_session(&self, session: StoredSearchSession) -> Result<(), SearchError> {
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), session);
        Ok(())
    }

    async fn insert_execution(
        &self,
        result_ref: String,
        execution: media_contract::ExecutionSelectionDto,
    ) -> Result<(), SearchError> {
        self.executions
            .lock()
            .unwrap()
            .insert(result_ref, execution);
        Ok(())
    }

    async fn execution_for(
        &self,
        result_ref: &str,
    ) -> Result<media_contract::ExecutionSelectionDto, SearchError> {
        self.executions
            .lock()
            .unwrap()
            .get(result_ref)
            .cloned()
            .ok_or(SearchError::NotFound)
    }

    async fn update_execution(
        &self,
        result_ref: &str,
        execution: media_contract::ExecutionSelectionDto,
    ) -> Result<(), SearchError> {
        let mut executions = self.executions.lock().unwrap();
        let stored = executions
            .get_mut(result_ref)
            .ok_or(SearchError::NotFound)?;
        *stored = execution;
        Ok(())
    }
}

struct FakeProvider {
    pages: Mutex<HashMap<ProviderDto, Vec<ProviderPage>>>,
}

#[async_trait::async_trait]
impl SearchProvider for FakeProvider {
    async fn search(
        &self,
        request: &StartSearchRequest,
        _: Option<&str>,
    ) -> Result<ProviderPage, SearchError> {
        let mut pages = self.pages.lock().unwrap();
        let pages = pages
            .get_mut(&request.source)
            .ok_or(SearchError::Provider)?;
        if pages.is_empty() {
            return Err(SearchError::Provider);
        }
        Ok(pages.remove(0))
    }
}

#[derive(Default)]
struct MemoryJobStore {
    jobs: Mutex<Vec<Job>>,
}

#[async_trait::async_trait]
impl JobStore for MemoryJobStore {
    async fn create(&self, _: OperationKey, job: NewJob) -> Result<Job, PortError> {
        let job = Job::rehydrate(
            job.id(),
            job.owner_id(),
            job.provider(),
            job.result_ref().to_owned(),
            media_core::JobState::Queued,
            None,
            job.notify_scope(),
        )
        .map_err(|_| PortError::Infrastructure)?;
        self.jobs.lock().unwrap().push(job.clone());
        Ok(job)
    }
    async fn find_for_owner(&self, id: JobId, owner: UserId) -> Result<Option<Job>, PortError> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .find(|job| job.id() == id && job.owner_id() == owner)
            .cloned())
    }
    async fn list_for_owner(&self, owner: UserId) -> Result<Vec<Job>, PortError> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|job| job.owner_id() == owner)
            .cloned()
            .collect())
    }
    async fn cancel(&self, _: OperationKey, _: JobId, _: UserId) -> Result<Option<Job>, PortError> {
        Ok(None)
    }
    async fn retry(
        &self,
        _: OperationKey,
        id: JobId,
        owner: UserId,
    ) -> Result<Option<Job>, PortError> {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(position) = jobs
            .iter()
            .position(|job| job.id() == id && job.owner_id() == owner)
        else {
            return Ok(None);
        };
        let current = &jobs[position];
        if current.state() != media_core::JobState::NeedsAction {
            return Err(PortError::Conflict);
        }
        let queued = Job::rehydrate(
            current.id(),
            current.owner_id(),
            current.provider(),
            current.result_ref().to_owned(),
            media_core::JobState::Queued,
            None,
            current.notify_scope(),
        )
        .map_err(|_| PortError::Infrastructure)?;
        jobs[position] = queued.clone();
        Ok(Some(queued))
    }
    async fn queue_status(&self) -> Result<QueueStatus, PortError> {
        Ok(QueueStatus {
            queued: 0,
            active: false,
            runner_state: media_core::RunnerLifecycleState::Ready,
            blocked_reason: None,
        })
    }
}

#[derive(Default)]
struct MemoryIdentityStore {
    confirmations: Mutex<Vec<EpisodeMappingConfirmation>>,
}

#[async_trait::async_trait]
impl IdentityStore for MemoryIdentityStore {
    async fn create_media(&self, _: CanonicalMedia) -> Result<CanonicalMedia, PortError> {
        Err(PortError::Infrastructure)
    }
    async fn add_external_reference(
        &self,
        _: MediaExternalReference,
    ) -> Result<MediaExternalReference, PortError> {
        Err(PortError::Infrastructure)
    }
    async fn find_media_by_external_reference(
        &self,
        _: ExternalNamespace,
        _: &str,
    ) -> Result<Option<CanonicalMedia>, PortError> {
        Ok(None)
    }
    async fn create_season(&self, _: CanonicalSeason) -> Result<CanonicalSeason, PortError> {
        Err(PortError::Infrastructure)
    }
    async fn create_episode(&self, _: CanonicalEpisode) -> Result<CanonicalEpisode, PortError> {
        Err(PortError::Infrastructure)
    }
    async fn save_episode_mapping(
        &self,
        _: EpisodeProviderMapping,
    ) -> Result<EpisodeProviderMapping, PortError> {
        Err(PortError::Infrastructure)
    }
    async fn find_episode_mapping(
        &self,
        _: Provider,
        _: &str,
        _: u32,
        _: u32,
    ) -> Result<Option<CanonicalEpisodeCoordinates>, PortError> {
        Ok(None)
    }
    async fn confirm_episode_mapping(
        &self,
        confirmation: EpisodeMappingConfirmation,
    ) -> Result<CanonicalEpisodeCoordinates, PortError> {
        let coordinates = CanonicalEpisodeCoordinates::new(
            EpisodeId::new(),
            confirmation.canonical_season(),
            confirmation.canonical_episode(),
            confirmation.title().to_owned(),
        );
        self.confirmations.lock().unwrap().push(confirmation);
        Ok(coordinates)
    }
}

#[tokio::test]
async fn ambiguous_episode_is_resolved_persisted_in_execution_and_requeued() {
    let persistence = Arc::new(MemorySearchPersistence::default());
    let jobs = Arc::new(MemoryJobStore::default());
    let identity = Arc::new(MemoryIdentityStore::default());
    let job_id = JobId::new();
    let result_ref = "selection:ambiguous-ova";
    jobs.jobs.lock().unwrap().push(
        Job::rehydrate(
            job_id,
            PRIMARY_USER_ID,
            Provider::Rezka,
            result_ref.to_owned(),
            media_core::JobState::NeedsAction,
            Some(media_core::NeedsActionReason::IdentityAmbiguous),
            NotifyScope::Initiator,
        )
        .unwrap(),
    );
    persistence
        .insert_execution(
            result_ref.to_owned(),
            media_contract::ExecutionSelectionDto::Rezka {
                locator: "/ova.html".to_owned(),
                title_id: 42,
                media_kind: MediaKindDto::Series,
                translation_id: 19,
                translation: Some("AniLibria".to_owned()),
                director: false,
                camrip: false,
                has_ads: false,
                season: Some(1),
                episode: Some(14),
                episodes: vec![media_contract::EpisodeSnapshotDto {
                    season: 1,
                    episode: 14,
                }],
                episode_mappings: Vec::new(),
                ambiguous_episodes: vec![media_contract::AmbiguousEpisodeDto {
                    provider: media_contract::EpisodeCoordinateDto {
                        season: 1,
                        episode: 14,
                    },
                    label: "OVA".to_owned(),
                }],
                release_year: Some(2016),
                title: "Separate OVA title".to_owned(),
            },
        )
        .await
        .unwrap();
    let service = DurableSearchService::new(
        persistence.clone(),
        Arc::new(FakeProvider {
            pages: Mutex::new(HashMap::new()),
        }),
        Arc::new(JobApplication::new(jobs)),
    )
    .with_identity(identity.clone());

    let action = service
        .episode_mapping_action(PRIMARY_USER_ID, job_id)
        .await
        .unwrap();
    assert_eq!((action.provider.season, action.provider.episode), (1, 14));
    let queued = service
        .resolve_episode_mapping(
            PRIMARY_USER_ID,
            OperationKey::from_bytes([91; 32]),
            job_id,
            media_contract::ResolveEpisodeMappingRequest {
                canonical_season: 0,
                canonical_episode: 1,
                canonical_title: Some("My Hero Academia".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(queued.state, media_contract::JobStateDto::Queued);
    let execution = persistence.execution_for(result_ref).await.unwrap();
    let media_contract::ExecutionSelectionDto::Rezka {
        episode_mappings,
        ambiguous_episodes,
        ..
    } = execution
    else {
        panic!("expected Rezka execution");
    };
    assert!(ambiguous_episodes.is_empty());
    assert_eq!(episode_mappings[0].canonical.season, 0);
    assert_eq!(episode_mappings[0].canonical.episode, 1);
    assert_eq!(episode_mappings[0].canonical_title, "My Hero Academia");
    assert_eq!(identity.confirmations.lock().unwrap().len(), 1);
}

fn request(source: ProviderDto) -> StartSearchRequest {
    StartSearchRequest {
        scope: telegram_scope("default", None),
        source,
        query: "Example".to_owned(),
        media_kind: (source == ProviderDto::Prowlarr).then_some(MediaKindDto::Movie),
        season: None,
        preferred_qualities: vec![],
        preferred_languages: vec![],
        preferred_codecs: vec![],
        preferred_release_groups: vec![],
    }
}

fn telegram_scope(chat_id: &str, thread_id: Option<&str>) -> SearchScopeDto {
    SearchScopeDto {
        platform: "telegram".to_owned(),
        chat_id: chat_id.to_owned(),
        thread_id: thread_id.map(str::to_owned),
    }
}

fn prowlarr_result(index: usize) -> ProviderResult {
    ProviderResult::prowlarr(
        SearchResultDto::Prowlarr {
            result_id: format!("torrent-{index}"),
            title: format!("Release {index}"),
            indexer: Some("Mock".to_owned()),
            size_bytes: 1000 + index as u64,
            seeders: 10,
            release_group: None,
            ranking: ProwlarrRankingDto {
                exact_title: true,
                exact_season: true,
                quality_preference: 0,
                language_preference: 0,
                seeders: 10,
                size_bytes: 1000 + index as u64,
                codec_preference: 0,
                release_group_preference: 0,
            },
        },
        format!("mock:guid:{index}"),
        "0123456789abcdef0123456789abcdef01234567".to_owned(),
        format!("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn={index}"),
    )
}

fn service(pages: HashMap<ProviderDto, Vec<ProviderPage>>) -> DurableSearchService {
    DurableSearchService::new(
        Arc::new(MemorySearchPersistence::default()),
        Arc::new(FakeProvider {
            pages: Mutex::new(pages),
        }),
        Arc::new(JobApplication::new(Arc::new(MemoryJobStore::default()))),
    )
}

#[tokio::test]
async fn prowlarr_series_search_requires_an_explicit_season() {
    let service = service(HashMap::new());
    let mut request = request(ProviderDto::Prowlarr);
    request.media_kind = Some(MediaKindDto::Series);

    assert_eq!(
        service.start(PRIMARY_USER_ID, request).await.unwrap_err(),
        SearchError::InvalidRequest
    );
}

#[tokio::test]
async fn rezka_accepts_media_kind_as_a_search_filter() {
    let public = SearchResultDto::Rezka {
        result_id: "rezka-movie".to_owned(),
        title: "Movie".to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Movie,
        thumbnail_url: None,
        translations: vec![],
        availability: None,
    };
    let mut pages = HashMap::new();
    pages.insert(
        ProviderDto::Rezka,
        vec![ProviderPage {
            results: vec![ProviderResult::rezka(public, "/movie.html".to_owned(), 42)],
            provider_continuation: None,
        }],
    );
    let service = service(pages);
    let mut request = request(ProviderDto::Rezka);
    request.media_kind = Some(MediaKindDto::Movie);

    let page = service.start(PRIMARY_USER_ID, request).await.unwrap();

    assert_eq!(page.results.len(), 1);
    assert!(matches!(
        page.results.first(),
        Some(SearchResultDto::Rezka {
            media_kind: MediaKindDto::Movie,
            ..
        })
    ));
}

#[tokio::test]
async fn prowlarr_paginates_five_and_runner_gets_only_the_exact_selected_result() {
    let mut pages = HashMap::new();
    pages.insert(
        ProviderDto::Prowlarr,
        vec![ProviderPage {
            results: (0..6).map(prowlarr_result).collect(),
            provider_continuation: None,
        }],
    );
    let service = service(pages);

    let first = service
        .start(PRIMARY_USER_ID, request(ProviderDto::Prowlarr))
        .await
        .unwrap();
    assert_eq!(first.results.len(), 5);
    let continuation = first.continuation.clone().unwrap();
    assert!(!serde_json::to_string(&first).unwrap().contains("magnet:"));
    let second = service
        .continue_search(
            PRIMARY_USER_ID,
            ContinueSearchRequest {
                continuation,
                scope: telegram_scope("default", None),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.results.len(), 1);

    let selected = service
        .select(
            PRIMARY_USER_ID,
            OperationKey::from_bytes([7; 32]),
            SelectResultRequest {
                session_id: first.session_id,
                result_id: "torrent-5".to_owned(),
                translation_id: None,
                season: None,
                episode: None,
                scope: telegram_scope("default", None),
            },
        )
        .await
        .unwrap();
    let execution = service.execution_for(&selected.result_ref).await.unwrap();
    assert!(matches!(
        execution,
        media_contract::ExecutionSelectionDto::Prowlarr {
            title,
            uri,
            media_kind: MediaKindDto::Movie,
            ..
        } if title == "Release 5" && uri.ends_with("dn=5")
    ));
}

#[tokio::test]
async fn prowlarr_continues_after_a_partially_usable_provider_page() {
    let service = service(HashMap::from([(
        ProviderDto::Prowlarr,
        vec![
            ProviderPage {
                results: (0..4).map(prowlarr_result).collect(),
                provider_continuation: Some("5".to_owned()),
            },
            ProviderPage {
                results: vec![prowlarr_result(4)],
                provider_continuation: None,
            },
        ],
    )]));

    let first = service
        .start(PRIMARY_USER_ID, request(ProviderDto::Prowlarr))
        .await
        .unwrap();
    assert_eq!(first.results.len(), 4);
    let second = service
        .continue_search(
            PRIMARY_USER_ID,
            ContinueSearchRequest {
                continuation: first.continuation.unwrap(),
                scope: telegram_scope("default", None),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.results.len(), 1);
    assert_eq!(second.results[0].result_id(), "torrent-4");
}

#[tokio::test]
async fn search_session_rejects_the_same_owner_from_another_chat_or_thread() {
    let mut start = request(ProviderDto::Prowlarr);
    start.scope = telegram_scope("chat-a", Some("thread-a"));
    let service = service(HashMap::from([(
        ProviderDto::Prowlarr,
        vec![ProviderPage {
            results: (0..6).map(prowlarr_result).collect(),
            provider_continuation: None,
        }],
    )]));
    let page = service.start(PRIMARY_USER_ID, start).await.unwrap();
    let foreign_scope = telegram_scope("chat-a", Some("thread-b"));

    assert_eq!(
        service
            .continue_search(
                PRIMARY_USER_ID,
                ContinueSearchRequest {
                    continuation: page.continuation.clone().unwrap(),
                    scope: foreign_scope.clone(),
                },
            )
            .await
            .unwrap_err(),
        SearchError::Forbidden
    );
    assert_eq!(
        service
            .select(
                PRIMARY_USER_ID,
                OperationKey::from_bytes([17; 32]),
                SelectResultRequest {
                    session_id: page.session_id,
                    result_id: "torrent-0".to_owned(),
                    translation_id: None,
                    season: None,
                    episode: None,
                    scope: foreign_scope,
                },
            )
            .await
            .unwrap_err(),
        SearchError::Forbidden
    );
}

#[tokio::test]
async fn rezka_requires_explicit_translation_and_available_episode_without_fallback() {
    let public = SearchResultDto::Rezka {
        result_id: "rezka-show".to_owned(),
        title: "Show".to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Series,
        thumbnail_url: None,
        translations: vec![RezkaTranslationDto {
            id: 37,
            name: "Original".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
        }],
        availability: Some(SeriesAvailabilityDto {
            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
            incomplete: true,
            seasons: vec![SeasonAvailabilityDto {
                season: 1,
                episodes: vec![1, 2],
            }],
            tracking_prompt: Some(TrackingPromptDto {
                title: "Show".to_owned(),
                latest_season: 1,
                latest_episode: 2,
            }),
        }),
    };
    let mut pages = HashMap::new();
    pages.insert(
        ProviderDto::Rezka,
        vec![ProviderPage {
            results: vec![ProviderResult::rezka(public, "/show.html".to_owned(), 42)],
            provider_continuation: None,
        }],
    );
    let service = service(pages);
    let page = service
        .start(PRIMARY_USER_ID, request(ProviderDto::Rezka))
        .await
        .unwrap();

    for selection in [
        SelectResultRequest {
            session_id: page.session_id.clone(),
            result_id: "rezka-show".to_owned(),
            translation_id: None,
            season: Some(1),
            episode: Some(1),
            scope: telegram_scope("default", None),
        },
        SelectResultRequest {
            session_id: page.session_id.clone(),
            result_id: "rezka-show".to_owned(),
            translation_id: Some(37),
            season: Some(1),
            episode: Some(9),
            scope: telegram_scope("default", None),
        },
    ] {
        assert_eq!(
            service
                .select(PRIMARY_USER_ID, OperationKey::from_bytes([8; 32]), selection)
                .await
                .unwrap_err(),
            SearchError::InvalidRequest
        );
    }
    assert_eq!(
        service
            .continue_search(
                SECONDARY_USER_ID,
                ContinueSearchRequest {
                    continuation: format!("{}:5", page.session_id),
                    scope: telegram_scope("default", None),
                }
            )
            .await
            .unwrap_err(),
        SearchError::NotFound
    );

    let whole_series = service
        .select(
            PRIMARY_USER_ID,
            OperationKey::from_bytes([10; 32]),
            SelectResultRequest {
                session_id: page.session_id.clone(),
                result_id: "rezka-show".to_owned(),
                translation_id: Some(37),
                season: None,
                episode: None,
                scope: telegram_scope("default", None),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        service
            .execution_for(&whole_series.result_ref)
            .await
            .unwrap(),
        media_contract::ExecutionSelectionDto::Rezka {
            translation_id: 37,
            season: None,
            episode: None,
            episodes,
            ..
        } if episodes == vec![
            media_contract::EpisodeSnapshotDto { season: 1, episode: 1 },
            media_contract::EpisodeSnapshotDto { season: 1, episode: 2 },
        ]
    ));

    let job = service
        .select(
            PRIMARY_USER_ID,
            OperationKey::from_bytes([9; 32]),
            SelectResultRequest {
                session_id: page.session_id,
                result_id: "rezka-show".to_owned(),
                translation_id: Some(37),
                season: Some(1),
                episode: Some(2),
                scope: telegram_scope("default", None),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        service.execution_for(&job.result_ref).await.unwrap(),
        media_contract::ExecutionSelectionDto::Rezka {
            translation_id: 37,
            season: Some(1),
            episode: Some(2),
            episodes,
            ..
        } if episodes == vec![media_contract::EpisodeSnapshotDto { season: 1, episode: 2 }]
    ));
}
