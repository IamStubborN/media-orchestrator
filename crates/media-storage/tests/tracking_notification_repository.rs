mod support;

use media_core::{
    PRIMARY_USER_ID, EpisodeSnapshot, NewTrackingCommand, NewTrackingSubscription,
    NotificationEventType, NotificationRecipient, OperationKey, Provider, TrackingId,
    TrackingScope, TrackingStore, SECONDARY_USER_ID,
};
use media_storage::{SeaOrmNotificationOutbox, SeaOrmTrackingStore};
use support::{TestDatabase, operation_key, query};

fn new_tracking(id: TrackingId, scope: TrackingScope) -> NewTrackingSubscription {
    NewTrackingSubscription::new(
        id,
        PRIMARY_USER_ID,
        NewTrackingCommand {
            provider: Provider::Rezka,
            title: "Ongoing Show".to_owned(),
            translation: "Studio Dub".to_owned(),
            known_episodes: vec![EpisodeSnapshot::new(1, 4).unwrap()],
            scope,
            series_ongoing: true,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn add_list_and_remove_are_idempotent_and_apply_scope_visibility() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let operation = operation_key();
    let personal = store
        .add(
            operation,
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    let replay = store
        .add(
            operation,
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    let family = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Family),
        )
        .await
        .unwrap();

    assert_eq!(replay.id(), personal.id());
    assert_eq!(store.list_visible(PRIMARY_USER_ID).await.unwrap().len(), 2);
    assert_eq!(
        store.list_visible(SECONDARY_USER_ID).await.unwrap(),
        vec![family.clone()]
    );

    let remove = operation_key();
    assert_eq!(
        store
            .remove_visible(remove, family.id(), SECONDARY_USER_ID)
            .await
            .unwrap(),
        Some(family.clone())
    );
    assert_eq!(
        store
            .remove_visible(remove, family.id(), SECONDARY_USER_ID)
            .await
            .unwrap(),
        Some(family)
    );
    assert!(
        store
            .list_visible(PRIMARY_USER_ID)
            .await
            .unwrap()
            .iter()
            .all(|item| item.id() != personal.id() || item.scope() == TrackingScope::Personal)
    );
}

#[tokio::test]
async fn future_discovery_updates_snapshot_and_atomically_fans_out_family_notification() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Family),
        )
        .await
        .unwrap();
    let episode = EpisodeSnapshot::new(1, 5).unwrap();
    let next_check = time::OffsetDateTime::now_utc() + time::Duration::hours(6);

    assert!(
        store
            .record_future_episode(tracking.id(), episode, next_check)
            .await
            .unwrap()
    );
    assert!(
        !store
            .record_future_episode(tracking.id(), episode, next_check)
            .await
            .unwrap()
    );

    let refreshed = store.list_visible(PRIMARY_USER_ID).await.unwrap();
    assert_eq!(
        refreshed[0].known_episodes(),
        &[EpisodeSnapshot::new(1, 4).unwrap(), episode]
    );
    let notifications = query(
        test_db.connection(),
        "SELECT recipient, event_type, payload FROM notification_outbox ORDER BY recipient",
    )
    .await;
    assert_eq!(notifications.len(), 2);
    assert_eq!(
        notifications
            .iter()
            .map(|row| row.try_get::<String>("", "recipient").unwrap())
            .collect::<Vec<_>>(),
        vec!["primary", "secondary"]
    );
    for row in notifications {
        assert_eq!(
            row.try_get::<String>("", "event_type").unwrap(),
            "future-episode-found"
        );
        let payload = row.try_get::<serde_json::Value>("", "payload").unwrap();
        assert_eq!(
            payload.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["message"]
        );
        assert!(!payload.to_string().contains("http"));
    }
    assert!(
        query(test_db.connection(), "SELECT id FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn outbox_leases_once_retries_with_backoff_and_keeps_stable_delivery_id() {
    let test_db = TestDatabase::start_migrated().await;
    let tracking = SeaOrmTrackingStore::new(test_db.connection().clone());
    let outbox = SeaOrmNotificationOutbox::new(test_db.connection().clone());
    let value = tracking
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    tracking
        .record_future_episode(
            value.id(),
            EpisodeSnapshot::new(1, 5).unwrap(),
            time::OffsetDateTime::now_utc() + time::Duration::hours(6),
        )
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let worker = uuid::Uuid::new_v4();
    let leased = outbox
        .lease_pending(worker, now, time::Duration::seconds(30), 10)
        .await
        .unwrap();

    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].recipient(), NotificationRecipient::Primary);
    assert_eq!(
        leased[0].event_type(),
        NotificationEventType::FutureEpisodeFound
    );
    assert!(
        outbox
            .lease_pending(uuid::Uuid::new_v4(), now, time::Duration::seconds(30), 10)
            .await
            .unwrap()
            .is_empty()
    );

    let delivery_id = leased[0].id();
    outbox
        .mark_failed(delivery_id, worker, now, "http_502")
        .await
        .unwrap();
    assert!(
        outbox
            .lease_pending(uuid::Uuid::new_v4(), now, time::Duration::seconds(30), 10)
            .await
            .unwrap()
            .is_empty()
    );
    let retried = outbox
        .lease_pending(
            worker,
            now + time::Duration::minutes(1),
            time::Duration::seconds(30),
            10,
        )
        .await
        .unwrap();
    assert_eq!(retried[0].id(), delivery_id);
    assert_eq!(retried[0].attempt_count(), 1);
    outbox.mark_delivered(delivery_id, worker).await.unwrap();
    assert!(
        outbox
            .lease_pending(
                worker,
                now + time::Duration::hours(1),
                time::Duration::seconds(30),
                10,
            )
            .await
            .unwrap()
            .is_empty()
    );
}

#[allow(dead_code)]
fn operation_key_from_bytes(bytes: [u8; 32]) -> OperationKey {
    OperationKey::from_bytes(bytes)
}
