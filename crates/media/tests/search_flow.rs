use secrecy::SecretString;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use time::OffsetDateTime;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

use media::search::{
    ConcreteSearchProvider, DurableSearchService, ProviderEpisodeAvailability, ProviderPage,
    ProviderResult, SearchPersistence, SearchProvider, StoredSearchSession,
    TrackedEpisodeDownloader, VerifiedSeriesIdentity,
};
use media_api::{ChoiceSetSelection, SearchError, SearchService};
use media_contract::{
    AlternativeSearchRequest, ContinueSearchRequest, MediaKindDto, ProviderDto, ProwlarrRankingDto,
    RezkaTranslationDto, SearchResultDto, SearchScopeDto, SeasonAvailabilityDto,
    SelectResultRequest, SeriesAvailabilityDto, SeriesGroupIdentityDto, SeriesGroupSourceDto,
    StartSearchRequest, TrackingPromptDto,
};
use media_core::{
    PRIMARY_USER_ID, CanonicalEpisode, CanonicalEpisodeCoordinates, CanonicalMedia, CanonicalSeason,
    EpisodeAvailabilityPort, EpisodeAvailabilityRequest, EpisodeDiscovery, EpisodeDiscoveryPort,
    EpisodeId, EpisodeMappingConfirmation, EpisodeProviderMapping, ExternalNamespace,
    IdentityStore, Job, JobApplication, JobId, JobStore, MediaExternalReference, NewJob,
    NotifyScope, OperationKey, PortError, Provider, QueueStatus, ReleaseCandidate, ReleaseIdentity,
    ReleaseLifecycle, ReleaseMetadataPort, ReleaseMetadataResult, ReleasePrecision, ReleaseQuery,
    ReleaseQueryError, ReleaseSource, ScheduledEpisode, SourceChoiceAction,
    TrackedEpisodeDownloadPort, TrackingDownload, TrackingId, TrackingScope, TrackingSubscription,
    UserId, SECONDARY_USER_ID,
};

#[derive(Default)]
struct MemorySearchPersistence {
    sessions: Mutex<HashMap<String, StoredSearchSession>>,
    executions: Mutex<HashMap<String, media_contract::ExecutionSelectionDto>>,
}

async fn discover_selected_rezka_translation(
    thumbnail_url: Option<&str>,
) -> media_core::EpisodeDiscovery {
    let public = SearchResultDto::Rezka {
        result_id: "rezka-show".to_owned(),
        title: "Show".to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Series,
        thumbnail_url: thumbnail_url.map(str::to_owned),
        translations: vec![RezkaTranslationDto {
            id: 37,
            name: "Original".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
            seasons: vec![],
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
        Some(TrackingDownload::new("42".to_owned(), 37, 1).unwrap()),
    )
    .unwrap();

    discovery.available_episodes(&tracking).await.unwrap()
}

#[tokio::test]
async fn tracking_discovery_uses_the_selected_rezka_translation_snapshot() {
    let discovery = discover_selected_rezka_translation(Some(
        "https://static.tvmaze.com/uploads/images/original_untouched/show.jpg",
    ))
    .await;

    assert_eq!(
        discovery.episodes(),
        vec![
            media_core::EpisodeSnapshot::new(1, 1).unwrap(),
            media_core::EpisodeSnapshot::new(1, 2).unwrap(),
        ]
        .as_slice()
    );
    assert_eq!(
        discovery.poster_url(),
        Some("https://static.tvmaze.com/uploads/images/original_untouched/show.jpg")
    );
}

#[tokio::test]
async fn tracking_discovery_keeps_missing_rezka_thumbnail_as_none() {
    let discovery = discover_selected_rezka_translation(None).await;

    assert_eq!(discovery.poster_url(), None);
}

struct ShowReleaseProvider {
    matched: bool,
}

#[async_trait::async_trait]
impl ReleaseMetadataPort for ShowReleaseProvider {
    async fn query(
        &self,
        query: &ReleaseQuery,
    ) -> Result<ReleaseMetadataResult, ReleaseQueryError> {
        assert_eq!(query.title, "Show");
        assert_eq!(query.year, Some(2026));
        let candidate = ReleaseCandidate {
            source_id: 88,
            title: "Show".to_owned(),
            original_title: None,
            year: Some(2026),
            poster_url: Some("https://static.tvmaze.com/resolved.jpg".to_owned()),
            lifecycle: ReleaseLifecycle::Ongoing,
        };
        if self.matched {
            Ok(ReleaseMetadataResult::Matched {
                source: "tvmaze".to_owned(),
                fetched_at: "2026-08-10T00:00:00Z".to_owned(),
                show: candidate,
                precision: ReleasePrecision::Unknown,
                lifecycle: ReleaseLifecycle::Ongoing,
                released_episodes: 2,
                expected_episodes: None,
                next_episode: None,
                schedule: Vec::new(),
            })
        } else {
            Ok(ReleaseMetadataResult::ChoiceNeeded {
                source: "tvmaze".to_owned(),
                fetched_at: "2026-08-10T00:00:00Z".to_owned(),
                candidates: vec![candidate],
            })
        }
    }
}

async fn discover_show_with_release(
    matched: bool,
) -> Result<media_core::EpisodeDiscovery, PortError> {
    let public = SearchResultDto::Rezka {
        result_id: "rezka-show".to_owned(),
        title: "Show".to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Series,
        thumbnail_url: Some("https://rezka.test/show.jpg".to_owned()),
        translations: vec![RezkaTranslationDto {
            id: 37,
            name: "Original".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
            seasons: vec![],
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
    let discovery = media::search::ProviderEpisodeDiscovery::with_release(
        provider,
        Arc::new(ShowReleaseProvider { matched }),
    );
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
    discovery.available_episodes(&tracking).await
}

#[tokio::test]
async fn tracking_discovery_attaches_identity_only_after_an_exact_release_match() {
    let matched = discover_show_with_release(true).await.unwrap();
    assert_eq!(
        matched.release_identity(),
        Some(media_core::ReleaseIdentity::new(media_core::ReleaseSource::Tvmaze, 88).unwrap())
    );
    assert_eq!(
        matched.poster_url(),
        Some("https://static.tvmaze.com/resolved.jpg")
    );

    assert!(matches!(
        discover_show_with_release(false).await,
        Err(PortError::Conflict)
    ));
}

struct LaterSeasonReleaseProvider;

#[async_trait::async_trait]
impl ReleaseMetadataPort for LaterSeasonReleaseProvider {
    async fn query(
        &self,
        query: &ReleaseQuery,
    ) -> Result<ReleaseMetadataResult, ReleaseQueryError> {
        assert_eq!(query.title, "Slow Horses");
        assert_eq!(query.original_title, None);
        assert_eq!(query.year, None);
        Ok(ReleaseMetadataResult::Matched {
            source: "tvmaze".to_owned(),
            fetched_at: "2026-08-10T00:00:00Z".to_owned(),
            show: ReleaseCandidate {
                source_id: 95480,
                title: "Slow Horses".to_owned(),
                original_title: None,
                year: Some(2022),
                poster_url: None,
                lifecycle: ReleaseLifecycle::Ongoing,
            },
            precision: ReleasePrecision::Unknown,
            lifecycle: ReleaseLifecycle::Ongoing,
            released_episodes: 1,
            expected_episodes: None,
            next_episode: None,
            schedule: Vec::new(),
        })
    }
}

#[tokio::test]
async fn manual_tracking_later_season_marker_does_not_bind_tvmaze_to_release_year() {
    let title = "Slow Horses: Season Four [TV-4]";
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::from([(
            ProviderDto::Rezka,
            vec![ProviderPage {
                results: vec![ProviderResult::rezka(
                    SearchResultDto::Rezka {
                        result_id: "slow-horses-tv4".to_owned(),
                        title: title.to_owned(),
                        original_title: None,
                        year: Some(2024),
                        media_kind: MediaKindDto::Series,
                        thumbnail_url: None,
                        translations: vec![RezkaTranslationDto {
                            id: 37,
                            name: "Original".to_owned(),
                            premium: false,
                            director: false,
                            camrip: false,
                            has_ads: false,
                            seasons: Vec::new(),
                        }],
                        availability: Some(SeriesAvailabilityDto {
                            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
                            incomplete: true,
                            seasons: vec![SeasonAvailabilityDto {
                                season: 4,
                                episodes: vec![1],
                            }],
                            tracking_prompt: None,
                        }),
                    },
                    "/slow-horses-tv4.html".to_owned(),
                    95480,
                )],
                provider_continuation: None,
            }],
        )])),
    });
    let discovery = media::search::ProviderEpisodeDiscovery::with_release(
        provider,
        Arc::new(LaterSeasonReleaseProvider),
    );
    let tracking = TrackingSubscription::rehydrate(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        title.to_owned(),
        "Original".to_owned(),
        vec![media_core::EpisodeSnapshot::new(1, 1).unwrap()],
        TrackingScope::Personal,
        None,
    )
    .unwrap();

    let resolved = discovery.available_episodes(&tracking).await.unwrap();
    assert_eq!(
        resolved.release_identity(),
        Some(ReleaseIdentity::new(ReleaseSource::Tvmaze, 95480).unwrap())
    );
}

#[derive(Default)]
struct SameTitleReleaseProvider {
    queries: Mutex<Vec<ReleaseQuery>>,
}

#[async_trait::async_trait]
impl ReleaseMetadataPort for SameTitleReleaseProvider {
    async fn query(
        &self,
        query: &ReleaseQuery,
    ) -> Result<ReleaseMetadataResult, ReleaseQueryError> {
        self.queries.lock().unwrap().push(query.clone());
        let year = query.year.expect("candidate year");
        let candidate = ReleaseCandidate {
            source_id: u64::try_from(year).unwrap(),
            title: query.title.clone(),
            original_title: query.original_title.clone(),
            year: Some(year),
            poster_url: Some(format!("https://static.tvmaze.com/{year}.jpg")),
            lifecycle: ReleaseLifecycle::Ongoing,
        };
        Ok(ReleaseMetadataResult::Matched {
            source: "tvmaze".to_owned(),
            fetched_at: "2026-08-10T00:00:00Z".to_owned(),
            show: candidate,
            precision: ReleasePrecision::Unknown,
            lifecycle: ReleaseLifecycle::Ongoing,
            released_episodes: 1,
            expected_episodes: None,
            next_episode: None,
            schedule: Vec::new(),
        })
    }
}

#[tokio::test]
async fn tracking_discovery_rejects_same_title_rezka_results_from_different_years() {
    let result = |year: u16, title_id: u64| {
        ProviderResult::rezka(
            SearchResultDto::Rezka {
                result_id: format!("rezka-show-{year}"),
                title: "Show".to_owned(),
                original_title: Some(format!("Original {year}")),
                year: Some(year),
                media_kind: MediaKindDto::Series,
                thumbnail_url: Some(format!("https://rezka.test/{year}.jpg")),
                translations: vec![RezkaTranslationDto {
                    id: 37,
                    name: "Original".to_owned(),
                    premium: false,
                    director: false,
                    camrip: false,
                    has_ads: false,
                    seasons: vec![],
                }],
                availability: Some(SeriesAvailabilityDto {
                    lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
                    incomplete: true,
                    seasons: vec![SeasonAvailabilityDto {
                        season: 1,
                        episodes: vec![1],
                    }],
                    tracking_prompt: None,
                }),
            },
            format!("/show-{year}.html"),
            title_id,
        )
    };
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::from([(
            ProviderDto::Rezka,
            vec![ProviderPage {
                results: vec![result(2024, 42), result(2026, 43)],
                provider_continuation: None,
            }],
        )])),
    });
    let discovery = media::search::ProviderEpisodeDiscovery::with_release(
        provider,
        Arc::new(SameTitleReleaseProvider::default()),
    );
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

    assert!(matches!(
        discovery.available_episodes(&tracking).await,
        Err(PortError::Conflict)
    ));
}

