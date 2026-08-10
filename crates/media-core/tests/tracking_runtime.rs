use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use media_core::{
    PRIMARY_USER_ID, EpisodeAvailability, EpisodeAvailabilityPort, EpisodeAvailabilityRequest,
    EpisodeDiscovery, EpisodeDiscoveryPort, EpisodeSnapshot, NewTrackingCommand,
    NewTrackingSubscription, NotificationDelivery, NotificationDeliveryFailure,
    NotificationDispatcher, NotificationEventType, NotificationId, NotificationOutboxPort,
    NotificationRecipient, NotificationSink, OperationKey, PortError, Provider,
    ProviderAvailability, ReleaseIdentity, ReleaseSource, SourceChoiceAction,
    TrackedEpisodeDownloadPort, TrackingDownload, TrackingId, TrackingRuntime,
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
}

#[async_trait::async_trait]
impl TrackingScheduleStore for ScheduleStore {
    async fn list_due(
        &self,
        _: time::OffsetDateTime,
        _: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        Ok(vec![self.due.clone()])
    }

    async fn set_release_metadata_if_missing(
        &self,
        _: TrackingId,
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
        _: TrackingId,
        episode: EpisodeSnapshot,
        _: time::OffsetDateTime,
        actions: Vec<SourceChoiceAction>,
        _: Option<String>,
    ) -> Result<bool, PortError> {
        self.pending
            .lock()
            .unwrap()
            .retain(|candidate| *candidate != episode);
        self.discovered.lock().unwrap().push((episode, actions));
        Ok(true)
    }

    async fn pending_episodes(&self, _: TrackingId) -> Result<Vec<EpisodeSnapshot>, PortError> {
        Ok(self.pending.lock().unwrap().clone())
    }

    async fn record_pending_episode(
        &self,
        _: TrackingId,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        let mut pending = self.pending.lock().unwrap();
        if !pending.contains(&episode) {
            pending.push(episode);
        }
        Ok(())
    }

    async fn finish_check(
        &self,
        _: TrackingId,
        next_check_at: time::OffsetDateTime,
        status: media_core::TrackingCheckStatus,
    ) -> Result<(), PortError> {
        self.finished.lock().unwrap().push((next_check_at, status));
        Ok(())
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

struct Availability;

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
        });
        let runtime = TrackingRuntime::new(
            store.clone(),
            Arc::new(MetadataDiscovery { resolved: true }),
        )
        .with_availability(Arc::new(Availability));

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
        });
        let runtime = TrackingRuntime::new(
            store.clone(),
            Arc::new(MetadataDiscovery { resolved: false }),
        )
        .with_availability(Arc::new(Availability));

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
    async fn list_due(
        &self,
        _: time::OffsetDateTime,
        _: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        Ok(self.due.clone())
    }

    async fn set_release_metadata_if_missing(
        &self,
        id: TrackingId,
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
        _: EpisodeSnapshot,
        _: time::OffsetDateTime,
        _: Vec<SourceChoiceAction>,
        _: Option<String>,
    ) -> Result<bool, PortError> {
        Ok(false)
    }

    async fn pending_episodes(&self, _: TrackingId) -> Result<Vec<EpisodeSnapshot>, PortError> {
        Ok(Vec::new())
    }

    async fn record_pending_episode(
        &self,
        _: TrackingId,
        _: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Ok(())
    }

    async fn finish_check(
        &self,
        id: TrackingId,
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
                now + time::Duration::hours(1),
                media_core::TrackingCheckStatus::EpisodeFound,
            )]
        );
    });
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

#[test]
fn calendar_candidate_stays_unrecorded_until_a_provider_confirms_it() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            release_metadata: Mutex::new(Vec::new()),
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
                release_identity: None,
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
                release_identity: None,
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
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(Discovery))
            .with_availability(Arc::new(Availability));

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
                release_identity: None,
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

#[async_trait::async_trait]
impl TrackedEpisodeDownloadPort for Enqueuer {
    async fn enqueue_episode(
        &self,
        _: &TrackingSubscription,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
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
        });
        let downloads = Arc::new(Enqueuer::default());
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(DownloadDiscovery))
            .with_downloads(downloads.clone());

        let result = runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

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
    });
}

struct Outbox {
    delivery: NotificationDelivery,
    delivered: Mutex<Vec<NotificationId>>,
    failed: Mutex<Vec<NotificationId>>,
    dead: Mutex<Vec<NotificationId>>,
}

impl Outbox {
    fn new(delivery: NotificationDelivery) -> Self {
        Self {
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
    async fn deliver(&self, _: &NotificationDelivery) -> Result<(), NotificationDeliveryFailure> {
        self.outcome
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
