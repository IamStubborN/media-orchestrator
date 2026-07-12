use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use media_core::{
    PRIMARY_USER_ID, EpisodeDiscoveryPort, EpisodeSnapshot, NewTrackingCommand,
    NewTrackingSubscription, NotificationDelivery, NotificationDeliveryFailure,
    NotificationDispatcher, NotificationEventType, NotificationId, NotificationOutboxPort,
    NotificationRecipient, NotificationSink, OperationKey, PortError, Provider, TrackingId,
    TrackingRuntime, TrackingScheduleStore, TrackingScope, TrackingSubscription,
};

struct ScheduleStore {
    due: TrackingSubscription,
    discovered: Mutex<Vec<EpisodeSnapshot>>,
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

    async fn record_future_episode(
        &self,
        _: TrackingId,
        episode: EpisodeSnapshot,
        _: time::OffsetDateTime,
    ) -> Result<bool, PortError> {
        self.discovered.lock().unwrap().push(episode);
        Ok(true)
    }

    async fn defer_check(&self, _: TrackingId, _: time::OffsetDateTime) -> Result<(), PortError> {
        Ok(())
    }
}

struct Discovery;

#[async_trait::async_trait]
impl EpisodeDiscoveryPort for Discovery {
    async fn available_episodes(
        &self,
        _: &TrackingSubscription,
    ) -> Result<Vec<EpisodeSnapshot>, PortError> {
        Ok(vec![
            EpisodeSnapshot::new(1, 4).unwrap(),
            EpisodeSnapshot::new(1, 5).unwrap(),
        ])
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
            known_episodes: vec![EpisodeSnapshot::new(1, 4).unwrap()],
            scope: TrackingScope::Personal,
            series_ongoing: true,
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
fn scheduler_records_only_future_episodes_and_never_creates_jobs() {
    block_on(async {
        let store = Arc::new(ScheduleStore {
            due: tracking(),
            discovered: Mutex::new(Vec::new()),
        });
        let runtime = TrackingRuntime::new(store.clone(), Arc::new(Discovery));

        let result = runtime
            .run_once(time::OffsetDateTime::now_utc(), 10)
            .await
            .unwrap();

        assert_eq!(result.checked, 1);
        assert_eq!(result.discovered, 1);
        assert_eq!(
            *store.discovered.lock().unwrap(),
            vec![EpisodeSnapshot::new(1, 5).unwrap()]
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

    async fn mark_delivered(&self, id: NotificationId, _: NotificationId) -> Result<(), PortError> {
        self.delivered.lock().unwrap().push(id);
        Ok(())
    }

    async fn mark_failed(
        &self,
        id: NotificationId,
        _: NotificationId,
        _: time::OffsetDateTime,
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
        "Media job started.".to_owned(),
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