#[tokio::test]
async fn tracking_discovery_resolves_candidates_before_matching_persisted_identity() {
    let result = |year: u16, title_id: u64| {
        ProviderResult::rezka(
            SearchResultDto::Rezka {
                result_id: format!("rezka-show-{year}"),
                title: "Show".to_owned(),
                original_title: Some(format!("Original {year}")),
                year: Some(year),
                media_kind: MediaKindDto::Series,
                thumbnail_url: Some(format!("https://rezka.test/{year}.jpg")),
                translations: vec![RezkaTranslationDto {
                    id: 37,
                    name: "Original".to_owned(),
                    premium: false,
                    director: false,
                    camrip: false,
                    has_ads: false,
                    seasons: vec![],
                }],
                availability: Some(SeriesAvailabilityDto {
                    lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
                    incomplete: true,
                    seasons: vec![SeasonAvailabilityDto {
                        season: 1,
                        episodes: vec![1],
                    }],
                    tracking_prompt: None,
                }),
            },
            format!("/show-{year}.html"),
            title_id,
        )
    };
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::from([(
            ProviderDto::Rezka,
            vec![
                ProviderPage {
                    results: vec![result(2024, 42), result(2026, 43)],
                    provider_continuation: None,
                },
                ProviderPage {
                    results: vec![result(2024, 42), result(2026, 43)],
                    provider_continuation: None,
                },
            ],
        )])),
    });
    let release = Arc::new(SameTitleReleaseProvider::default());
    let discovery =
        media::search::ProviderEpisodeDiscovery::with_release(provider, release.clone());
    let expected_identity =
        media_core::ReleaseIdentity::new(media_core::ReleaseSource::Tvmaze, 2026).unwrap();
    let tracking = TrackingSubscription::rehydrate_with_identity(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Show".to_owned(),
        "Original".to_owned(),
        vec![media_core::EpisodeSnapshot::new(1, 1).unwrap()],
        TrackingScope::Personal,
        Some(expected_identity),
        None,
    )
    .unwrap();

    for _ in 0..2 {
        let result = discovery.available_episodes(&tracking).await.unwrap();
        assert_eq!(result.release_identity(), Some(expected_identity));
        assert_eq!(
            result.poster_url(),
            Some("https://static.tvmaze.com/2026.jpg")
        );
    }
    let queries = release.queries.lock().unwrap();
    assert_eq!(queries.len(), 4);
    assert!(queries.iter().all(|query| query.source_id.is_none()));
}

struct FakeReleaseProvider {
    expected_source_id: Option<u64>,
    expected_title: &'static str,
}

