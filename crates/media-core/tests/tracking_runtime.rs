use std::{
    collections::BTreeMap,
    future::Future,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

use media_core::{
    AnonymousSessionPort, EpisodeAvailability, EpisodeAvailabilityPort, EpisodeAvailabilityRequest,
    EpisodeDiscovery, EpisodeDiscoveryPort, EpisodeSnapshot, FutureEpisodeRecord,
    NewTrackingCommand, NewTrackingSubscription, NotificationDelivery, NotificationDeliveryFailure,
    NotificationDeliveryFence, NotificationDeliveryPermit, NotificationDispatcher,
    NotificationEventType, NotificationId, NotificationOutboxPort, NotificationRecipient,
    NotificationSink, NotificationSinkOutcome, OperationKey, PRIMARY_USER_ID, PortError, Provider,
    ProviderAvailability, ReleaseIdentity, ReleaseSource, SourceChoiceAction,
    TrackedEpisodeDownloadPort, TrackingClaimToken, TrackingDownload, TrackingId, TrackingRuntime,
    TrackingScheduleStore, TrackingScope, TrackingSubscription,
};

#[test]
fn cancelled_notification_event_round_trips_from_wire() {
    assert_eq!(
        NotificationEventType::from_wire("cancelled"),
        Some(NotificationEventType::Cancelled)
    );
}

struct ScheduleStore {
    due: TrackingSubscription,
    discovered: Mutex<Vec<(EpisodeSnapshot, Vec<SourceChoiceAction>)>>,
    pending: Mutex<Vec<EpisodeSnapshot>>,
    finished: Mutex<Vec<(time::OffsetDateTime, media_core::TrackingCheckStatus)>>,
    release_metadata: Mutex<Vec<(media_core::ReleaseIdentity, String)>>,
    season_complete: Mutex<Vec<(EpisodeSnapshot, bool)>>,
}

#[async_trait::async_trait]
impl TrackingScheduleStore for ScheduleStore {
    async fn claim_due(
        &self,
        _: time::OffsetDateTime,
        _: TrackingClaimToken,
        _: time::OffsetDateTime,
        _: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        Ok(vec![self.due.clone()])
    }

    async fn set_release_metadata_if_missing(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        release_identity: media_core::ReleaseIdentity,
        poster_url: String,
    ) -> Result<(), PortError> {
        self.release_metadata
            .lock()
            .unwrap()
            .push((release_identity, poster_url));
        Ok(())
    }

    async fn record_future_episode(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
        next_check_at: time::OffsetDateTime,
        actions: Vec<SourceChoiceAction>,
        poster_url: Option<String>,
    ) -> Result<bool, PortError> {
        self.record_future_episode_with_counts(FutureEpisodeRecord {
            id,
            claim_token,
            episode,
            next_check_at,
            actions,
            poster_url,
            rezka_count: 0,
            prowlarr_count: 0,
            season_complete: false,
        })
        .await
    }

    async fn record_future_episode_with_counts(
        &self,
        record: FutureEpisodeRecord,
    ) -> Result<bool, PortError> {
        self.pending
            .lock()
            .unwrap()
            .retain(|candidate| *candidate != record.episode);
        self.discovered
            .lock()
            .unwrap()
            .push((record.episode, record.actions.clone()));
        self.season_complete
            .lock()
            .unwrap()
            .push((record.episode, record.season_complete));
        Ok(true)
    }

    async fn pending_episodes(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
    ) -> Result<Vec<EpisodeSnapshot>, PortError> {
        Ok(self.pending.lock().unwrap().clone())
    }

    async fn record_pending_episode(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        let mut pending = self.pending.lock().unwrap();
        if !pending.contains(&episode) {
            pending.push(episode);
        }
        Ok(())
    }

    async fn reserve_episode_download(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn finish_check(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        next_check_at: time::OffsetDateTime,
        status: media_core::TrackingCheckStatus,
    ) -> Result<(), PortError> {
        self.finished.lock().unwrap().push((next_check_at, status));
        Ok(())
    }
}

struct Availability;

#[async_trait::async_trait]
impl EpisodeAvailabilityPort for Availability {
    async fn probe(
        &self,
        _: EpisodeAvailabilityRequest<'_>,
    ) -> Result<EpisodeAvailability, PortError> {
        Ok(EpisodeAvailability::new(
            ProviderAvailability::Available,
            ProviderAvailability::Unavailable,
        ))
    }
}

struct UnavailableAvailability;

#[async_trait::async_trait]
impl EpisodeAvailabilityPort for UnavailableAvailability {
    async fn probe(
        &self,
        _: EpisodeAvailabilityRequest<'_>,
    ) -> Result<EpisodeAvailability, PortError> {
        Ok(EpisodeAvailability::new(
            ProviderAvailability::Unavailable,
            ProviderAvailability::Unknown,
        ))
    }
}

struct Discovery;

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for Discovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        EpisodeDiscovery::new(
            vec![
                EpisodeSnapshot::new(1, 1).unwrap(),
                EpisodeSnapshot::new(1, 2).unwrap(),
                EpisodeSnapshot::new(1, 3).unwrap(),
                EpisodeSnapshot::new(1, 4).unwrap(),
                EpisodeSnapshot::new(1, 5).unwrap(),
            ],
            "Ongoing Show".to_owned(),
            Some("Original Show".to_owned()),
        )
        .map_err(|_| PortError::Conflict)
    }
}

