use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use media::search::{
    DurableSearchService, ProviderPage, ProviderResult, SearchPersistence, SearchProvider,
    StoredSearchSession,
};
use media_api::{SearchError, SearchService};
use media_contract::{
    ContinueSearchRequest, MediaKindDto, ProviderDto, ProwlarrRankingDto, RezkaTranslationDto,
    SearchResultDto, SeasonAvailabilityDto, SelectResultRequest, SeriesAvailabilityDto,
    StartSearchRequest, TrackingPromptDto,
};
use media_core::{
    PRIMARY_USER_ID, EpisodeDiscoveryPort, Job, JobApplication, JobId, JobStore, NewJob,
    OperationKey, PortError, Provider, QueueStatus, TrackingId, TrackingScope,
    TrackingSubscription, UserId, SECONDARY_USER_ID,
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
    async fn queue_status(&self) -> Result<QueueStatus, PortError> {
        Ok(QueueStatus {
            queued: 0,
            active: false,
        })
    }
}

fn request(source: ProviderDto) -> StartSearchRequest {
    StartSearchRequest {
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
        .continue_search(PRIMARY_USER_ID, ContinueSearchRequest { continuation })
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
        },
        SelectResultRequest {
            session_id: page.session_id.clone(),
            result_id: "rezka-show".to_owned(),
            translation_id: Some(37),
            season: Some(1),
            episode: Some(9),
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
                    continuation: format!("{}:5", page.session_id)
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
            ..
        }
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
            ..
        }
    ));
}
