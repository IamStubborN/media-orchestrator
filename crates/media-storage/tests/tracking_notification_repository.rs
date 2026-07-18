mod support;

use media_core::{
    PRIMARY_USER_ID, EpisodeSnapshot, NewTrackingCommand, NewTrackingSubscription,
    NotificationEventType, NotificationId, NotificationRecipient, OperationKey, Provider,
    TrackingId, TrackingScope, TrackingStore, SECONDARY_USER_ID,
};
use media_storage::{SeaOrmNotificationOutbox, SeaOrmTrackingStore};
use sea_orm::ConnectionTrait;
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
            download: None,
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
        let message = payload["message"].as_str().unwrap();
        assert!(message.starts_with("📺 **Новая серия доступна**"));
        assert!(message.contains("🔔 S01E05"));
        assert!(message.contains("➡️ **Дальше:** выберите источник"));
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
    let worker = NotificationId::new();
    let leased = outbox
        .lease_pending(worker, now, time::Duration::seconds(30), 10)
        .await
        .unwrap();

    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].recipient(), NotificationRecipient::Primary);
    assert_eq!(leased[0].status_key(), None);
    assert_eq!(
        leased[0].event_type(),
        NotificationEventType::FutureEpisodeFound
    );
    assert!(
        outbox
            .lease_pending(NotificationId::new(), now, time::Duration::seconds(30), 10)
            .await
            .unwrap()
            .is_empty()
    );

    let delivery_id = leased[0].id();
    let generation = leased[0].generation();
    outbox
        .mark_failed(delivery_id, worker, now, generation, "http_502")
        .await
        .unwrap();
    assert!(
        outbox
            .lease_pending(NotificationId::new(), now, time::Duration::seconds(30), 10)
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
    outbox
        .mark_delivered(delivery_id, worker, retried[0].generation())
        .await
        .unwrap();
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

#[tokio::test]
async fn stale_delivery_ack_releases_the_lease_without_consuming_a_new_generation() {
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

    let delivered_worker = NotificationId::new();
    let first = outbox
        .lease_pending(delivered_worker, now, time::Duration::seconds(30), 1)
        .await
        .unwrap()
        .remove(0);
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE notification_outbox SET generation = generation + 1, payload = '{\"message\":\"generation 2\"}'::jsonb",
        )
        .await
        .unwrap();
    outbox
        .mark_delivered(first.id(), delivered_worker, first.generation())
        .await
        .unwrap();

    let failed_worker = NotificationId::new();
    let second = outbox
        .lease_pending(
            failed_worker,
            now + time::Duration::minutes(1),
            time::Duration::seconds(30),
            1,
        )
        .await
        .unwrap()
        .remove(0);
    assert_eq!(second.generation(), 2);
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE notification_outbox SET generation = generation + 1, payload = '{\"message\":\"generation 3\"}'::jsonb",
        )
        .await
        .unwrap();
    outbox
        .mark_failed(
            second.id(),
            failed_worker,
            now + time::Duration::minutes(1),
            second.generation(),
            "stale_failure",
        )
        .await
        .unwrap();

    let dead_worker = NotificationId::new();
    let third = outbox
        .lease_pending(
            dead_worker,
            now + time::Duration::minutes(2),
            time::Duration::seconds(30),
            1,
        )
        .await
        .unwrap()
        .remove(0);
    assert_eq!(third.generation(), 3);
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE notification_outbox SET generation = generation + 1, payload = '{\"message\":\"generation 4\"}'::jsonb",
        )
        .await
        .unwrap();
    outbox
        .mark_dead(
            third.id(),
            dead_worker,
            now + time::Duration::minutes(2),
            third.generation(),
            "stale_dead_letter",
        )
        .await
        .unwrap();

    let latest = outbox
        .lease_pending(
            NotificationId::new(),
            now + time::Duration::minutes(3),
            time::Duration::seconds(30),
            1,
        )
        .await
        .unwrap()
        .remove(0);
    assert_eq!(latest.id(), first.id());
    assert_eq!(latest.generation(), 4);
    assert_eq!(latest.attempt_count(), 0);
    assert_eq!(latest.message(), "generation 4");
    let rows = query(
        test_db.connection(),
        "SELECT delivered_at, dead_at, last_error_code FROM notification_outbox",
    )
    .await;
    assert!(
        rows[0]
            .try_get::<Option<time::OffsetDateTime>>("", "delivered_at")
            .unwrap()
            .is_none()
    );
    assert!(
        rows[0]
            .try_get::<Option<time::OffsetDateTime>>("", "dead_at")
            .unwrap()
            .is_none()
    );
    assert!(
        rows[0]
            .try_get::<Option<String>>("", "last_error_code")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn mark_failed_does_not_overflow_backoff_at_high_attempt_counts() {
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
    let worker = NotificationId::new();
    let leased = outbox
        .lease_pending(worker, now, time::Duration::seconds(30), 10)
        .await
        .unwrap();
    let delivery_id = leased[0].id();
    let generation = leased[0].generation();

    test_db
        .connection()
        .execute_unprepared("UPDATE notification_outbox SET attempt_count = 40")
        .await
        .unwrap();

    // With attempt_count this high, `30 * power(2, attempt_count)` overflowed int4
    // before the exponent was clamped, so mark_failed used to error here.
    outbox
        .mark_failed(delivery_id, worker, now, generation, "webhook_http")
        .await
        .unwrap();

    let rows = query(
        test_db.connection(),
        "SELECT attempt_count FROM notification_outbox",
    )
    .await;
    assert_eq!(rows[0].try_get::<i32>("", "attempt_count").unwrap(), 41);
}

#[tokio::test]
async fn mark_dead_buries_a_delivery_so_it_is_never_leased_again() {
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
    let worker = NotificationId::new();
    let leased = outbox
        .lease_pending(worker, now, time::Duration::seconds(30), 10)
        .await
        .unwrap();
    let delivery_id = leased[0].id();
    let generation = leased[0].generation();

    outbox
        .mark_dead(delivery_id, worker, now, generation, "webhook_rejected")
        .await
        .unwrap();

    assert!(
        outbox
            .lease_pending(
                NotificationId::new(),
                now + time::Duration::hours(6),
                time::Duration::seconds(30),
                10,
            )
            .await
            .unwrap()
            .is_empty()
    );
    let rows = query(
        test_db.connection(),
        "SELECT last_error_code FROM notification_outbox WHERE dead_at IS NOT NULL",
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].try_get::<String>("", "last_error_code").unwrap(),
        "webhook_rejected"
    );
}

#[allow(dead_code)]
fn operation_key_from_bytes(bytes: [u8; 32]) -> OperationKey {
    OperationKey::from_bytes(bytes)
}
