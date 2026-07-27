mod support;

use media_core::{
    PRIMARY_USER_ID, EpisodeSnapshot, NewTrackingCommand, NewTrackingSubscription,
    NotificationContent, NotificationEventType, NotificationId, NotificationRecipient,
    OperationKey, Provider, SourceChoiceAction, TrackingId, TrackingScope, TrackingStore,
    SECONDARY_USER_ID,
};
use media_storage::{SeaOrmNotificationOutbox, SeaOrmTrackingStore};
use sea_orm::ConnectionTrait;
use support::{TestDatabase, operation_key, query};

fn all_source_actions() -> Vec<SourceChoiceAction> {
    vec![
        SourceChoiceAction::All,
        SourceChoiceAction::Rezka,
        SourceChoiceAction::Prowlarr,
    ]
}

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
async fn cancelled_structured_notification_can_be_leased() {
    let test_db = TestDatabase::start_migrated().await;
    test_db
        .connection()
        .execute_unprepared(
            r#"
            INSERT INTO notification_outbox (
                id, aggregate_type, aggregate_id, event_type, recipient,
                source_dedupe_key, payload
            ) VALUES (
                '00000000-0000-4000-8000-000000000101',
                'job',
                '00000000-0000-4000-8000-000000000102',
                'cancelled',
                'primary',
                decode('01', 'hex'),
                '{
                    "event_type":"media.notification",
                    "schema_version":2,
                    "delivery_kind":"card",
                    "card_key":"media-job:00000000-0000-4000-8000-000000000102",
                    "revision":1,
                    "lifecycle_cycle":1,
                    "terminal":true,
                    "state":"cancelled",
                    "media":{
                        "job_id":"00000000-0000-4000-8000-000000000102",
                        "title":"Cancelled Show",
                        "kind":"series",
                        "provider":"rezka",
                        "season":1
                    },
                    "progress":{"completed_episodes":0,"total_episodes":1},
                    "next_step":"none",
                    "actions":["details"]
                }'::jsonb
            )
            "#,
        )
        .await
        .unwrap();
    let outbox = SeaOrmNotificationOutbox::new(test_db.connection().clone());

    let deliveries = outbox
        .lease_pending(
            NotificationId::new(),
            time::OffsetDateTime::now_utc(),
            time::Duration::seconds(30),
            10,
        )
        .await
        .unwrap();

    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].event_type(), NotificationEventType::Cancelled);
    let NotificationContent::Media(notification) = deliveries[0].content() else {
        panic!("expected a structured media notification");
    };
    assert!(notification.terminal());
}