struct MetadataDiscovery {
    resolved: bool,
}

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for MetadataDiscovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        let discovery = EpisodeDiscovery::new(
            (1..=4)
                .map(|episode| EpisodeSnapshot::new(1, episode).unwrap())
                .collect(),
            "Ongoing Show".to_owned(),
            Some("Original Show".to_owned()),
        )
        .map_err(|_| PortError::Conflict)?
        .with_poster_url(Some("https://static.tvmaze.com/poster.jpg".to_owned()));
        Ok(if self.resolved {
            discovery
                .with_release_identity(ReleaseIdentity::new(ReleaseSource::Tvmaze, 77).unwrap())
        } else {
            discovery
        })
    }
}

fn tracking() -> TrackingSubscription {
    NewTrackingSubscription::new(
        TrackingId::new(),
        PRIMARY_USER_ID,
        NewTrackingCommand {
            provider: Provider::Rezka,
            title: "Ongoing Show".to_owned(),
            translation: "Studio Dub".to_owned(),
            known_episodes: (1..=4)
                .map(|episode| EpisodeSnapshot::new(1, episode).unwrap())
                .collect(),
            scope: TrackingScope::Personal,
            series_ongoing: true,
            poster_url: None,
            release_identity: None,
            download: None,
        },
    )
    .unwrap()
    .into_persisted()
}

fn tracking_named(title: &str) -> TrackingSubscription {
    NewTrackingSubscription::new(
        TrackingId::new(),
        PRIMARY_USER_ID,
        NewTrackingCommand {
            provider: Provider::Rezka,
            title: title.to_owned(),
            translation: "Studio Dub".to_owned(),
            known_episodes: vec![EpisodeSnapshot::new(1, 4).unwrap()],
            scope: TrackingScope::Personal,
            series_ongoing: true,
            poster_url: None,
            release_identity: None,
            download: None,
        },
    )
    .unwrap()
    .into_persisted()
}

struct FailingDiscovery;

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for FailingDiscovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        Err(PortError::Infrastructure)
    }
}

struct ConflictingDiscovery;

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for ConflictingDiscovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        Err(PortError::Conflict)
    }
}

#[test]
fn release_failure_cooldown_starts_when_failure_is_observed() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking_named("Failed release"),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let started_at = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
        let failure_observed_at = time::OffsetDateTime::now_utc();
        let result = TrackingRuntime::new(store.clone(), Arc::new(FailingDiscovery))
            .run_once(started_at, 10)
            .await
            .unwrap();

        assert_eq!(result.checked, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(result.release_infrastructure_failures, 1);
        assert_eq!(result.release_conflict_failures, 0);
        let finished = store.finished.lock().unwrap();
        assert_eq!(finished[0].1, media_core::TrackingCheckStatus::ReleaseError);
        assert!(
            finished[0].0 >= failure_observed_at + time::Duration::minutes(15),
            "failure cooldown started before the failure was observed"
        );
    });
}

#[test]
fn release_failure_counters_preserve_the_bounded_port_error_class() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking_named("Failed release"),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let result = TrackingRuntime::new(store, Arc::new(ConflictingDiscovery))
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.failed, 1);
        assert_eq!(result.release_conflict_failures, 1);
        assert_eq!(result.release_infrastructure_failures, 0);
    });
}

fn download_tracking() -> TrackingSubscription {
    NewTrackingSubscription::new(
        TrackingId::new(),
        PRIMARY_USER_ID,
        NewTrackingCommand {
            provider: Provider::Rezka,
            title: "Blades of the Guardians S2".to_owned(),
            translation: "Studio Dub".to_owned(),
            known_episodes: vec![
                EpisodeSnapshot::new(2, 7).unwrap(),
                EpisodeSnapshot::new(3, 1).unwrap(),
            ],
            scope: TrackingScope::Personal,
            series_ongoing: true,
            poster_url: None,
            release_identity: None,
            download: Some(TrackingDownload::new("42".to_owned(), 19, 2).unwrap()),
        },
    )
    .unwrap()
    .into_persisted()
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[test]
fn scheduler_backfills_resolved_release_identity_and_poster_without_a_new_episode() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(
            store.clone(),
            Arc::new(MetadataDiscovery { resolved: true }),
        );

        let result = runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.discovered, 0);
        assert_eq!(
            *store.release_metadata.lock().unwrap(),
            vec![(
                ReleaseIdentity::new(ReleaseSource::Tvmaze, 77).unwrap(),
                "https://static.tvmaze.com/poster.jpg".to_owned(),
            )]
        );
    });
}

#[test]
fn scheduler_does_not_persist_a_poster_without_resolved_release_identity() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(
            store.clone(),
            Arc::new(MetadataDiscovery { resolved: false }),
        );

        runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert!(store.release_metadata.lock().unwrap().is_empty());
    });
}

struct MetadataBatchStore {
    due: Vec<TrackingSubscription>,
    fail_id: TrackingId,
    stored: Mutex<Vec<TrackingId>>,
    finished: Mutex<Vec<(TrackingId, media_core::TrackingCheckStatus)>>,
}

#[async_trait::async_trait]
impl TrackingScheduleStore for MetadataBatchStore {
    async fn claim_due(
        &self,
        _: time::OffsetDateTime,
        _: TrackingClaimToken,
        _: time::OffsetDateTime,
        _: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        Ok(self.due.clone())
    }

    async fn set_release_metadata_if_missing(
        &self,
        id: TrackingId,
        _: TrackingClaimToken,
        _: ReleaseIdentity,
        _: String,
    ) -> Result<(), PortError> {
        if id == self.fail_id {
            return Err(PortError::Infrastructure);
        }
        self.stored.lock().unwrap().push(id);
        Ok(())
    }

