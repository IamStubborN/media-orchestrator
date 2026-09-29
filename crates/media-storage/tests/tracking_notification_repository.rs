mod support;

use media_core::{
    EpisodeSnapshot, NewTrackingCommand, NewTrackingSubscription, NotificationContent,
    NotificationEventType, NotificationId, NotificationRecipient, OperationKey, PRIMARY_USER_ID,
    Provider, ReleaseIdentity, ReleaseSource, SECONDARY_USER_ID, SourceChoiceAction,
    TrackingCheckStatus, TrackingClaimToken, TrackingDownload, TrackingDownloadPatch, TrackingId,
    TrackingScheduleStore, TrackingScope, TrackingStore,
};
use media_storage::{SeaOrmNotificationOutbox, SeaOrmTrackingStore};
use sea_orm::ConnectionTrait;
use support::{TestDatabase, execute, operation_key, query};

fn all_source_actions() -> Vec<SourceChoiceAction> {
    vec![
        SourceChoiceAction::All,
        SourceChoiceAction::Rezka,
        SourceChoiceAction::Prowlarr,
    ]
}

async fn claim_tracking(store: &SeaOrmTrackingStore, id: TrackingId) -> TrackingClaimToken {
    let now = time::OffsetDateTime::now_utc();
    let token = TrackingClaimToken::new();
    let claimed = store
        .claim_due(now, token, now + time::Duration::minutes(15), 100)
        .await
        .unwrap();
    assert!(claimed.iter().any(|tracking| tracking.id() == id));
    token
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
            poster_url: None,
            release_identity: None,
            download: None,
        },
    )
    .unwrap()
}

fn new_tracking_with_release_identity(id: TrackingId) -> NewTrackingSubscription {
    NewTrackingSubscription::new(
        id,
        PRIMARY_USER_ID,
        NewTrackingCommand {
            provider: Provider::Rezka,
            title: "Lucky".to_owned(),
            translation: "release-calendar".to_owned(),
            known_episodes: vec![EpisodeSnapshot::new(1, 4).unwrap()],
            scope: TrackingScope::Personal,
            series_ongoing: true,
            poster_url: None,
            release_identity: Some(ReleaseIdentity::new(ReleaseSource::Tvmaze, 77).unwrap()),
            download: None,
        },
    )
    .unwrap()
}

fn new_tracking_with_poster(id: TrackingId) -> NewTrackingSubscription {
    NewTrackingSubscription::new(
        id,
        PRIMARY_USER_ID,
        NewTrackingCommand {
            provider: Provider::Rezka,
            title: "Poster Show".to_owned(),
            translation: "release-calendar".to_owned(),
            known_episodes: vec![EpisodeSnapshot::new(1, 1).unwrap()],
            scope: TrackingScope::Personal,
            series_ongoing: true,
            poster_url: Some("https://image.tmdb.org/t/p/w780/show.jpg".to_owned()),
            release_identity: Some(ReleaseIdentity::new(ReleaseSource::Tvmaze, 88).unwrap()),
            download: None,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn tracking_poster_round_trips_through_repository() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let created = store
        .add(operation_key(), new_tracking_with_poster(TrackingId::new()))
        .await
        .unwrap();

    assert_eq!(
        created.poster_url(),
        Some("https://image.tmdb.org/t/p/w780/show.jpg")
    );
    let listed = store.list_visible(PRIMARY_USER_ID).await.unwrap();
    assert_eq!(listed[0].poster_url(), created.poster_url());
}

#[tokio::test]
async fn tracking_release_identity_round_trips_through_repository() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let created = store
        .add(
            operation_key(),
            new_tracking_with_release_identity(TrackingId::new()),
        )
        .await
        .unwrap();

    assert_eq!(created.release_identity().unwrap().source_id(), 77);
    let listed = store.list_visible(PRIMARY_USER_ID).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].release_identity(), created.release_identity());
}