#[tokio::test]
async fn detailed_notification_payload_can_be_leased_without_losing_result_state() {
    let test_db = TestDatabase::start_migrated().await;
    test_db
        .connection()
        .execute_unprepared(
            r#"
            INSERT INTO notification_outbox (
                id, aggregate_type, aggregate_id, event_type, recipient,
                source_dedupe_key, payload
            ) VALUES (
                '00000000-0000-4000-8000-000000000111',
                'job',
                '00000000-0000-4000-8000-000000000112',
                'completed',
                'primary',
                decode('11', 'hex'),
                '{
                    "event_type":"media.notification",
                    "schema_version":2,
                    "delivery_kind":"card",
                    "card_key":"media-job:00000000-0000-4000-8000-000000000112",
                    "revision":8,
                    "lifecycle_cycle":1,
                    "terminal":true,
                    "state":"completed",
                    "media":{
                        "job_id":"00000000-0000-4000-8000-000000000112",
                        "title":"Клинки Хранителей",
                        "kind":"series",
                        "provider":"rezka",
                        "season":2,
                        "translation":"AniLibria",
                        "origin":"tracked-episode"
                    },
                    "progress":{
                        "completed_episodes":1,
                        "total_episodes":1,
                        "current_episode":8,
                        "connection_attempt":5,
                        "connection_attempt_limit":20,
                        "vpn_rotation_pending":false,
                        "storage_available_bytes":53687091200,
                        "storage_required_bytes":1073741824
                    },
                    "result":{
                        "video":{
                            "codec":"hevc",
                            "profile":"Main",
                            "width":1920,
                            "height":1080
                        },
                        "audio":{
                            "language":"rus",
                            "codec":"aac",
                            "channels":2,
                            "channel_layout":"stereo",
                            "title":"AniLibria"
                        },
                        "subtitles":{"downloaded":2,"missing":0},
                        "file_size_bytes":440401920,
                        "duration_seconds":1421,
                        "processing":{"mode":"vaapi-upscale","elapsed_seconds":252},
                        "publication":{
                            "library":"tv-shows",
                            "title":"Клинки Хранителей",
                            "season":2,
                            "episode":8
                        }
                    },
                    "actions":["search-alternative","details"]
                }'::jsonb
            )
            "#,
        )
        .await
        .unwrap();
    let deliveries = SeaOrmNotificationOutbox::new(test_db.connection().clone())
        .lease_pending(
            NotificationId::new(),
            time::OffsetDateTime::now_utc(),
            time::Duration::seconds(30),
            10,
        )
        .await
        .unwrap();

    assert_eq!(deliveries.len(), 1);
    let NotificationContent::Media(notification) = deliveries[0].content() else {
        panic!("expected a structured media notification");
    };
    assert_eq!(
        notification.media().origin(),
        Some(media_core::MediaNotificationOrigin::TrackedEpisode)
    );
    let progress = notification.progress().unwrap();
    assert_eq!(progress.current_episode(), Some(8));
    assert_eq!(progress.connection_attempt(), Some(5));
    assert_eq!(progress.connection_attempt_limit(), Some(20));
    assert_eq!(progress.vpn_rotation_pending(), Some(false));
    assert_eq!(progress.storage_available_bytes(), Some(53_687_091_200));
    assert_eq!(progress.storage_required_bytes(), Some(1_073_741_824));
    let result = notification.result().unwrap();
    assert_eq!(result.video().unwrap().width(), 1920);
    assert_eq!(result.audio().unwrap().language(), Some("rus"));
    assert_eq!(result.subtitles().unwrap().downloaded(), 2);
    assert_eq!(result.file_size_bytes(), Some(440_401_920));
    assert_eq!(
        result.processing().unwrap().mode(),
        media_core::MediaNotificationProcessingMode::VaapiUpscale
    );
    assert_eq!(result.publication().unwrap().episode(), Some(8));
    assert_eq!(
        notification.actions(),
        &[
            media_core::MediaNotificationAction::SearchAlternative,
            media_core::MediaNotificationAction::Details
        ]
    );
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
            .record_future_episode(tracking.id(), episode, next_check, all_source_actions())
            .await
            .unwrap()
    );
    assert!(
        !store
            .record_future_episode(tracking.id(), episode, next_check, all_source_actions())
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
            vec![
                "actions",
                "card_key",
                "episode",
                "event_type",
                "schema_version",
                "season",
                "title",
                "tracking_id",
            ]
        );
        assert_eq!(payload["event_type"], "media.source-choice");
        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["tracking_id"], tracking.id().to_string());
        assert_eq!(payload["title"], "Ongoing Show");
        assert_eq!(payload["season"], 1);
        assert_eq!(payload["episode"], 5);
        assert_eq!(
            payload["actions"],
            serde_json::json!(["all", "rezka", "prowlarr"])
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
async fn future_discovery_persists_only_the_confirmed_source_action() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();

    assert!(
        store
            .record_future_episode(
                tracking.id(),
                EpisodeSnapshot::new(1, 5).unwrap(),
                time::OffsetDateTime::now_utc() + time::Duration::hours(6),
                vec![SourceChoiceAction::Rezka],
            )
            .await
            .unwrap()
    );

    let payload = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(payload["actions"], serde_json::json!(["rezka"]));
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
            all_source_actions(),
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
    assert_eq!(
        leased[0].status_key(),
        Some(format!("tracking:{}:1:5", value.id()).as_str())
    );
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
            all_source_actions(),
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
            all_source_actions(),
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
            all_source_actions(),
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