    async fn record_future_episode(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
        _: time::OffsetDateTime,
        _: Vec<SourceChoiceAction>,
        _: Option<String>,
    ) -> Result<bool, PortError> {
        Ok(false)
    }

    async fn pending_episodes(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
    ) -> Result<Vec<EpisodeSnapshot>, PortError> {
        Ok(Vec::new())
    }

    async fn record_pending_episode(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn reserve_episode_download(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn finish_check(
        &self,
        id: TrackingId,
        _: TrackingClaimToken,
        _: time::OffsetDateTime,
        status: media_core::TrackingCheckStatus,
    ) -> Result<(), PortError> {
        self.finished.lock().unwrap().push((id, status));
        Ok(())
    }
}

struct MetadataBatchDiscovery;

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for MetadataBatchDiscovery {
    async fn resolved_release_metadata(
        &self,
        tracking: &TrackingSubscription,
    ) -> Result<Option<(ReleaseIdentity, String)>, PortError> {
        let source_id = match tracking.title() {
            "Invalid" => 77,
            "Store failure" => 78,
            _ => 79,
        };
        let poster = if tracking.title() == "Invalid" {
            "http://invalid.test/poster.jpg"
        } else {
            "https://static.tvmaze.com/poster.jpg"
        };
        Ok(Some((
            ReleaseIdentity::new(ReleaseSource::Tvmaze, source_id).unwrap(),
            poster.to_owned(),
        )))
    }

    async fn available_episodes(
        &self,
        tracking: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        EpisodeDiscovery::new(
            tracking.known_episodes().to_vec(),
            tracking.title().to_owned(),
            None,
        )
        .map_err(|_| PortError::Conflict)
    }
}

#[test]
fn optional_metadata_failures_do_not_abort_or_starve_the_tracking_batch() {
    block_on(async {
        let invalid = tracking_named("Invalid");
        let store_failure = tracking_named("Store failure");
        let good = tracking_named("Good");
        let store = Arc::new(MetadataBatchStore {
            due: vec![invalid.clone(), store_failure.clone(), good.clone()],
            fail_id: store_failure.id(),
            stored: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(MetadataBatchDiscovery));

        let result = runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.checked, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(*store.stored.lock().unwrap(), vec![good.id()]);
        assert_eq!(
            *store.finished.lock().unwrap(),
            vec![
                (invalid.id(), media_core::TrackingCheckStatus::NoNewEpisode),
                (
                    store_failure.id(),
                    media_core::TrackingCheckStatus::NoNewEpisode,
                ),
                (good.id(), media_core::TrackingCheckStatus::NoNewEpisode),
            ]
        );
    });
}

struct DiscoveryWriteFailureStore {
    due: Vec<TrackingSubscription>,
    fail_id: TrackingId,
    recorded: Mutex<Vec<TrackingId>>,
    finished: Mutex<
        Vec<(
            TrackingId,
            time::OffsetDateTime,
            media_core::TrackingCheckStatus,
        )>,
    >,
    claims: Mutex<Vec<(time::OffsetDateTime, time::OffsetDateTime)>>,
}

#[async_trait::async_trait]
impl TrackingScheduleStore for DiscoveryWriteFailureStore {
    async fn claim_due(
        &self,
        now: time::OffsetDateTime,
        _: TrackingClaimToken,
        claim_until: time::OffsetDateTime,
        _: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        self.claims.lock().unwrap().push((now, claim_until));
        Ok(self.due.clone())
    }

    async fn set_release_metadata_if_missing(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: ReleaseIdentity,
        _: String,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn record_future_episode(
        &self,
        id: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
        _: time::OffsetDateTime,
        _: Vec<SourceChoiceAction>,
        _: Option<String>,
    ) -> Result<bool, PortError> {
        if id == self.fail_id {
            return Err(PortError::Infrastructure);
        }
        self.recorded.lock().unwrap().push(id);
        Ok(true)
    }

    async fn pending_episodes(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
    ) -> Result<Vec<EpisodeSnapshot>, PortError> {
        Ok(Vec::new())
    }

    async fn record_pending_episode(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn reserve_episode_download(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn finish_check(
        &self,
        id: TrackingId,
        _: TrackingClaimToken,
        next_check_at: time::OffsetDateTime,
        status: media_core::TrackingCheckStatus,
    ) -> Result<(), PortError> {
        self.finished
            .lock()
            .unwrap()
            .push((id, next_check_at, status));
        Ok(())
    }
}

#[test]
fn discovery_write_failure_is_cooled_down_and_does_not_abort_the_batch() {
    block_on(async {
        let failed = tracking_named("Failed write");
        let healthy = tracking_named("Healthy write");
        let store = Arc::new(DiscoveryWriteFailureStore {
            due: vec![failed.clone(), healthy.clone()],
            fail_id: failed.id(),
            recorded: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            claims: Mutex::new(Vec::new()),
        });
        let now = time::OffsetDateTime::now_utc();
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(Discovery))
            .with_availability(Arc::new(Availability));

        let result = runtime.run_once(now, 10).await.unwrap();

        assert_eq!(result.checked, 2);
        assert_eq!(result.discovered, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(*store.recorded.lock().unwrap(), vec![healthy.id()]);
        assert_eq!(
            *store.claims.lock().unwrap(),
            vec![(now, now + time::Duration::minutes(15))]
        );
        let finished = store.finished.lock().unwrap();
        assert_eq!(finished[0].0, failed.id());
        assert_eq!(finished[0].2, media_core::TrackingCheckStatus::SourceError);
        assert!(finished[0].1 >= now + time::Duration::minutes(15));
        assert_eq!(
            finished[1],
            (
                healthy.id(),
                now + time::Duration::hours(3),
                media_core::TrackingCheckStatus::EpisodeFound,
            )
        );
    });
}

#[test]
fn scheduler_records_only_episodes_missing_from_the_known_set() {
    block_on(async {
        let now = time::OffsetDateTime::now_utc();
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(Discovery))
            .with_availability(Arc::new(Availability));

        let result = runtime.run_once(now, 10).await.unwrap();

        assert_eq!(result.checked, 1);
        assert_eq!(result.discovered, 1);
        assert_eq!(
            *store.discovered.lock().unwrap(),
            vec![(
                EpisodeSnapshot::new(1, 5).unwrap(),
                vec![SourceChoiceAction::Rezka]
            )]
        );
        assert_eq!(
            *store.finished.lock().unwrap(),
            vec![(
                now + time::Duration::hours(3),
                media_core::TrackingCheckStatus::EpisodeFound,
            )]
        );
    });
}

#[test]
fn notify_only_records_downloadable_episodes_after_provider_probe() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(Discovery))
            .with_availability(Arc::new(Availability));

        let result = runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.discovered, 1);
        assert_eq!(
            *store.discovered.lock().unwrap(),
            vec![(
                EpisodeSnapshot::new(1, 5).unwrap(),
                vec![SourceChoiceAction::Rezka]
            )]
        );
        assert!(store.pending.lock().unwrap().is_empty());
        assert_eq!(
            *store.season_complete.lock().unwrap(),
            vec![(EpisodeSnapshot::new(1, 5).unwrap(), false)]
        );
    });
}