#[tokio::test]
async fn create_with_same_tvmaze_id_returns_existing_notify_track() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let first = store
        .add(
            operation_key(),
            new_tracking_with_release_identity(TrackingId::new()),
        )
        .await
        .unwrap();
    let duplicate = NewTrackingSubscription::new(
        TrackingId::new(),
        PRIMARY_USER_ID,
        NewTrackingCommand {
            provider: Provider::Rezka,
            title: "Lucky RU".to_owned(),
            translation: "release-calendar".to_owned(),
            known_episodes: vec![EpisodeSnapshot::new(1, 4).unwrap()],
            scope: TrackingScope::Personal,
            series_ongoing: true,
            poster_url: None,
            release_identity: Some(ReleaseIdentity::new(ReleaseSource::Tvmaze, 77).unwrap()),
            download: None,
        },
    )
    .unwrap();
    let second = store.add(operation_key(), duplicate).await.unwrap();
    assert_eq!(first.id(), second.id());
    assert_eq!(store.list_visible(PRIMARY_USER_ID).await.unwrap().len(), 1);
}

#[tokio::test]
async fn due_tracking_is_claimed_atomically_with_a_failure_cooldown() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let first = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    let second = store
        .add(
            operation_key(),
            new_tracking_with_release_identity(TrackingId::new()),
        )
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let claim_until = now + time::Duration::minutes(15);

    let first_claim = store
        .claim_due(now, TrackingClaimToken::new(), claim_until, 1)
        .await
        .unwrap();
    let second_claim = store
        .claim_due(now, TrackingClaimToken::new(), claim_until, 1)
        .await
        .unwrap();
    let exhausted = store
        .claim_due(now, TrackingClaimToken::new(), claim_until, 1)
        .await
        .unwrap();

    assert_eq!(first_claim.len(), 1);
    assert_eq!(second_claim.len(), 1);
    assert_ne!(first_claim[0].id(), second_claim[0].id());
    assert!(exhausted.is_empty());
    assert!(first_claim[0].next_check_at() <= now);
    assert!(second_claim[0].next_check_at() <= now);
    let claimed_ids = [first_claim[0].id(), second_claim[0].id()];
    assert!(claimed_ids.contains(&first.id()));
    assert!(claimed_ids.contains(&second.id()));

    let retry = store
        .claim_due(
            claim_until,
            TrackingClaimToken::new(),
            claim_until + time::Duration::minutes(15),
            2,
        )
        .await
        .unwrap();
    assert_eq!(retry.len(), 2);
}

#[tokio::test]
async fn manual_check_during_an_active_claim_is_deferred_until_finish() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let token = TrackingClaimToken::new();
    let claimed = store
        .claim_due(now, token, now + time::Duration::minutes(15), 1)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);

    store
        .request_check_visible(tracking.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .claim_due(
                now + time::Duration::seconds(1),
                TrackingClaimToken::new(),
                now + time::Duration::minutes(16),
                1,
            )
            .await
            .unwrap()
            .is_empty()
    );

    store
        .finish_check(
            tracking.id(),
            token,
            now + time::Duration::hours(1),
            TrackingCheckStatus::NoNewEpisode,
        )
        .await
        .unwrap();
    let recheck = store
        .claim_due(
            now + time::Duration::seconds(2),
            TrackingClaimToken::new(),
            now + time::Duration::minutes(17),
            1,
        )
        .await
        .unwrap();
    assert_eq!(recheck.len(), 1);
    assert_eq!(recheck[0].id(), tracking.id());
}

#[tokio::test]
async fn manual_check_survives_schedule_write_and_repeated_worker_crashes() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let claim_until = now + time::Duration::minutes(15);
    let token = TrackingClaimToken::new();
    store.claim_due(now, token, claim_until, 1).await.unwrap();
    store
        .request_check_visible(tracking.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    store
        .record_future_episode(
            tracking.id(),
            token,
            EpisodeSnapshot::new(1, 5).unwrap(),
            now + time::Duration::hours(1),
            all_source_actions(),
            None,
        )
        .await
        .unwrap();

    assert!(
        store
            .claim_due(
                claim_until - time::Duration::seconds(1),
                TrackingClaimToken::new(),
                claim_until + time::Duration::minutes(15),
                1,
            )
            .await
            .unwrap()
            .is_empty()
    );
    let replacement_token = TrackingClaimToken::new();
    let reclaimed = store
        .claim_due(
            claim_until,
            replacement_token,
            claim_until + time::Duration::minutes(15),
            1,
        )
        .await
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id(), tracking.id());
    let claim = query(
        test_db.connection(),
        "SELECT check_claim_token, check_requested_at FROM tracking_subscriptions",
    )
    .await;
    assert_eq!(
        claim[0]
            .try_get::<uuid::Uuid>("", "check_claim_token")
            .unwrap(),
        replacement_token.into_uuid()
    );
    assert_eq!(
        claim[0]
            .try_get::<Option<time::OffsetDateTime>>("", "check_requested_at")
            .unwrap(),
        None
    );

    let reclaimed_after_second_crash = store
        .claim_due(
            claim_until + time::Duration::minutes(15),
            TrackingClaimToken::new(),
            claim_until + time::Duration::minutes(30),
            1,
        )
        .await
        .unwrap();
    assert_eq!(reclaimed_after_second_crash.len(), 1);
    assert_eq!(reclaimed_after_second_crash[0].id(), tracking.id());
}