#[async_trait::async_trait]
impl ReleaseMetadataPort for FakeReleaseProvider {
    async fn query(
        &self,
        query: &ReleaseQuery,
    ) -> Result<ReleaseMetadataResult, ReleaseQueryError> {
        assert_eq!(query.title, self.expected_title);
        assert_eq!(query.source_id, self.expected_source_id);
        Ok(ReleaseMetadataResult::Matched {
            source: "tvmaze".to_owned(),
            fetched_at: "2026-07-13T14:00:00Z".to_owned(),
            show: ReleaseCandidate {
                source_id: 7,
                title: self.expected_title.to_owned(),
                original_title: None,
                year: Some(2024),
                poster_url: Some("https://static.tvmaze.com/poster.jpg".to_owned()),
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
        Arc::new(FakeReleaseProvider {
            expected_source_id: None,
            expected_title: "Sugar",
        }),
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
        discovery
            .available_episodes(&tracking)
            .await
            .unwrap()
            .episodes(),
        vec![media_core::EpisodeSnapshot::new(1, 1).unwrap()].as_slice()
    );
}

#[tokio::test]
async fn calendar_tracking_normalizes_later_season_title_before_release_query() {
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::new()),
    });
    let discovery = media::search::ProviderEpisodeDiscovery::with_release(
        provider,
        Arc::new(FakeReleaseProvider {
            expected_source_id: None,
            expected_title: "Slow Horses",
        }),
    );
    let tracking = TrackingSubscription::rehydrate(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Slow Horses: Season Four [TV-4]".to_owned(),
        "release-calendar".to_owned(),
        vec![media_core::EpisodeSnapshot::new(2, 1).unwrap()],
        TrackingScope::Personal,
        None,
    )
    .unwrap();

    let result = discovery.available_episodes(&tracking).await.unwrap();
    assert_eq!(
        result.episodes(),
        vec![media_core::EpisodeSnapshot::new(1, 1).unwrap()].as_slice()
    );
}

#[tokio::test]
async fn calendar_tracking_uses_persisted_release_identity() {
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::new()),
    });
    let discovery = media::search::ProviderEpisodeDiscovery::with_release(
        provider,
        Arc::new(FakeReleaseProvider {
            expected_source_id: Some(7),
            expected_title: "Sugar",
        }),
    );
    let tracking = TrackingSubscription::rehydrate_with_identity(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Sugar".to_owned(),
        "release-calendar".to_owned(),
        vec![media_core::EpisodeSnapshot::new(1, 1).unwrap()],
        TrackingScope::Personal,
        Some(media_core::ReleaseIdentity::new(media_core::ReleaseSource::Tvmaze, 7).unwrap()),
        None,
    )
    .unwrap();

    assert_eq!(
        discovery
            .resolved_release_metadata(&tracking)
            .await
            .unwrap(),
        Some((
            media_core::ReleaseIdentity::new(media_core::ReleaseSource::Tvmaze, 7).unwrap(),
            "https://static.tvmaze.com/poster.jpg".to_owned(),
        ))
    );
    discovery.available_episodes(&tracking).await.unwrap();
}

#[tokio::test]
async fn tracked_episode_download_creates_one_exact_rezka_episode_execution_for_the_owner() {
    let public = SearchResultDto::Rezka {
        result_id: "rezka:42".to_owned(),
        title: "Blades of the Guardians S2".to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Series,
        thumbnail_url: Some("https://image.tmdb.org/t/p/w780/blades.jpg".to_owned()),
        translations: vec![RezkaTranslationDto {
            id: 19,
            name: "Studio Dub".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
            seasons: vec![],
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
            library_title: Some(library_title),
            thumbnail_url: Some(thumbnail_url),
            ..
        } if library_title == "Blades of the Guardians S2"
            && thumbnail_url == "https://image.tmdb.org/t/p/w780/blades.jpg"
    ));
}

#[tokio::test]
async fn choice_set_download_selects_the_exact_cached_result_without_generic_scope() {
    let choice_set = uuid::Uuid::new_v4();
    let session_id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("choice:{choice_set}:rezka").as_bytes(),
    )
    .to_string();
    let public = SearchResultDto::Rezka {
        result_id: "rezka:cached".to_owned(),
        title: "Cached Show".to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Series,
        thumbnail_url: None,
        translations: vec![RezkaTranslationDto {
            id: 7,
            name: "Dub".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
            seasons: vec![],
        }],
        availability: Some(SeriesAvailabilityDto {
            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
            incomplete: true,
            seasons: vec![SeasonAvailabilityDto {
                season: 3,
                episodes: vec![5],
            }],
            tracking_prompt: None,
        }),
    };
    let persistence = Arc::new(MemorySearchPersistence::default());
    persistence.sessions.lock().unwrap().insert(
        session_id.clone(),
        StoredSearchSession {
            id: session_id,
            owner: PRIMARY_USER_ID,
            request: StartSearchRequest {
                scope: SearchScopeDto {
                    platform: "system".to_owned(),
                    chat_id: "tracking:cached".to_owned(),
                    thread_id: Some("episode:3:5".to_owned()),
                },
                source: ProviderDto::Rezka,
                query: "Cached Show".to_owned(),
                media_kind: Some(MediaKindDto::Series),
                season: None,
                series_group: None,
                preferred_qualities: vec![],
                preferred_languages: vec![],
                preferred_codecs: vec![],
                preferred_release_groups: vec![],
            },
            expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
            results: vec![ProviderResult::rezka(public, "/cached.html".to_owned(), 99)],
            provider_continuation: None,
            query_aliases: vec![],
        },
    );
    let jobs = Arc::new(MemoryJobStore::default());
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::new()),
    });
    let service = DurableSearchService::new(
        persistence.clone(),
        provider,
        Arc::new(JobApplication::new(jobs.clone())),
    );

    let job = service
        .select_choice_set(
            PRIMARY_USER_ID,
            ChoiceSetSelection {
                operation: OperationKey::from_bytes([7; 32]),
                choice_set_id: choice_set.to_string(),
                source: ProviderDto::Rezka,
                result_id: "rezka:cached".to_owned(),
                translation_id: Some(7),
                season: Some(3),
                episode: Some(5),
            },
        )
        .await
        .unwrap();

    let execution = persistence
        .executions
        .lock()
        .unwrap()
        .get(&job.result_ref)
        .cloned()
        .unwrap();
    assert!(matches!(
        execution,
        media_contract::ExecutionSelectionDto::Rezka {
            title_id: 99,
            translation_id: 7,
            season: Some(3),
            episode: Some(5),
            ..
        }
    ));

    let omitted = service
        .select_choice_set(
            PRIMARY_USER_ID,
            ChoiceSetSelection {
                operation: OperationKey::from_bytes([17; 32]),
                choice_set_id: choice_set.to_string(),
                source: ProviderDto::Rezka,
                result_id: "rezka:cached".to_owned(),
                translation_id: Some(7),
                season: None,
                episode: None,
            },
        )
        .await
        .unwrap();
    let omitted_execution = persistence
        .executions
        .lock()
        .unwrap()
        .get(&omitted.result_ref)
        .cloned()
        .unwrap();
    assert!(matches!(
        omitted_execution,
        media_contract::ExecutionSelectionDto::Rezka {
            season: Some(3),
            episode: Some(5),
            ..
        }
    ));

    let mismatch = service
        .select_choice_set(
            PRIMARY_USER_ID,
            ChoiceSetSelection {
                operation: OperationKey::from_bytes([18; 32]),
                choice_set_id: choice_set.to_string(),
                source: ProviderDto::Rezka,
                result_id: "rezka:cached".to_owned(),
                translation_id: Some(7),
                season: Some(2),
                episode: Some(8),
            },
        )
        .await;
    assert!(matches!(mismatch, Err(SearchError::InvalidRequest)));

    let family = service
        .choice_set(SECONDARY_USER_ID, &choice_set.to_string())
        .await
        .unwrap();
    assert_eq!(family["status"], "refresh_required");
    service
        .select_choice_set(
            SECONDARY_USER_ID,
            ChoiceSetSelection {
                operation: OperationKey::from_bytes([8; 32]),
                choice_set_id: choice_set.to_string(),
                source: ProviderDto::Rezka,
                result_id: "rezka:cached".to_owned(),
                translation_id: Some(7),
                season: Some(3),
                episode: Some(5),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        jobs.jobs.lock().unwrap().last().map(Job::owner_id),
        Some(SECONDARY_USER_ID)
    );
}