#[test]
fn calendar_candidate_stays_unrecorded_until_a_provider_confirms_it() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(Discovery))
            .with_availability(Arc::new(UnavailableAvailability));

        let result = runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.discovered, 0);
        assert!(store.discovered.lock().unwrap().is_empty());
        assert_eq!(
            *store.pending.lock().unwrap(),
            vec![EpisodeSnapshot::new(1, 5).unwrap()]
        );
        assert!(store.season_complete.lock().unwrap().is_empty());
    });
}

struct ScheduledFinaleDiscovery {
    last_scheduled: BTreeMap<u32, u32>,
}

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for ScheduledFinaleDiscovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        EpisodeDiscovery::new(
            vec![
                EpisodeSnapshot::new(1, 1).unwrap(),
                EpisodeSnapshot::new(1, 2).unwrap(),
                EpisodeSnapshot::new(1, 3).unwrap(),
                EpisodeSnapshot::new(1, 4).unwrap(),
                EpisodeSnapshot::new(1, 5).unwrap(),
            ],
            "Ongoing Show".to_owned(),
            Some("Original Show".to_owned()),
        )
        .map(|discovery| discovery.with_last_scheduled_by_season(self.last_scheduled.clone()))
        .map_err(|_| PortError::Conflict)
    }
}

#[test]
fn last_scheduled_downloadable_episode_marks_the_season_complete() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(
            store.clone(),
            Arc::new(ScheduledFinaleDiscovery {
                last_scheduled: BTreeMap::from([(1, 5)]),
            }),
        )
        .with_availability(Arc::new(Availability));

        let result = runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.discovered, 1);
        assert_eq!(
            *store.discovered.lock().unwrap(),
            vec![(
                EpisodeSnapshot::new(1, 5).unwrap(),
                vec![SourceChoiceAction::Rezka]
            )]
        );
        assert_eq!(
            *store.season_complete.lock().unwrap(),
            vec![(EpisodeSnapshot::new(1, 5).unwrap(), true)]
        );
    });
}

#[test]
fn mid_season_episode_is_not_season_complete_when_later_calendar_rows_exist() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(
            store.clone(),
            Arc::new(ScheduledFinaleDiscovery {
                last_scheduled: BTreeMap::from([(1, 10)]),
            }),
        )
        .with_availability(Arc::new(Availability));

        runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(
            *store.season_complete.lock().unwrap(),
            vec![(EpisodeSnapshot::new(1, 5).unwrap(), false)]
        );
    });
}

#[test]
fn missing_episode_remains_eligible_after_a_later_episode_is_known() {
    block_on(async {
        let due = NewTrackingSubscription::new(
            TrackingId::new(),
            PRIMARY_USER_ID,
            NewTrackingCommand {
                provider: Provider::Rezka,
                title: "Ongoing Show".to_owned(),
                translation: "release-calendar".to_owned(),
                known_episodes: vec![
                    EpisodeSnapshot::new(1, 1).unwrap(),
                    EpisodeSnapshot::new(1, 2).unwrap(),
                    EpisodeSnapshot::new(1, 4).unwrap(),
                ],
                scope: TrackingScope::Personal,
                series_ongoing: true,
                poster_url: None,
                release_identity: Some(ReleaseIdentity::new(ReleaseSource::Tvmaze, 600).unwrap()),
                download: None,
            },
        )
        .unwrap()
        .into_persisted();
        let store = Arc::new(ScheduleStore {
            due,
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(vec![EpisodeSnapshot::new(1, 3).unwrap()]),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(Discovery))
            .with_availability(Arc::new(Availability));

        runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(
            store
                .discovered
                .lock()
                .unwrap()
                .iter()
                .map(|(episode, _)| *episode)
                .collect::<Vec<_>>(),
            vec![
                EpisodeSnapshot::new(1, 3).unwrap(),
                EpisodeSnapshot::new(1, 5).unwrap()
            ]
        );
    });
}

#[test]
fn scheduler_does_not_backfill_seasons_older_than_the_tracked_season() {
    block_on(async {
        let due = NewTrackingSubscription::new(
            TrackingId::new(),
            PRIMARY_USER_ID,
            NewTrackingCommand {
                provider: Provider::Rezka,
                title: "Long-running Show".to_owned(),
                translation: "release-calendar".to_owned(),
                known_episodes: vec![
                    EpisodeSnapshot::new(1, 1).unwrap(),
                    EpisodeSnapshot::new(9, 9).unwrap(),
                ],
                scope: TrackingScope::Personal,
                series_ongoing: true,
                poster_url: None,
                release_identity: Some(ReleaseIdentity::new(ReleaseSource::Tvmaze, 601).unwrap()),
                download: None,
            },
        )
        .unwrap()
        .into_persisted();
        let store = Arc::new(ScheduleStore {
            due,
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(Discovery));

        runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert!(store.discovered.lock().unwrap().is_empty());
    });
}

struct HistoricalGapDiscovery;

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for HistoricalGapDiscovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        EpisodeDiscovery::new(
            vec![
                EpisodeSnapshot::new(9, 1).unwrap(),
                EpisodeSnapshot::new(9, 4).unwrap(),
                EpisodeSnapshot::new(9, 10).unwrap(),
            ],
            "Long-running Show".to_owned(),
            None,
        )
        .map_err(|_| PortError::Conflict)
    }
}