#[tokio::test]
async fn stale_finish_cannot_overwrite_a_reclaimed_subscription() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let first_token = TrackingClaimToken::new();
    let first_until = now + time::Duration::minutes(15);
    store
        .claim_due(now, first_token, first_until, 1)
        .await
        .unwrap();
    let second_token = TrackingClaimToken::new();
    store
        .claim_due(
            first_until,
            second_token,
            first_until + time::Duration::minutes(15),
            1,
        )
        .await
        .unwrap();

    assert!(matches!(
        store
            .finish_check(
                tracking.id(),
                first_token,
                now + time::Duration::hours(8),
                TrackingCheckStatus::ReleaseError,
            )
            .await,
        Err(media_core::PortError::Conflict)
    ));
    let claim = query(
        test_db.connection(),
        "SELECT check_claim_token, check_status FROM tracking_subscriptions",
    )
    .await;
    assert_eq!(
        claim[0]
            .try_get::<uuid::Uuid>("", "check_claim_token")
            .unwrap(),
        second_token.into_uuid()
    );
    assert_eq!(
        claim[0].try_get::<String>("", "check_status").unwrap(),
        "never"
    );
}

#[tokio::test]
async fn baseline_and_download_patches_during_claim_preserve_an_immediate_recheck() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let baseline_token = TrackingClaimToken::new();
    let baseline_until = now + time::Duration::minutes(15);
    store
        .claim_due(now, baseline_token, baseline_until, 1)
        .await
        .unwrap();
    store
        .set_baseline_visible(
            tracking.id(),
            PRIMARY_USER_ID,
            EpisodeSnapshot::new(1, 6).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .finish_check(
                tracking.id(),
                baseline_token,
                now + time::Duration::hours(1),
                TrackingCheckStatus::NoNewEpisode,
            )
            .await,
        Err(media_core::PortError::Conflict)
    ));
    assert!(
        store
            .claim_due(
                baseline_until - time::Duration::seconds(1),
                TrackingClaimToken::new(),
                baseline_until + time::Duration::minutes(15),
                1,
            )
            .await
            .unwrap()
            .is_empty()
    );

    let download_token = TrackingClaimToken::new();
    let download_until = baseline_until + time::Duration::minutes(15);
    let claimed = store
        .claim_due(baseline_until, download_token, download_until, 1)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    store
        .patch_download_visible(
            tracking.id(),
            PRIMARY_USER_ID,
            TrackingDownloadPatch::new(
                "Studio Dub".to_owned(),
                TrackingDownload::new("42".to_owned(), 7, 1).unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .finish_check(
                tracking.id(),
                download_token,
                now + time::Duration::minutes(15),
                TrackingCheckStatus::NoNewEpisode,
            )
            .await,
        Err(media_core::PortError::Conflict)
    ));
    assert!(
        store
            .claim_due(
                download_until - time::Duration::seconds(1),
                TrackingClaimToken::new(),
                download_until + time::Duration::minutes(15),
                1,
            )
            .await
            .unwrap()
            .is_empty()
    );

    let final_claim = store
        .claim_due(
            download_until,
            TrackingClaimToken::new(),
            download_until + time::Duration::minutes(15),
            1,
        )
        .await
        .unwrap();
    assert_eq!(final_claim.len(), 1);
    assert_eq!(final_claim[0].id(), tracking.id());
}