#[tokio::test]
async fn choice_set_refreshes_empty_source_and_keeps_healthy_provider_after_failure() {
    let choice_set = uuid::Uuid::new_v4();
    let persistence = Arc::new(MemorySearchPersistence::default());
    let expired = OffsetDateTime::now_utc() - time::Duration::minutes(1);
    for source in [ProviderDto::Rezka, ProviderDto::Prowlarr] {
        let source_name = match source {
            ProviderDto::Rezka => "rezka",
            ProviderDto::Prowlarr => "prowlarr",
        };
        let id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("choice:{choice_set}:{source_name}").as_bytes(),
        )
        .to_string();
        persistence.sessions.lock().unwrap().insert(
            id.clone(),
            StoredSearchSession {
                id,
                owner: PRIMARY_USER_ID,
                request: StartSearchRequest {
                    scope: SearchScopeDto {
                        platform: "system".to_owned(),
                        chat_id: "tracking:cached".to_owned(),
                        thread_id: Some("episode:3:5".to_owned()),
                    },
                    source,
                    query: "Tracked Show".to_owned(),
                    media_kind: Some(MediaKindDto::Series),
                    season: Some(3),
                    series_group: None,
                    preferred_qualities: vec![],
                    preferred_languages: vec![],
                    preferred_codecs: vec![],
                    preferred_release_groups: vec![],
                },
                expires_at: expired,
                results: vec![],
                provider_continuation: None,
                query_aliases: vec!["Tracked Show".to_owned()],
            },
        );
    }
    let mut exact = prowlarr_result(1);
    if let SearchResultDto::Prowlarr { title, .. } = &mut exact.public {
        *title = "Tracked Show [S03E05]".to_owned();
    }
    let provider = Arc::new(FakeProvider {
        // Rezka deliberately has no page and fails; Prowlarr remains healthy.
        pages: Mutex::new(HashMap::from([(
            ProviderDto::Prowlarr,
            vec![ProviderPage {
                results: vec![exact],
                provider_continuation: None,
            }],
        )])),
    });
    let service = DurableSearchService::new(
        persistence,
        provider,
        Arc::new(JobApplication::new(Arc::new(MemoryJobStore::default()))),
    );

    let refreshed = service
        .refresh_choice_set(PRIMARY_USER_ID, &choice_set.to_string())
        .await
        .unwrap();
    assert_eq!(refreshed["status"], "refresh_required");
    assert_eq!(refreshed["prowlarr_count"], 1);
    assert_eq!(refreshed["rezka_count"], 0);
    assert!(
        refreshed["sources"]["prowlarr"]["results"]
            .as_array()
            .is_some_and(|results| results.len() == 1)
    );
}

#[tokio::test]
async fn choice_set_refresh_matches_rezka_root_for_later_season_query() {
    let choice_set = uuid::Uuid::new_v4();
    let session_id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("choice:{choice_set}:rezka").as_bytes(),
    )
    .to_string();
    let persistence = Arc::new(MemorySearchPersistence::default());
    persistence.sessions.lock().unwrap().insert(
        session_id.clone(),
        StoredSearchSession {
            id: session_id,
            owner: PRIMARY_USER_ID,
            request: StartSearchRequest {
                scope: SearchScopeDto {
                    platform: "system".to_owned(),
                    chat_id: "tracking:calendar".to_owned(),
                    thread_id: Some("episode:2:1".to_owned()),
                },
                source: ProviderDto::Rezka,
                query: "Sugar: Season Two [TV-2]".to_owned(),
                media_kind: Some(MediaKindDto::Series),
                season: None,
                series_group: None,
                preferred_qualities: vec![],
                preferred_languages: vec![],
                preferred_codecs: vec![],
                preferred_release_groups: vec![],
            },
            expires_at: OffsetDateTime::now_utc() - time::Duration::minutes(1),
            results: vec![],
            provider_continuation: None,
            query_aliases: vec!["Sugar: Season Two [TV-2]".to_owned()],
        },
    );
    let result = SearchResultDto::Rezka {
        result_id: "rezka-root".to_owned(),
        title: "Sugar".to_owned(),
        original_title: None,
        year: Some(2024),
        media_kind: MediaKindDto::Series,
        thumbnail_url: None,
        translations: vec![RezkaTranslationDto {
            id: 7,
            name: "Dub".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
            seasons: vec![SeasonAvailabilityDto {
                season: 2,
                episodes: vec![1],
            }],
        }],
        availability: Some(SeriesAvailabilityDto {
            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
            incomplete: false,
            seasons: vec![SeasonAvailabilityDto {
                season: 2,
                episodes: vec![1],
            }],
            tracking_prompt: None,
        }),
    };
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::from([(
            ProviderDto::Rezka,
            vec![ProviderPage {
                results: vec![ProviderResult::rezka(result, "/sugar.html".to_owned(), 42)],
                provider_continuation: None,
            }],
        )])),
    });
    let service = DurableSearchService::new(
        persistence,
        provider,
        Arc::new(JobApplication::new(Arc::new(MemoryJobStore::default()))),
    );

    let refreshed = service
        .refresh_choice_set(PRIMARY_USER_ID, &choice_set.to_string())
        .await
        .unwrap();
    assert_eq!(refreshed["rezka_count"], 1);
    assert_eq!(refreshed["status"], "refresh_required");
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

    async fn session_for_user(
        &self,
        id: &str,
        user: UserId,
    ) -> Result<StoredSearchSession, SearchError> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .filter(|session| {
                session.owner == user
                    || (session.owner == PRIMARY_USER_ID && user == SECONDARY_USER_ID)
                    || (session.owner == SECONDARY_USER_ID && user == PRIMARY_USER_ID)
            })
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

    async fn verify_series_identity(
        &self,
        selected: &media_contract::SearchResultDto,
        requested: Option<SeriesGroupIdentityDto>,
    ) -> Result<Option<VerifiedSeriesIdentity>, SearchError> {
        match requested {
            Some(SeriesGroupIdentityDto {
                source: SeriesGroupSourceDto::Tmdb,
                source_id: 94997,
            }) => Ok(Some(VerifiedSeriesIdentity {
                tmdb_id: 94997,
                canonical_title: "Магия и мускулы".to_owned(),
                legacy_path_titles: vec!["rezka-series-tmdb-94997".to_owned()],
            })),
            Some(SeriesGroupIdentityDto {
                source: SeriesGroupSourceDto::Tvmaze,
                source_id: 88,
            }) => Ok(Some(VerifiedSeriesIdentity {
                tmdb_id: 94997,
                canonical_title: "Магия и мускулы".to_owned(),
                legacy_path_titles: vec![
                    "tvmaze-88".to_owned(),
                    "rezka-series-tvmaze-88".to_owned(),
                    "rezka-series-tmdb-94997".to_owned(),
                ],
            })),
            Some(_) => Err(SearchError::InvalidRequest),
            None => {
                let _ = selected;
                Ok(None)
            }
        }
    }
}