#[test]
fn scheduler_ignores_historical_gaps_but_rechecks_pending_future_episode() {
    block_on(async {
        let due = NewTrackingSubscription::new(
            TrackingId::new(),
            PRIMARY_USER_ID,
            NewTrackingCommand {
                provider: Provider::Rezka,
                title: "Long-running Show".to_owned(),
                translation: "release-calendar".to_owned(),
                known_episodes: vec![
                    EpisodeSnapshot::new(9, 8).unwrap(),
                    EpisodeSnapshot::new(9, 9).unwrap(),
                ],
                scope: TrackingScope::Personal,
                series_ongoing: true,
                poster_url: None,
                release_identity: Some(ReleaseIdentity::new(ReleaseSource::Tvmaze, 602).unwrap()),
                download: None,
            },
        )
        .unwrap()
        .into_persisted();
        let store = Arc::new(ScheduleStore {
            due,
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(vec![EpisodeSnapshot::new(9, 10).unwrap()]),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(HistoricalGapDiscovery))
            .with_availability(Arc::new(Availability));

        runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(
            store
                .discovered
                .lock()
                .unwrap()
                .iter()
                .map(|(episode, _)| *episode)
                .collect::<Vec<_>>(),
            vec![EpisodeSnapshot::new(9, 10).unwrap()]
        );
    });
}

struct DownloadDiscovery;

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for DownloadDiscovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        EpisodeDiscovery::new(
            vec![
                EpisodeSnapshot::new(2, 7).unwrap(),
                EpisodeSnapshot::new(2, 8).unwrap(),
                EpisodeSnapshot::new(3, 1).unwrap(),
            ],
            "Blades of the Guardians S2".to_owned(),
            None,
        )
        .map_err(|_| PortError::Conflict)
    }
}

#[derive(Default)]
struct Enqueuer {
    episodes: Mutex<Vec<EpisodeSnapshot>>,
}

struct FencedDownloadStore {
    due: TrackingSubscription,
    claim_valid: AtomicBool,
    reservations: Mutex<Vec<EpisodeSnapshot>>,
    released: Mutex<Vec<EpisodeSnapshot>>,
    discoveries: Mutex<Vec<EpisodeSnapshot>>,
}

#[async_trait::async_trait]
impl TrackingScheduleStore for FencedDownloadStore {
    async fn claim_due(
        &self,
        _: time::OffsetDateTime,
        _: TrackingClaimToken,
        _: time::OffsetDateTime,
        _: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        Ok(vec![self.due.clone()])
    }

    async fn set_release_metadata_if_missing(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: ReleaseIdentity,
        _: String,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn record_future_episode(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        episode: EpisodeSnapshot,
        _: time::OffsetDateTime,
        _: Vec<SourceChoiceAction>,
        _: Option<String>,
    ) -> Result<bool, PortError> {
        if !self.claim_valid.load(Ordering::SeqCst) {
            return Err(PortError::Conflict);
        }
        self.discoveries.lock().unwrap().push(episode);
        Ok(true)
    }

    async fn pending_episodes(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
    ) -> Result<Vec<EpisodeSnapshot>, PortError> {
        Ok(Vec::new())
    }

    async fn record_pending_episode(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn reserve_episode_download(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        if !self.claim_valid.load(Ordering::SeqCst) {
            return Err(PortError::Conflict);
        }
        self.reservations.lock().unwrap().push(episode);
        Ok(())
    }

    async fn release_episode_download(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        if !self.claim_valid.load(Ordering::SeqCst) {
            return Err(PortError::Conflict);
        }
        self.released.lock().unwrap().push(episode);
        Ok(())
    }

    async fn finish_check(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: time::OffsetDateTime,
        _: media_core::TrackingCheckStatus,
    ) -> Result<(), PortError> {
        if self.claim_valid.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }
}

struct BlockedDownloadDiscovery {
    reached: Arc<Barrier>,
    resume: Arc<Barrier>,
}

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for BlockedDownloadDiscovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError> {
        self.reached.wait();
        self.resume.wait();
        EpisodeDiscovery::new(
            vec![EpisodeSnapshot::new(2, 8).unwrap()],
            "Blades of the Guardians S2".to_owned(),
            None,
        )
        .map_err(|_| PortError::Conflict)
    }
}

struct BlockedEnqueuer {
    reached: Arc<Barrier>,
    resume: Arc<Barrier>,
    episodes: Mutex<Vec<EpisodeSnapshot>>,
}

#[async_trait::async_trait]
impl TrackedEpisodeDownloadPort for BlockedEnqueuer {
    async fn enqueue_episode(
        &self,
        _: &TrackingSubscription,
        episode: EpisodeSnapshot,
    ) -> Result<(), &'static str> {
        self.reached.wait();
        self.resume.wait();
        self.episodes.lock().unwrap().push(episode);
        Ok(())
    }
}