#[tokio::test]
async fn expired_claim_cannot_write_or_reserve_without_being_reclaimed() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    store
        .patch_download_visible(
            tracking.id(),
            PRIMARY_USER_ID,
            TrackingDownloadPatch::new(
                "Studio Dub".to_owned(),
                TrackingDownload::new("42".to_owned(), 7, 1).unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let token = claim_tracking(&store, tracking.id()).await;
    execute(
        test_db.connection(),
        "UPDATE tracking_subscriptions SET check_claim_until = now() - interval '1 second'",
    )
    .await
    .unwrap();
    let episode = EpisodeSnapshot::new(1, 5).unwrap();

    assert!(
        store
            .set_release_metadata_if_missing(
                tracking.id(),
                token,
                ReleaseIdentity::new(ReleaseSource::Tvmaze, 99).unwrap(),
                "https://static.tvmaze.com/expired.jpg".to_owned(),
            )
            .await
            .is_err()
    );
    assert!(store.pending_episodes(tracking.id(), token).await.is_err());
    assert!(
        store
            .record_pending_episode(tracking.id(), token, episode)
            .await
            .is_err()
    );
    assert!(
        store
            .release_episode_download(tracking.id(), token, episode)
            .await
            .is_err()
    );
    assert!(
        store
            .reserve_episode_download(tracking.id(), token, episode)
            .await
            .is_err()
    );
    assert!(
        store
            .record_future_episode(
                tracking.id(),
                token,
                episode,
                time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                all_source_actions(),
                None,
            )
            .await
            .is_err()
    );
    assert!(
        store
            .finish_check(
                tracking.id(),
                token,
                time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                TrackingCheckStatus::NoNewEpisode,
            )
            .await
            .is_err()
    );
    let mutations = query(
        test_db.connection(),
        "SELECT
           (SELECT count(*) FROM tracking_download_reservations) AS reservations,
           (SELECT count(*) FROM tracking_discoveries) AS discoveries,
           (SELECT count(*) FROM tracking_availability_candidates) AS candidates",
    )
    .await;
    assert_eq!(mutations[0].try_get::<i64>("", "reservations").unwrap(), 0);
    assert_eq!(mutations[0].try_get::<i64>("", "discoveries").unwrap(), 0);
    assert_eq!(mutations[0].try_get::<i64>("", "candidates").unwrap(), 0);
}

#[tokio::test]
async fn download_reservation_is_fenced_and_keeps_the_authorized_configuration() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    store
        .patch_download_visible(
            tracking.id(),
            PRIMARY_USER_ID,
            TrackingDownloadPatch::new(
                "Old Dub".to_owned(),
                TrackingDownload::new("42".to_owned(), 7, 1).unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let token = claim_tracking(&store, tracking.id()).await;
    let episode = EpisodeSnapshot::new(1, 5).unwrap();
    store
        .reserve_episode_download(tracking.id(), token, episode)
        .await
        .unwrap();
    store
        .patch_download_visible(
            tracking.id(),
            PRIMARY_USER_ID,
            TrackingDownloadPatch::new(
                "New Dub".to_owned(),
                TrackingDownload::new("84".to_owned(), 9, 1).unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();

    assert!(
        store
            .reserve_episode_download(tracking.id(), token, episode)
            .await
            .is_err()
    );
    let reservation = query(
        test_db.connection(),
        "SELECT provider_media_ref, translation_id, download_season
         FROM tracking_download_reservations",
    )
    .await;
    assert_eq!(
        reservation[0]
            .try_get::<String>("", "provider_media_ref")
            .unwrap(),
        "42"
    );
    assert_eq!(
        reservation[0].try_get::<i64>("", "translation_id").unwrap(),
        7
    );
    assert_eq!(
        reservation[0]
            .try_get::<i32>("", "download_season")
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn failed_download_reservation_can_be_reclaimed_with_a_new_configuration() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    store
        .patch_download_visible(
            tracking.id(),
            PRIMARY_USER_ID,
            TrackingDownloadPatch::new(
                "Old Dub".to_owned(),
                TrackingDownload::new("42".to_owned(), 7, 1).unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let first_token = claim_tracking(&store, tracking.id()).await;
    let episode = EpisodeSnapshot::new(1, 5).unwrap();
    store
        .reserve_episode_download(tracking.id(), first_token, episode)
        .await
        .unwrap();

    store
        .patch_download_visible(
            tracking.id(),
            PRIMARY_USER_ID,
            TrackingDownloadPatch::new(
                "New Dub".to_owned(),
                TrackingDownload::new("84".to_owned(), 9, 1).unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .reserve_episode_download(tracking.id(), first_token, episode)
            .await
            .is_err()
    );
    execute(
        test_db.connection(),
        "UPDATE tracking_subscriptions SET check_claim_until = now() - interval '1 second'",
    )
    .await
    .unwrap();
    let second_token = claim_tracking(&store, tracking.id()).await;
    store
        .reserve_episode_download(tracking.id(), second_token, episode)
        .await
        .unwrap();

    let reservation = query(
        test_db.connection(),
        "SELECT provider_media_ref, translation_id FROM tracking_download_reservations",
    )
    .await;
    assert_eq!(
        reservation[0]
            .try_get::<String>("", "provider_media_ref")
            .unwrap(),
        "84"
    );
    assert_eq!(
        reservation[0].try_get::<i64>("", "translation_id").unwrap(),
        9
    );
}

#[tokio::test]
async fn resolved_release_metadata_backfill_is_atomic_and_identity_safe() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let created = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    let resolved = ReleaseIdentity::new(ReleaseSource::Tvmaze, 81228).unwrap();
    let claim_token = claim_tracking(&store, created.id()).await;

    assert!(matches!(
        store
            .set_release_metadata_if_missing(
                created.id(),
                claim_token,
                resolved,
                "http://invalid.test/poster.jpg".to_owned(),
            )
            .await,
        Err(media_core::PortError::Conflict)
    ));
    let listed = store.list_visible(PRIMARY_USER_ID).await.unwrap();
    assert_eq!(listed[0].release_identity(), None);
    assert_eq!(listed[0].poster_url(), None);

    store
        .set_release_metadata_if_missing(
            created.id(),
            claim_token,
            resolved,
            "https://static.tvmaze.com/lucky.jpg".to_owned(),
        )
        .await
        .unwrap();

    let listed = store.list_visible(PRIMARY_USER_ID).await.unwrap();
    assert_eq!(listed[0].release_identity(), Some(resolved));
    assert_eq!(
        listed[0].poster_url(),
        Some("https://static.tvmaze.com/lucky.jpg")
    );

    store
        .set_release_metadata_if_missing(
            created.id(),
            claim_token,
            ReleaseIdentity::new(ReleaseSource::Tvmaze, 99999).unwrap(),
            "https://static.tvmaze.com/wrong.jpg".to_owned(),
        )
        .await
        .unwrap();
    let listed = store.list_visible(PRIMARY_USER_ID).await.unwrap();
    assert_eq!(listed[0].release_identity(), Some(resolved));
    assert_eq!(
        listed[0].poster_url(),
        Some("https://static.tvmaze.com/lucky.jpg")
    );
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
                        "origin":"tracked-episode",
                        "poster_url":"https://image.tmdb.org/t/p/w780/blades.jpg"
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
    assert_eq!(
        notification.media().poster_url(),
        Some("https://image.tmdb.org/t/p/w780/blades.jpg")
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
    let claim_token = claim_tracking(&store, tracking.id()).await;

    assert!(
        store
            .record_future_episode(
                tracking.id(),
                claim_token,
                episode,
                next_check,
                all_source_actions(),
                Some("https://static.tvmaze.com/poster.jpg".to_owned()),
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .record_future_episode(
                tracking.id(),
                claim_token,
                episode,
                next_check,
                all_source_actions(),
                Some("https://static.tvmaze.com/poster.jpg".to_owned()),
            )
            .await
            .unwrap()
    );

    let refreshed = store.list_visible(PRIMARY_USER_ID).await.unwrap();
    assert_eq!(
        refreshed[0].known_episodes(),
        &[EpisodeSnapshot::new(1, 4).unwrap(), episode]
    );
    assert_eq!(
        refreshed[0].poster_url(),
        Some("https://static.tvmaze.com/poster.jpg")
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
        let payload_keys = payload.as_object().unwrap();
        for key in [
            "actions",
            "card_key",
            "episode",
            "event_type",
            "poster_url",
            "schema_version",
            "season",
            "title",
            "tracking_id",
            "choice_set_id",
            "choice_set_expires_at",
            "rezka_count",
            "prowlarr_count",
        ] {
            assert!(payload_keys.contains_key(key), "missing payload key: {key}");
        }
        assert_eq!(payload["event_type"], "media.source-choice");
        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["tracking_id"], tracking.id().to_string());
        assert_eq!(payload["title"], "Ongoing Show");
        assert_eq!(payload["season"], 1);
        assert_eq!(payload["episode"], 5);
        assert!(payload["choice_set_id"].as_str().is_some());
        assert!(payload["choice_set_expires_at"].as_str().is_some());
        assert_eq!(payload["rezka_count"], 0);
        assert_eq!(payload["prowlarr_count"], 0);
        assert_eq!(
            payload["poster_url"],
            "https://static.tvmaze.com/poster.jpg"
        );
        assert_eq!(
            payload["actions"],
            serde_json::json!(["all", "rezka", "prowlarr"])
        );
    }
    assert!(
        query(test_db.connection(), "SELECT id FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn baseline_update_replaces_one_season_and_schedules_an_immediate_check() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmTrackingStore::new(test_db.connection().clone());
    let tracking = store
        .add(
            operation_key(),
            new_tracking(TrackingId::new(), TrackingScope::Personal),
        )
        .await
        .unwrap();
    test_db
        .connection()
        .execute_unprepared(&format!(
            "INSERT INTO tracking_availability_candidates (tracking_id, season, episode)
             VALUES ('{}', 2, 5), ('{}', 2, 7)",
            tracking.id(),
            tracking.id()
        ))
        .await
        .unwrap();

    let updated = store
        .set_baseline_visible(
            tracking.id(),
            PRIMARY_USER_ID,
            EpisodeSnapshot::new(2, 6).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        updated.known_episodes(),
        &[
            EpisodeSnapshot::new(1, 4).unwrap(),
            EpisodeSnapshot::new(2, 1).unwrap(),
            EpisodeSnapshot::new(2, 2).unwrap(),
            EpisodeSnapshot::new(2, 3).unwrap(),
            EpisodeSnapshot::new(2, 4).unwrap(),
            EpisodeSnapshot::new(2, 5).unwrap(),
            EpisodeSnapshot::new(2, 6).unwrap(),
        ]
    );
    let candidates = query(
        test_db.connection(),
        "SELECT episode FROM tracking_availability_candidates ORDER BY episode",
    )
    .await;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].try_get::<i32>("", "episode").unwrap(), 7);

    let checked = store
        .request_check_visible(tracking.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert!(checked.next_check_at() <= time::OffsetDateTime::now_utc());
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
    let claim_token = claim_tracking(&store, tracking.id()).await;

    assert!(
        store
            .record_future_episode(
                tracking.id(),
                claim_token,
                EpisodeSnapshot::new(1, 5).unwrap(),
                time::OffsetDateTime::now_utc() + time::Duration::hours(6),
                vec![SourceChoiceAction::Rezka],
                None,
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
    let claim_token = claim_tracking(&tracking, value.id()).await;
    tracking
        .record_future_episode(
            value.id(),
            claim_token,
            EpisodeSnapshot::new(1, 5).unwrap(),
            time::OffsetDateTime::now_utc() + time::Duration::hours(6),
            all_source_actions(),
            None,
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
    let claim_token = claim_tracking(&tracking, value.id()).await;
    tracking
        .record_future_episode(
            value.id(),
            claim_token,
            EpisodeSnapshot::new(1, 5).unwrap(),
            time::OffsetDateTime::now_utc() + time::Duration::hours(6),
            all_source_actions(),
            None,
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
async fn delivery_permit_rejects_expired_lease_and_only_allows_current_owner() {
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
    let claim_token = claim_tracking(&tracking, value.id()).await;
    tracking
        .record_future_episode(
            value.id(),
            claim_token,
            EpisodeSnapshot::new(1, 5).unwrap(),
            time::OffsetDateTime::now_utc() + time::Duration::hours(6),
            all_source_actions(),
            None,
        )
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let worker = NotificationId::new();
    let delivery = outbox
        .lease_pending(worker, now, time::Duration::seconds(30), 1)
        .await
        .unwrap()
        .remove(0);
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE notification_outbox SET lease_expires_at = now() - interval '1 second'",
        )
        .await
        .unwrap();

    assert!(
        outbox
            .acquire_delivery_permit(delivery.id(), worker, delivery.generation())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        outbox
            .acquire_delivery_permit(delivery.id(), NotificationId::new(), delivery.generation(),)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn live_delivery_permit_blocks_takeover_until_http_and_ack_fence_is_released() {
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
    let claim_token = claim_tracking(&tracking, value.id()).await;
    tracking
        .record_future_episode(
            value.id(),
            claim_token,
            EpisodeSnapshot::new(1, 5).unwrap(),
            time::OffsetDateTime::now_utc() + time::Duration::hours(6),
            all_source_actions(),
            None,
        )
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let worker_a = NotificationId::new();
    let delivery = outbox
        .lease_pending(worker_a, now, time::Duration::seconds(30), 1)
        .await
        .unwrap()
        .remove(0);
    let permit = outbox
        .acquire_delivery_permit(delivery.id(), worker_a, delivery.generation())
        .await
        .unwrap()
        .expect("the exact live owner must acquire a permit");
    let worker_b = NotificationId::new();

    assert!(
        outbox
            .lease_pending(
                worker_b,
                now + time::Duration::minutes(1),
                time::Duration::seconds(30),
                1,
            )
            .await
            .unwrap()
            .is_empty(),
        "takeover must skip an aggregate whose external side effect fence is held"
    );
    drop(permit);

    let takeover = outbox
        .lease_pending(
            worker_b,
            now + time::Duration::minutes(1),
            time::Duration::seconds(30),
            1,
        )
        .await
        .unwrap()
        .remove(0);
    assert!(
        outbox
            .acquire_delivery_permit(takeover.id(), worker_a, takeover.generation())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        outbox
            .acquire_delivery_permit(takeover.id(), worker_b, takeover.generation())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn removing_tracking_revokes_queued_and_already_leased_source_choice() {
    for lease_before_remove in [false, true] {
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
        let claim_token = claim_tracking(&tracking, value.id()).await;
        tracking
            .record_future_episode(
                value.id(),
                claim_token,
                EpisodeSnapshot::new(1, 5).unwrap(),
                time::OffsetDateTime::now_utc() + time::Duration::hours(6),
                all_source_actions(),
                None,
            )
            .await
            .unwrap();
        let worker = NotificationId::new();
        let leased = if lease_before_remove {
            Some(
                outbox
                    .lease_pending(
                        worker,
                        time::OffsetDateTime::now_utc(),
                        time::Duration::seconds(30),
                        1,
                    )
                    .await
                    .unwrap()
                    .remove(0),
            )
        } else {
            None
        };

        tracking
            .remove_visible(operation_key(), value.id(), PRIMARY_USER_ID)
            .await
            .unwrap()
            .expect("the owner must be allowed to remove tracking");

        assert!(
            outbox
                .lease_pending(
                    NotificationId::new(),
                    time::OffsetDateTime::now_utc() + time::Duration::minutes(1),
                    time::Duration::seconds(30),
                    10,
                )
                .await
                .unwrap()
                .is_empty()
        );
        if let Some(leased) = leased {
            assert!(
                outbox
                    .acquire_delivery_permit(leased.id(), worker, leased.generation())
                    .await
                    .unwrap()
                    .is_none(),
                "a leased stale source choice must be fenced before the sink side effect"
            );
        }
    }
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
    let claim_token = claim_tracking(&tracking, value.id()).await;
    tracking
        .record_future_episode(
            value.id(),
            claim_token,
            EpisodeSnapshot::new(1, 5).unwrap(),
            time::OffsetDateTime::now_utc() + time::Duration::hours(6),
            all_source_actions(),
            None,
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
    let claim_token = claim_tracking(&tracking, value.id()).await;
    tracking
        .record_future_episode(
            value.id(),
            claim_token,
            EpisodeSnapshot::new(1, 5).unwrap(),
            time::OffsetDateTime::now_utc() + time::Duration::hours(6),
            all_source_actions(),
            None,
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