struct CountingAvailabilityProvider {
    prowlarr_calls: Mutex<Vec<String>>,
    page: Mutex<Option<ProviderPage>>,
}

#[async_trait::async_trait]
impl SearchProvider for CountingAvailabilityProvider {
    async fn search(
        &self,
        request: &StartSearchRequest,
        _: Option<&str>,
    ) -> Result<ProviderPage, SearchError> {
        if request.source != ProviderDto::Prowlarr {
            return Err(SearchError::Provider);
        }
        self.prowlarr_calls
            .lock()
            .unwrap()
            .push(request.query.clone());
        self.page
            .lock()
            .unwrap()
            .take()
            .ok_or(SearchError::Provider)
    }
}

#[tokio::test]
async fn tracking_prowlarr_availability_uses_one_search_for_results_and_readiness() {
    let mut result = prowlarr_result(1);
    let SearchResultDto::Prowlarr { title, .. } = &mut result.public else {
        panic!("expected Prowlarr result");
    };
    *title = "Original Show [S01E05] (2026)".to_owned();
    let provider = Arc::new(CountingAvailabilityProvider {
        prowlarr_calls: Mutex::new(Vec::new()),
        page: Mutex::new(Some(ProviderPage {
            results: vec![result],
            provider_continuation: None,
        })),
    });
    let persistence = Arc::new(MemorySearchPersistence::default());
    let availability = ProviderEpisodeAvailability::new(provider.clone(), persistence);
    let tracking = TrackingSubscription::rehydrate(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Tracked Show".to_owned(),
        "release-calendar".to_owned(),
        vec![media_core::EpisodeSnapshot::new(1, 4).unwrap()],
        TrackingScope::Personal,
        None,
    )
    .unwrap();
    let discovery = EpisodeDiscovery::new(
        vec![media_core::EpisodeSnapshot::new(1, 5).unwrap()],
        "Localized Show".to_owned(),
        Some("Original Show".to_owned()),
    )
    .unwrap();

    let result = availability
        .probe(EpisodeAvailabilityRequest::new(
            &tracking,
            &discovery,
            media_core::EpisodeSnapshot::new(1, 5).unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(result.actions(), vec![SourceChoiceAction::Prowlarr]);
    assert_eq!(result.prowlarr_count(), 1);
    assert_eq!(
        *provider.prowlarr_calls.lock().unwrap(),
        vec!["Original Show"]
    );
}

#[tokio::test]
async fn tracking_prowlarr_single_search_failure_is_unknown() {
    let provider = Arc::new(CountingAvailabilityProvider {
        prowlarr_calls: Mutex::new(Vec::new()),
        page: Mutex::new(None),
    });
    let persistence = Arc::new(MemorySearchPersistence::default());
    let availability = ProviderEpisodeAvailability::new(provider.clone(), persistence);
    let tracking = TrackingSubscription::rehydrate(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Tracked Show".to_owned(),
        "release-calendar".to_owned(),
        vec![media_core::EpisodeSnapshot::new(1, 4).unwrap()],
        TrackingScope::Personal,
        None,
    )
    .unwrap();
    let discovery = EpisodeDiscovery::new(
        vec![media_core::EpisodeSnapshot::new(1, 5).unwrap()],
        "Localized Show".to_owned(),
        Some("Original Show".to_owned()),
    )
    .unwrap();

    let result = availability
        .probe(EpisodeAvailabilityRequest::new(
            &tracking,
            &discovery,
            media_core::EpisodeSnapshot::new(1, 5).unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(result.prowlarr(), media_core::ProviderAvailability::Unknown);
    assert_eq!(
        *provider.prowlarr_calls.lock().unwrap(),
        vec!["Original Show"]
    );
}

#[tokio::test]
async fn tracking_rezka_availability_matches_later_season_root_title() {
    let result = SearchResultDto::Rezka {
        result_id: "rezka-slow-horses".to_owned(),
        title: "Slow Horses".to_owned(),
        original_title: None,
        year: Some(2022),
        media_kind: MediaKindDto::Series,
        thumbnail_url: None,
        translations: vec![RezkaTranslationDto {
            id: 7,
            name: "Dub".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
            seasons: vec![SeasonAvailabilityDto {
                season: 4,
                episodes: vec![1],
            }],
        }],
        availability: Some(SeriesAvailabilityDto {
            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
            incomplete: false,
            seasons: vec![SeasonAvailabilityDto {
                season: 4,
                episodes: vec![1],
            }],
            tracking_prompt: None,
        }),
    };
    let provider = Arc::new(FakeProvider {
        pages: Mutex::new(HashMap::from([(
            ProviderDto::Rezka,
            vec![ProviderPage {
                results: vec![ProviderResult::rezka(
                    result,
                    "/slow-horses.html".to_owned(),
                    42,
                )],
                provider_continuation: None,
            }],
        )])),
    });
    let availability =
        ProviderEpisodeAvailability::new(provider, Arc::new(MemorySearchPersistence::default()));
    let tracking = TrackingSubscription::rehydrate(
        TrackingId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        "Slow Horses: Season Four [TV-4]".to_owned(),
        "release-calendar".to_owned(),
        vec![media_core::EpisodeSnapshot::new(4, 1).unwrap()],
        TrackingScope::Personal,
        None,
    )
    .unwrap();
    let discovery = EpisodeDiscovery::new(
        vec![media_core::EpisodeSnapshot::new(4, 1).unwrap()],
        "Slow Horses: Season Four [TV-4]".to_owned(),
        None,
    )
    .unwrap();

    let result = availability
        .probe(EpisodeAvailabilityRequest::new(
            &tracking,
            &discovery,
            media_core::EpisodeSnapshot::new(4, 1).unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(result.actions(), vec![SourceChoiceAction::Rezka]);
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
    async fn cancel(
        &self,
        _: OperationKey,
        _: JobId,
        _: UserId,
        _: Option<u64>,
    ) -> Result<Option<Job>, PortError> {
        Ok(None)
    }
    async fn retry(
        &self,
        _: OperationKey,
        id: JobId,
        owner: UserId,
        _: Option<u64>,
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
                library_title: None,
                library_path_title: None,
                library_path_aliases: Vec::new(),
                thumbnail_url: None,
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
        series_group: None,
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
            thumbnail_url: None,
            website_url: None,
            indexer: Some("Mock".to_owned()),
            size_bytes: 1000 + index as u64,
            seeders: 10,
            leechers: 0,
            published_at: None,
            age_days: None,
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
async fn alternative_search_is_owner_scoped_and_uses_the_opposite_provider() {
    let persistence = Arc::new(MemorySearchPersistence::default());
    let jobs = Arc::new(MemoryJobStore::default());
    let job_id = JobId::new();
    let result_ref = "selection:original";
    jobs.jobs.lock().unwrap().push(
        Job::rehydrate(
            job_id,
            PRIMARY_USER_ID,
            Provider::Rezka,
            result_ref.to_owned(),
            media_core::JobState::Failed,
            None,
            NotifyScope::Initiator,
        )
        .unwrap(),
    );
    persistence
        .insert_execution(
            result_ref.to_owned(),
            media_contract::ExecutionSelectionDto::Rezka {
                locator: "/show.html".to_owned(),
                title_id: 42,
                media_kind: MediaKindDto::Series,
                translation_id: 19,
                translation: Some("Studio Dub".to_owned()),
                director: false,
                camrip: false,
                has_ads: false,
                season: Some(2),
                episode: Some(8),
                episodes: vec![media_contract::EpisodeSnapshotDto {
                    season: 2,
                    episode: 8,
                }],
                episode_mappings: Vec::new(),
                ambiguous_episodes: Vec::new(),
                release_year: Some(2026),
                library_title: None,
                library_path_title: None,
                library_path_aliases: Vec::new(),
                thumbnail_url: None,
                title: "Blades of the Guardians".to_owned(),
            },
        )
        .await
        .unwrap();
    let service = DurableSearchService::new(
        persistence.clone(),
        Arc::new(FakeProvider {
            pages: Mutex::new(HashMap::from([(
                ProviderDto::Prowlarr,
                vec![ProviderPage {
                    results: vec![prowlarr_result(1)],
                    provider_continuation: None,
                }],
            )])),
        }),
        Arc::new(JobApplication::new(jobs.clone())),
    );
    let scope = telegram_scope("42", None);

    let page = service
        .start_alternative(
            PRIMARY_USER_ID,
            job_id,
            AlternativeSearchRequest {
                scope: scope.clone(),
            },
        )
        .await
        .unwrap();

    assert_eq!(page.source, ProviderDto::Prowlarr);
    assert_eq!(page.results.len(), 1);
    let session = persistence
        .sessions
        .lock()
        .unwrap()
        .get(&page.session_id)
        .cloned()
        .unwrap();
    assert_eq!(session.request.query, "Blades of the Guardians");
    assert_eq!(session.request.media_kind, Some(MediaKindDto::Series));
    assert_eq!(session.request.season, Some(2));
    assert_eq!(session.request.scope, scope);
    assert_eq!(jobs.jobs.lock().unwrap().len(), 1);
    assert_eq!(
        service
            .start_alternative(
                SECONDARY_USER_ID,
                job_id,
                AlternativeSearchRequest {
                    scope: telegram_scope("99", None),
                },
            )
            .await
            .unwrap_err(),
        SearchError::NotFound
    );
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
async fn rezka_movie_path_is_stable_across_root_alias_case_and_unicode_queries() {
    let public = SearchResultDto::Rezka {
        result_id: "rezka-avatar".to_owned(),
        title:
            "Аватар Аанг: Последний маг воздуха / Легенда об Аанге: Последний маг воздуха / Café"
                .to_owned(),
        original_title: None,
        year: Some(2026),
        media_kind: MediaKindDto::Movie,
        thumbnail_url: None,
        translations: vec![RezkaTranslationDto {
            id: 7,
            name: "Дубляж".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
            seasons: vec![],
        }],
        availability: None,
    };
    let provider_page = ProviderPage {
        results: vec![ProviderResult::rezka(public, "/avatar.html".to_owned(), 42)],
        provider_continuation: None,
    };
    let service = service(HashMap::from([(
        ProviderDto::Rezka,
        vec![provider_page; 5],
    )]));
    for (index, query) in [
        "Аватар Аанг",
        "Аватар Аанг: Последний маг воздуха",
        "Легенда об Аанге: Последний маг воздуха",
        "легенда ОБ аанге: последний маг воздуха",
        "Cafe\u{301}",
    ]
    .into_iter()
    .enumerate()
    {
        let mut search = request(ProviderDto::Rezka);
        search.query = query.to_owned();
        search.media_kind = Some(MediaKindDto::Movie);
        let page = service.start(PRIMARY_USER_ID, search).await.unwrap();
        let job = service
            .select(
                PRIMARY_USER_ID,
                OperationKey::from_bytes([u8::try_from(index + 41).unwrap(); 32]),
                SelectResultRequest {
                    session_id: page.session_id,
                    result_id: "rezka-avatar".to_owned(),
                    translation_id: Some(7),
                    season: None,
                    episode: None,
                    scope: telegram_scope("default", None),
                },
            )
            .await
            .unwrap();
        let execution = service.execution_for(&job.result_ref).await.unwrap();

        assert!(matches!(
            execution,
            media_contract::ExecutionSelectionDto::Rezka {
                media_kind: MediaKindDto::Movie,
                library_path_title: Some(library_path_title),
                ..
            } if library_path_title == "rezka-42"
        ));
    }
}

#[tokio::test]
async fn rezka_series_path_ignores_provider_variants_and_groups_only_by_explicit_identity() {
    let result = |result_id: &str, title: &str, title_id| {
        ProviderResult::rezka(
            SearchResultDto::Rezka {
                result_id: result_id.to_owned(),
                title: title.to_owned(),
                original_title: None,
                year: Some(2026),
                media_kind: MediaKindDto::Series,
                thumbnail_url: None,
                translations: vec![RezkaTranslationDto {
                    id: 7,
                    name: "Дубляж".to_owned(),
                    premium: false,
                    director: false,
                    camrip: false,
                    has_ads: false,
                    seasons: vec![SeasonAvailabilityDto {
                        season: 1,
                        episodes: vec![1],
                    }],
                }],
                availability: Some(SeriesAvailabilityDto {
                    lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
                    incomplete: true,
                    seasons: vec![SeasonAvailabilityDto {
                        season: 1,
                        episodes: vec![1],
                    }],
                    tracking_prompt: None,
                }),
            },
            format!("/{title_id}.html"),
            title_id,
        )
    };
    let pages = [
        ("same-a", "Магия и мускулы [ТВ-1]", 101),
        ("same-b", "магия И МУСКУЛЫ", 101),
        ("same-c", "Cafe\u{301} / CAFÉ [TV-1]", 101),
        ("season-one", "Магия и мускулы [ТВ-1]", 101),
        ("season-two", "Магия и мускулы [ТВ-2]", 202),
    ]
    .map(|(id, title, title_id)| ProviderPage {
        results: vec![result(id, title, title_id)],
        provider_continuation: None,
    });
    let service = service(HashMap::from([(ProviderDto::Rezka, pages.to_vec())]));
    for (index, result_id) in ["same-a", "same-b", "same-c"].into_iter().enumerate() {
        let mut search = request(ProviderDto::Rezka);
        search.query = "магия И мускулы".to_owned();
        search.media_kind = Some(MediaKindDto::Series);
        let page = service.start(PRIMARY_USER_ID, search).await.unwrap();
        let job = service
            .select(
                PRIMARY_USER_ID,
                OperationKey::from_bytes([u8::try_from(index + 70).unwrap(); 32]),
                SelectResultRequest {
                    session_id: page.session_id,
                    result_id: result_id.to_owned(),
                    translation_id: Some(7),
                    season: Some(1),
                    episode: None,
                    scope: telegram_scope("default", None),
                },
            )
            .await
            .unwrap();
        let execution = service.execution_for(&job.result_ref).await.unwrap();
        let media_contract::ExecutionSelectionDto::Rezka {
            library_path_title: Some(path),
            ..
        } = execution
        else {
            panic!("Rezka series execution must have a path title");
        };
        assert_eq!(path, "rezka-101");
    }

    let groups = [
        SeriesGroupIdentityDto {
            source: SeriesGroupSourceDto::Tmdb,
            source_id: 94997,
        },
        SeriesGroupIdentityDto {
            source: SeriesGroupSourceDto::Tvmaze,
            source_id: 88,
        },
    ];
    for (index, (result_id, group)) in ["season-one", "season-two"]
        .into_iter()
        .zip(groups)
        .enumerate()
    {
        let mut search = request(ProviderDto::Rezka);
        search.query = "Магия и мускулы".to_owned();
        search.media_kind = Some(MediaKindDto::Series);
        search.series_group = Some(group);
        let page = service.start(PRIMARY_USER_ID, search).await.unwrap();
        let job = service
            .select(
                PRIMARY_USER_ID,
                OperationKey::from_bytes([u8::try_from(index + 80).unwrap(); 32]),
                SelectResultRequest {
                    session_id: page.session_id,
                    result_id: result_id.to_owned(),
                    translation_id: Some(7),
                    season: Some(1),
                    episode: None,
                    scope: telegram_scope("default", None),
                },
            )
            .await
            .unwrap();
        let execution = service.execution_for(&job.result_ref).await.unwrap();
        let media_contract::ExecutionSelectionDto::Rezka {
            library_path_title: Some(path),
            library_path_aliases,
            ..
        } = execution
        else {
            panic!("expected Rezka series execution");
        };
        assert_eq!(path, "Магия и мускулы {tmdb-94997}");
        if index == 0 {
            assert_eq!(
                library_path_aliases,
                [
                    "rezka-101",
                    "rezka-series-tmdb-94997",
                    "Магия и мускулы ТВ-1",
                ]
            );
        } else {
            assert_eq!(
                library_path_aliases,
                [
                    "rezka-202",
                    "rezka-series-tmdb-94997",
                    "rezka-series-tvmaze-88",
                    "tvmaze-88",
                    "Магия и мускулы ТВ-2",
                ],
            );
        }
    }
}

#[tokio::test]
async fn unverified_tmdb_series_identity_cannot_select_a_physical_path() {
    let result = ProviderResult::rezka(
        SearchResultDto::Rezka {
            result_id: "wrong-id".to_owned(),
            title: "Verified Show [TV-1]".to_owned(),
            original_title: Some("Verified Show".to_owned()),
            year: Some(2026),
            media_kind: MediaKindDto::Series,
            thumbnail_url: None,
            translations: vec![RezkaTranslationDto {
                id: 7,
                name: "Dub".to_owned(),
                premium: false,
                director: false,
                camrip: false,
                has_ads: false,
                seasons: vec![SeasonAvailabilityDto {
                    season: 1,
                    episodes: vec![1],
                }],
            }],
            availability: Some(SeriesAvailabilityDto {
                lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
                incomplete: true,
                seasons: vec![SeasonAvailabilityDto {
                    season: 1,
                    episodes: vec![1],
                }],
                tracking_prompt: None,
            }),
        },
        "/wrong.html".to_owned(),
        42,
    );
    let service = service(HashMap::from([(
        ProviderDto::Rezka,
        vec![ProviderPage {
            results: vec![result],
            provider_continuation: None,
        }],
    )]));
    let mut search = request(ProviderDto::Rezka);
    search.query = "Verified Show".to_owned();
    search.media_kind = Some(MediaKindDto::Series);
    search.series_group = Some(SeriesGroupIdentityDto {
        source: SeriesGroupSourceDto::Tmdb,
        source_id: 999,
    });
    let page = service.start(PRIMARY_USER_ID, search).await.unwrap();

    let selected = service
        .select(
            PRIMARY_USER_ID,
            OperationKey::from_bytes([91; 32]),
            SelectResultRequest {
                session_id: page.session_id,
                result_id: "wrong-id".to_owned(),
                translation_id: Some(7),
                season: Some(1),
                episode: Some(1),
                scope: telegram_scope("default", None),
            },
        )
        .await;

    assert!(matches!(selected, Err(SearchError::InvalidRequest)));
}

fn concrete_identity_provider(server: &MockServer) -> ConcreteSearchProvider {
    let config = media_integrations::tmdb::TmdbConfig::new(
        format!("{}/3/", server.uri()).parse().unwrap(),
        SecretString::from("test-key"),
        "ru-RU",
        Duration::from_secs(2),
    )
    .unwrap();
    ConcreteSearchProvider::new(None, None).with_tmdb(Some(Arc::new(
        media_integrations::tmdb::TmdbClient::new(config).unwrap(),
    )))
}

fn later_season_result(title: &str, year: u16) -> SearchResultDto {
    SearchResultDto::Rezka {
        result_id: "selected-rezka-title".to_owned(),
        title: title.to_owned(),
        original_title: None,
        year: Some(year),
        media_kind: MediaKindDto::Series,
        thumbnail_url: None,
        translations: Vec::new(),
        availability: None,
    }
}

#[tokio::test]
async fn concrete_tmdb_verification_binds_tv2_title_root_but_rejects_wrong_identity() {
    let server = MockServer::start().await;
    Mock::given(path("/3/search/tv"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_results": 1,
            "results": [{
                "id": 94997,
                "name": "Магия и мускулы",
                "original_name": "Mashle: Magic and Muscles",
                "first_air_date": "2023-04-08"
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(path("/3/tv/94997"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 94997,
            "name": "Магия и мускулы",
            "original_name": "Mashle: Magic and Muscles",
            "first_air_date": "2023-04-08"
        })))
        .mount(&server)
        .await;
    Mock::given(path("/3/tv/999"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 999,
            "name": "Unrelated Show",
            "first_air_date": "2023-04-08"
        })))
        .mount(&server)
        .await;
    let provider = concrete_identity_provider(&server);
    let selected = later_season_result(
        "Магия и мускулы: Экзамен на звание Вестника Бога [ТВ-2]",
        2024,
    );

    let verified = provider
        .verify_series_identity(
            &selected,
            Some(SeriesGroupIdentityDto {
                source: SeriesGroupSourceDto::Tmdb,
                source_id: 94997,
            }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(verified.tmdb_id, 94997);
    assert_eq!(
        provider
            .verify_series_identity(
                &selected,
                Some(SeriesGroupIdentityDto {
                    source: SeriesGroupSourceDto::Tmdb,
                    source_id: 999,
                }),
            )
            .await,
        Err(SearchError::InvalidRequest),
    );
}

#[tokio::test]
async fn concrete_tmdb_verification_rejects_ambiguous_later_season_remake_title() {
    let server = MockServer::start().await;
    Mock::given(path("/3/tv/101"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 101,
            "name": "The Office",
            "first_air_date": "2001-07-09"
        })))
        .mount(&server)
        .await;
    Mock::given(path("/3/search/tv"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_results": 2,
            "results": [
                {"id": 101, "name": "The Office", "first_air_date": "2001-07-09"},
                {"id": 202, "name": "The Office", "first_air_date": "2005-03-24"}
            ]
        })))
        .mount(&server)
        .await;
    let provider = concrete_identity_provider(&server);
    let selected = later_season_result("The Office: Season Two [TV-2]", 2006);

    assert_eq!(
        provider
            .verify_series_identity(
                &selected,
                Some(SeriesGroupIdentityDto {
                    source: SeriesGroupSourceDto::Tmdb,
                    source_id: 101,
                }),
            )
            .await,
        Err(SearchError::InvalidRequest),
    );
}

#[tokio::test]
async fn concrete_tmdb_auto_binding_accepts_tv4_later_release_year() {
    let server = MockServer::start().await;
    Mock::given(path("/3/search/tv"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_results": 1,
            "results": [{
                "id": 95480,
                "name": "Slow Horses",
                "first_air_date": "2022-04-01"
            }]
        })))
        .mount(&server)
        .await;
    let provider = concrete_identity_provider(&server);
    let selected = later_season_result("Slow Horses: Season Four [TV-4]", 2024);

    let verified = provider
        .verify_series_identity(&selected, None)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(verified.tmdb_id, 95480);
}

#[tokio::test]
async fn concrete_tvmaze_identity_uses_imdb_only_after_tvdb_candidate_fails_validation() {
    let server = MockServer::start().await;
    Mock::given(path("/shows/88"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 88,
            "name": "Show",
            "premiered": "2024-01-01",
            "externals": {"thetvdb": 123, "imdb": "tt999"}
        })))
        .mount(&server)
        .await;
    Mock::given(path("/3/find/123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tv_results": [{
                "id": 111,
                "name": "Unrelated Show",
                "first_air_date": "2024-01-01"
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(path("/3/find/tt999"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tv_results": [{
                "id": 94997,
                "name": "Show",
                "first_air_date": "2024-01-01"
            }]
        })))
        .mount(&server)
        .await;
    let tmdb_config = media_integrations::tmdb::TmdbConfig::new(
        format!("{}/3/", server.uri()).parse().unwrap(),
        SecretString::from("test-key"),
        "en-US",
        Duration::from_secs(2),
    )
    .unwrap();
    let tvmaze_config = media_integrations::tvmaze::TvmazeConfig::new(
        format!("{}/", server.uri()).parse().unwrap(),
        Duration::from_secs(2),
        "media-test".to_owned(),
        0,
    )
    .unwrap();
    let provider = ConcreteSearchProvider::new(None, None)
        .with_tmdb(Some(Arc::new(
            media_integrations::tmdb::TmdbClient::new(tmdb_config).unwrap(),
        )))
        .with_tvmaze(Arc::new(
            media_integrations::tvmaze::TvmazeClient::new(tvmaze_config).unwrap(),
        ));

    let selected = later_season_result("Show: Season Two [TV-2]", 2024);
    let verified = provider
        .verify_series_identity(
            &selected,
            Some(SeriesGroupIdentityDto {
                source: SeriesGroupSourceDto::Tvmaze,
                source_id: 88,
            }),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(verified.tmdb_id, 94997);
}

#[tokio::test]
async fn prowlarr_paginates_ten_and_runner_gets_only_the_exact_selected_result() {
    let mut pages = HashMap::new();
    pages.insert(
        ProviderDto::Prowlarr,
        vec![ProviderPage {
            results: (0..12).map(prowlarr_result).collect(),
            provider_continuation: None,
        }],
    );
    let service = service(pages);

    let first = service
        .start(PRIMARY_USER_ID, request(ProviderDto::Prowlarr))
        .await
        .unwrap();
    assert_eq!(first.results.len(), 10);
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
    assert_eq!(second.results.len(), 2);

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
async fn prowlarr_selection_preserves_a_single_episode_target() {
    let service = service(HashMap::from([(
        ProviderDto::Prowlarr,
        vec![ProviderPage {
            results: vec![prowlarr_result(0)],
            provider_continuation: None,
        }],
    )]));
    let page = service
        .start(
            PRIMARY_USER_ID,
            StartSearchRequest {
                scope: telegram_scope("default", None),
                source: ProviderDto::Prowlarr,
                query: "Example Show".to_owned(),
                media_kind: Some(MediaKindDto::Series),
                season: Some(2),
                series_group: None,
                preferred_qualities: vec![],
                preferred_languages: vec![],
                preferred_codecs: vec![],
                preferred_release_groups: vec![],
            },
        )
        .await
        .unwrap();

    let job = service
        .select(
            PRIMARY_USER_ID,
            OperationKey::from_bytes([31; 32]),
            SelectResultRequest {
                session_id: page.session_id,
                result_id: "torrent-0".to_owned(),
                translation_id: None,
                season: Some(2),
                episode: Some(7),
                scope: telegram_scope("default", None),
            },
        )
        .await
        .unwrap();
    let execution = service.execution_for(&job.result_ref).await.unwrap();

    assert!(matches!(
        execution,
        media_contract::ExecutionSelectionDto::Prowlarr {
            media_kind: MediaKindDto::Series,
            season: Some(2),
            episode: Some(7),
            library_title: Some(library_title),
            ..
        } if library_title == "Example Show"
    ));
}

#[tokio::test]
async fn search_session_rejects_the_same_owner_from_another_chat_or_thread() {
    let mut start = request(ProviderDto::Prowlarr);
    start.scope = telegram_scope("chat-a", Some("thread-a"));
    let service = service(HashMap::from([(
        ProviderDto::Prowlarr,
        vec![ProviderPage {
            results: (0..12).map(prowlarr_result).collect(),
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
        translations: vec![
            RezkaTranslationDto {
                id: 37,
                name: "Original".to_owned(),
                premium: false,
                director: false,
                camrip: false,
                has_ads: false,
                seasons: vec![SeasonAvailabilityDto {
                    season: 1,
                    episodes: vec![1, 2],
                }],
            },
            RezkaTranslationDto {
                id: 38,
                name: "Premium Dub".to_owned(),
                premium: true,
                director: false,
                camrip: false,
                has_ads: false,
                seasons: vec![],
            },
        ],
        availability: Some(SeriesAvailabilityDto {
            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
            incomplete: true,
            seasons: vec![
                SeasonAvailabilityDto {
                    season: 1,
                    episodes: vec![1, 2],
                },
                SeasonAvailabilityDto {
                    season: 2,
                    episodes: vec![],
                },
            ],
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
        SelectResultRequest {
            session_id: page.session_id.clone(),
            result_id: "rezka-show".to_owned(),
            translation_id: Some(38),
            season: Some(1),
            episode: Some(1),
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

    let whole_season = service
        .select(
            PRIMARY_USER_ID,
            OperationKey::from_bytes([11; 32]),
            SelectResultRequest {
                session_id: page.session_id.clone(),
                result_id: "rezka-show".to_owned(),
                translation_id: Some(37),
                season: Some(1),
                episode: None,
                scope: telegram_scope("default", None),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        service
            .execution_for(&whole_season.result_ref)
            .await
            .unwrap(),
        media_contract::ExecutionSelectionDto::Rezka {
            translation_id: 37,
            season: Some(1),
            episode: None,
            episodes,
            ..
        } if episodes == vec![
            media_contract::EpisodeSnapshotDto { season: 1, episode: 1 },
            media_contract::EpisodeSnapshotDto { season: 1, episode: 2 },
        ]
    ));

    for (operation, season, episode) in [
        ([12; 32], Some(2), None),
        ([13; 32], Some(3), None),
        ([14; 32], None, Some(1)),
    ] {
        assert_eq!(
            service
                .select(
                    PRIMARY_USER_ID,
                    OperationKey::from_bytes(operation),
                    SelectResultRequest {
                        session_id: page.session_id.clone(),
                        result_id: "rezka-show".to_owned(),
                        translation_id: Some(37),
                        season,
                        episode,
                        scope: telegram_scope("default", None),
                    },
                )
                .await
                .unwrap_err(),
            SearchError::InvalidRequest
        );
    }

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