#[test]
fn config_patch_while_discovery_is_blocked_prevents_download_enqueue() {
    let reached = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let store = Arc::new(FencedDownloadStore {
        due: download_tracking(),
        claim_valid: AtomicBool::new(true),
        reservations: Mutex::new(Vec::new()),
        released: Mutex::new(Vec::new()),
        discoveries: Mutex::new(Vec::new()),
    });
    let downloads = Arc::new(Enqueuer::default());
    let runtime = TrackingRuntime::new(
        store.clone(),
        Arc::new(BlockedDownloadDiscovery {
            reached: reached.clone(),
            resume: resume.clone(),
        }),
    )
    .with_downloads(downloads.clone());
    let worker = std::thread::spawn(move || {
        block_on(runtime.run_once(time::OffsetDateTime::now_utc(), 1)).unwrap()
    });
    reached.wait();
    store.claim_valid.store(false, Ordering::SeqCst);
    resume.wait();
    let result = worker.join().unwrap();

    assert_eq!(result.queued, 0);
    assert!(store.reservations.lock().unwrap().is_empty());
    assert!(downloads.episodes.lock().unwrap().is_empty());
}

#[test]
fn config_patch_after_durable_reservation_does_not_undo_authorized_enqueue() {
    let reached = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let store = Arc::new(FencedDownloadStore {
        due: download_tracking(),
        claim_valid: AtomicBool::new(true),
        reservations: Mutex::new(Vec::new()),
        released: Mutex::new(Vec::new()),
        discoveries: Mutex::new(Vec::new()),
    });
    let downloads = Arc::new(BlockedEnqueuer {
        reached: reached.clone(),
        resume: resume.clone(),
        episodes: Mutex::new(Vec::new()),
    });
    let runtime = TrackingRuntime::new(store.clone(), Arc::new(DownloadDiscovery))
        .with_downloads(downloads.clone());
    let worker = std::thread::spawn(move || {
        block_on(runtime.run_once(time::OffsetDateTime::now_utc(), 1)).unwrap()
    });
    reached.wait();
    assert_eq!(
        *store.reservations.lock().unwrap(),
        vec![EpisodeSnapshot::new(2, 8).unwrap()]
    );
    store.claim_valid.store(false, Ordering::SeqCst);
    resume.wait();
    let result = worker.join().unwrap();

    assert_eq!(result.queued, 1);
    assert_eq!(
        *downloads.episodes.lock().unwrap(),
        vec![EpisodeSnapshot::new(2, 8).unwrap()]
    );
    assert!(store.discoveries.lock().unwrap().is_empty());
}

struct FailingEnqueuer;

#[async_trait::async_trait]
impl TrackedEpisodeDownloadPort for FailingEnqueuer {
    async fn enqueue_episode(
        &self,
        _: &TrackingSubscription,
        _: EpisodeSnapshot,
    ) -> Result<(), &'static str> {
        Err(media_core::ENQUEUE_FAILURE_CODE)
    }
}

#[test]
fn failed_enqueue_releases_its_durable_reservation_for_a_new_configuration() {
    block_on(async {
        let store = Arc::new(FencedDownloadStore {
            due: download_tracking(),
            claim_valid: AtomicBool::new(true),
            reservations: Mutex::new(Vec::new()),
            released: Mutex::new(Vec::new()),
            discoveries: Mutex::new(Vec::new()),
        });
        let result = TrackingRuntime::new(store.clone(), Arc::new(DownloadDiscovery))
            .with_downloads(Arc::new(FailingEnqueuer))
            .run_once(time::OffsetDateTime::now_utc(), 1)
            .await
            .unwrap();

        assert_eq!(result.queued, 0);
        assert_eq!(
            *store.released.lock().unwrap(),
            vec![EpisodeSnapshot::new(2, 8).unwrap()]
        );
    });
}

#[test]
fn repeated_identical_enqueue_failures_increase_cooldown_beyond_fifteen_minutes() {
    block_on(async {
        let due = download_tracking()
            .with_check_diagnostics(Some(media_core::ENQUEUE_FAILURE_CODE.to_owned()), 3);
        let store = Arc::new(ScheduleStore {
            due,
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let started_at = time::OffsetDateTime::now_utc();
        let result = TrackingRuntime::new(store.clone(), Arc::new(DownloadDiscovery))
            .with_downloads(Arc::new(FailingEnqueuer))
            .run_once(started_at, 1)
            .await
            .unwrap();

        assert_eq!(result.queued, 0);
        assert_eq!(result.source_failures, 1);
        let finished = store.finished.lock().unwrap();
        assert_eq!(finished[0].1, media_core::TrackingCheckStatus::SourceError);
        // previous identical failures=3 → next count 4 → 120 minutes
        assert!(
            finished[0].0 >= started_at + time::Duration::minutes(120),
            "expected backoff of at least 120 minutes, got {:?}",
            finished[0].0 - started_at
        );
        assert!(
            finished[0].0 < started_at + time::Duration::minutes(121),
            "cooldown drifted beyond the 120 minute step"
        );
    });
}

#[async_trait::async_trait]
impl TrackedEpisodeDownloadPort for Enqueuer {
    async fn enqueue_episode(
        &self,
        _: &TrackingSubscription,
        episode: EpisodeSnapshot,
    ) -> Result<(), &'static str> {
        self.episodes.lock().unwrap().push(episode);
        Ok(())
    }
}

#[test]
fn scheduler_enqueues_only_new_episodes_from_the_selected_download_season() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: download_tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let downloads = Arc::new(Enqueuer::default());
        let now = time::OffsetDateTime::now_utc();
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(DownloadDiscovery))
            .with_downloads(downloads.clone());

        let result = runtime.run_once(now, 10).await.unwrap();

        assert_eq!(result.queued, 1);
        assert_eq!(result.discovered, 1);
        assert_eq!(
            *downloads.episodes.lock().unwrap(),
            vec![EpisodeSnapshot::new(2, 8).unwrap()]
        );
        assert_eq!(
            *store.discovered.lock().unwrap(),
            vec![(EpisodeSnapshot::new(2, 8).unwrap(), Vec::new())]
        );
        assert_eq!(
            *store.finished.lock().unwrap(),
            vec![(
                now + time::Duration::minutes(30),
                media_core::TrackingCheckStatus::DownloadQueued,
            )]
        );
    });
}

#[derive(Default)]
struct CountingSession {
    warmups: Mutex<u32>,
}

#[async_trait::async_trait]
impl AnonymousSessionPort for CountingSession {
    async fn prepare_anonymous_session(&self) -> Result<(), PortError> {
        *self.warmups.lock().unwrap() += 1;
        Ok(())
    }
}

struct MultiDueStore {
    due: Vec<TrackingSubscription>,
    discovered: Mutex<Vec<EpisodeSnapshot>>,
    finished: Mutex<Vec<TrackingId>>,
}

#[async_trait::async_trait]
impl TrackingScheduleStore for MultiDueStore {
    async fn claim_due(
        &self,
        _: time::OffsetDateTime,
        _: TrackingClaimToken,
        _: time::OffsetDateTime,
        _: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        Ok(self.due.clone())
    }

    async fn set_release_metadata_if_missing(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: ReleaseIdentity,
        _: String,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn record_future_episode(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        episode: EpisodeSnapshot,
        _: time::OffsetDateTime,
        _: Vec<SourceChoiceAction>,
        _: Option<String>,
    ) -> Result<bool, PortError> {
        self.discovered.lock().unwrap().push(episode);
        Ok(true)
    }

    async fn pending_episodes(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
    ) -> Result<Vec<EpisodeSnapshot>, PortError> {
        Ok(Vec::new())
    }

    async fn record_pending_episode(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn reserve_episode_download(
        &self,
        _: TrackingId,
        _: TrackingClaimToken,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn finish_check(
        &self,
        id: TrackingId,
        _: TrackingClaimToken,
        _: time::OffsetDateTime,
        _: media_core::TrackingCheckStatus,
    ) -> Result<(), PortError> {
        self.finished.lock().unwrap().push(id);
        Ok(())
    }
}

#[test]
fn auto_download_pass_warms_the_anonymous_session_once() {
    block_on(async {
        let store = Arc::new(MultiDueStore {
            due: vec![download_tracking(), download_tracking()],
            discovered: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
        });
        let downloads = Arc::new(Enqueuer::default());
        let session = Arc::new(CountingSession::default());
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(DownloadDiscovery))
            .with_downloads(downloads.clone())
            .with_anonymous_session(session.clone());

        let result = runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.queued, 2);
        assert_eq!(*session.warmups.lock().unwrap(), 1);
        assert_eq!(downloads.episodes.lock().unwrap().len(), 2);
    });
}

#[test]
fn notify_only_pass_warms_the_anonymous_session_once() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
            season_complete: Mutex::new(Vec::new()),
        });
        let session = Arc::new(CountingSession::default());
        let runtime = TrackingRuntime::new(store, Arc::new(Discovery))
            .with_availability(Arc::new(Availability))
            .with_anonymous_session(session.clone());

        runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(*session.warmups.lock().unwrap(), 1);
    });
}

struct Outbox {
    delivery: NotificationDelivery,
    current_generation: AtomicU64,
    delivered: Mutex<Vec<NotificationId>>,
    failed: Mutex<Vec<NotificationId>>,
    dead: Mutex<Vec<NotificationId>>,
}

impl Outbox {
    fn new(delivery: NotificationDelivery) -> Self {
        Self {
            current_generation: AtomicU64::new(delivery.generation()),
            delivery,
            delivered: Mutex::new(Vec::new()),
            failed: Mutex::new(Vec::new()),
            dead: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl NotificationOutboxPort for Outbox {
    async fn lease_pending(
        &self,
        _: NotificationId,
        _: time::OffsetDateTime,
        _: time::Duration,
        _: u32,
    ) -> Result<Vec<NotificationDelivery>, PortError> {
        Ok(vec![self.delivery.clone()])
    }

    async fn acquire_delivery_permit(
        &self,
        id: NotificationId,
        _: NotificationId,
        generation: u64,
    ) -> Result<Option<NotificationDeliveryPermit>, PortError> {
        Ok((id == self.delivery.id()
            && generation == self.current_generation.load(Ordering::SeqCst))
        .then(|| NotificationDeliveryPermit::hold(())))
    }

    async fn mark_delivered(
        &self,
        id: NotificationId,
        _: NotificationId,
        _: u64,
    ) -> Result<(), PortError> {
        self.delivered.lock().unwrap().push(id);
        Ok(())
    }

    async fn mark_failed(
        &self,
        id: NotificationId,
        _: NotificationId,
        _: time::OffsetDateTime,
        _: u64,
        _: &str,
    ) -> Result<(), PortError> {
        self.failed.lock().unwrap().push(id);
        Ok(())
    }

    async fn mark_dead(
        &self,
        id: NotificationId,
        _: NotificationId,
        _: time::OffsetDateTime,
        _: u64,
        _: &str,
    ) -> Result<(), PortError> {
        self.dead.lock().unwrap().push(id);
        Ok(())
    }
}

struct Sink {
    outcome: Result<(), NotificationDeliveryFailure>,
}

#[async_trait::async_trait]
impl NotificationSink for Sink {
    async fn deliver(
        &self,
        _: &NotificationDelivery,
        fence: &NotificationDeliveryFence,
    ) -> Result<NotificationSinkOutcome, NotificationDeliveryFailure> {
        let Some(_permit) = fence
            .acquire()
            .await
            .map_err(|_| NotificationDeliveryFailure::retryable("delivery_fence"))?
        else {
            return Ok(NotificationSinkOutcome::Superseded);
        };
        self.outcome
            .map(|()| NotificationSinkOutcome::Delivered(_permit))
    }
}

struct BlockedSink {
    entered: Arc<Barrier>,
    resume: Arc<Barrier>,
    side_effected: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl NotificationSink for BlockedSink {
    async fn deliver(
        &self,
        _: &NotificationDelivery,
        fence: &NotificationDeliveryFence,
    ) -> Result<NotificationSinkOutcome, NotificationDeliveryFailure> {
        self.entered.wait();
        self.resume.wait();
        let Some(_permit) = fence
            .acquire()
            .await
            .map_err(|_| NotificationDeliveryFailure::retryable("delivery_fence"))?
        else {
            return Ok(NotificationSinkOutcome::Superseded);
        };
        self.side_effected.store(true, Ordering::SeqCst);
        Ok(NotificationSinkOutcome::Delivered(_permit))
    }
}

fn started_delivery() -> NotificationDelivery {
    NotificationDelivery::rehydrate(
        NotificationId::new(),
        NotificationRecipient::Primary,
        NotificationEventType::Started,
        None,
        "Media job started.".to_owned(),
        1,
        0,
    )
    .unwrap()
}

#[test]
fn dispatcher_marks_exact_stable_delivery_id_after_sink_success() {
    block_on(async {
        let delivery = started_delivery();
        let outbox = Arc::new(Outbox::new(delivery.clone()));
        let dispatcher =
            NotificationDispatcher::new(outbox.clone(), Arc::new(Sink { outcome: Ok(()) }));

        let result = dispatcher
            .run_once(NotificationId::new(), time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.delivered, 1);
        assert_eq!(*outbox.delivered.lock().unwrap(), vec![delivery.id()]);
        assert!(outbox.failed.lock().unwrap().is_empty());
        assert!(outbox.dead.lock().unwrap().is_empty());
    });
}

#[test]
fn dispatcher_fences_a_blocked_stale_generation_before_the_side_effect() {
    let delivery = started_delivery();
    let outbox = Arc::new(Outbox::new(delivery));
    let entered = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let side_effected = Arc::new(AtomicBool::new(false));
    let dispatcher = NotificationDispatcher::new(
        outbox.clone(),
        Arc::new(BlockedSink {
            entered: entered.clone(),
            resume: resume.clone(),
            side_effected: side_effected.clone(),
        }),
    );
    let task = std::thread::spawn(move || {
        block_on(dispatcher.run_once(NotificationId::new(), time::OffsetDateTime::now_utc(), 10))
            .unwrap()
    });

    entered.wait();
    outbox.current_generation.store(2, Ordering::SeqCst);
    resume.wait();
    let result = task.join().unwrap();

    assert_eq!(result, media_core::NotificationDispatchResult::default());
    assert!(!side_effected.load(Ordering::SeqCst));
    assert!(outbox.delivered.lock().unwrap().is_empty());
}

#[test]
fn dispatcher_retries_retryable_failures_and_buries_terminal_ones() {
    block_on(async {
        let retryable = started_delivery();
        let outbox = Arc::new(Outbox::new(retryable.clone()));
        let dispatcher = NotificationDispatcher::new(
            outbox.clone(),
            Arc::new(Sink {
                outcome: Err(NotificationDeliveryFailure::retryable("webhook_http")),
            }),
        );
        let result = dispatcher
            .run_once(NotificationId::new(), time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();
        assert_eq!(result.failed, 1);
        assert_eq!(result.dead, 0);
        assert_eq!(*outbox.failed.lock().unwrap(), vec![retryable.id()]);
        assert!(outbox.dead.lock().unwrap().is_empty());

        let terminal = started_delivery();
        let outbox = Arc::new(Outbox::new(terminal.clone()));
        let dispatcher = NotificationDispatcher::new(
            outbox.clone(),
            Arc::new(Sink {
                outcome: Err(NotificationDeliveryFailure::terminal("webhook_rejected")),
            }),
        );
        let result = dispatcher
            .run_once(NotificationId::new(), time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();
        assert_eq!(result.failed, 0);
        assert_eq!(result.dead, 1);
        assert_eq!(*outbox.dead.lock().unwrap(), vec![terminal.id()]);
        assert!(outbox.failed.lock().unwrap().is_empty());
    });
}

#[allow(dead_code)]
fn operation_key() -> OperationKey {
    OperationKey::from_bytes([7; 32])
}
