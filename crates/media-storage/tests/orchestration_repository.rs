mod support;

use media_core::{
    PRIMARY_USER_ID, BootstrapClient, CheckpointValue, ClientRole, ClientStore, CredentialDigest,
    JobEvent, JobEventId, JobId, JobState, JobStore, LeaseStore, NewJob, NotificationContent,
    NotificationEventType, NotificationId, NotifyScope, Provider, RUNNER_CLIENT_ID,
    SECONDARY_USER_ID,
};
use media_storage::{
    SeaOrmClientStore, SeaOrmJobStore, SeaOrmLeaseStore, SeaOrmNotificationOutbox,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use support::{TestDatabase, operation_key, query};

async fn setup() -> (TestDatabase, SeaOrmJobStore, SeaOrmLeaseStore) {
    let test_db = TestDatabase::start_migrated().await;
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE runner_lifecycle SET state = 'ready', reason = NULL WHERE singleton = true",
        )
        .await
        .unwrap();
    SeaOrmClientStore::new(test_db.connection().clone())
        .upsert_client(
            BootstrapClient::new(
                RUNNER_CLIENT_ID,
                "download-runner".to_owned(),
                ClientRole::Runner,
                None,
                CredentialDigest::from([0x66; 32]),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let jobs = SeaOrmJobStore::new(test_db.connection().clone());
    let leases = SeaOrmLeaseStore::new(test_db.connection().clone());
    (test_db, jobs, leases)
}

fn new_job(reference: &str) -> NewJob {
    new_job_with_notifications(reference, PRIMARY_USER_ID, NotifyScope::Initiator)
}

fn new_job_with_notifications(
    reference: &str,
    owner: media_core::UserId,
    notify_scope: NotifyScope,
) -> NewJob {
    new_job_for_provider(reference, owner, notify_scope, Provider::Rezka)
}

fn new_job_for_provider(
    reference: &str,
    owner: media_core::UserId,
    notify_scope: NotifyScope,
    provider: Provider,
) -> NewJob {
    NewJob::new(
        JobId::new(),
        owner,
        provider,
        reference.to_owned(),
        notify_scope,
    )
    .unwrap()
}

async fn notification_rows(test_db: &TestDatabase) -> Vec<sea_orm::QueryResult> {
    query(
        test_db.connection(),
        "SELECT event_type, recipient, payload FROM notification_outbox \
         ORDER BY event_type, recipient",
    )
    .await
}

fn structured_payload(row: &sea_orm::QueryResult, forbidden: &[&str]) -> serde_json::Value {
    let payload = row.try_get::<serde_json::Value>("", "payload").unwrap();
    assert_eq!(payload["schema_version"], 2);
    assert_eq!(payload["event_type"], "media.notification");
    assert!(payload.get("message").is_none());
    let serialized = payload.to_string();
    assert!(!serialized.contains("://"));
    for value in forbidden {
        assert!(
            !serialized.contains(value),
            "payload leaked {value:?}: {payload}"
        );
    }
    payload
}

fn detailed_artifact_checkpoint(
    audio_layout: &str,
    file_size_bytes: u64,
    duration_seconds: u64,
    processing_seconds: u64,
) -> media_core::Checkpoint {
    [
        (
            "artifact_video_codec".to_owned(),
            CheckpointValue::String("hevc".to_owned()),
        ),
        (
            "artifact_video_profile".to_owned(),
            CheckpointValue::String("Main".to_owned()),
        ),
        ("artifact_width".to_owned(), CheckpointValue::Unsigned(1920)),
        (
            "artifact_height".to_owned(),
            CheckpointValue::Unsigned(1080),
        ),
        (
            "artifact_audio_language".to_owned(),
            CheckpointValue::String("rus".to_owned()),
        ),
        (
            "artifact_audio_codec".to_owned(),
            CheckpointValue::String("aac".to_owned()),
        ),
        (
            "artifact_audio_channels".to_owned(),
            CheckpointValue::Unsigned(2),
        ),
        (
            "artifact_audio_channel_layout".to_owned(),
            CheckpointValue::String(audio_layout.to_owned()),
        ),
        (
            "artifact_audio_title".to_owned(),
            CheckpointValue::String("AniLibria".to_owned()),
        ),
        (
            "artifact_subtitles_downloaded".to_owned(),
            CheckpointValue::Unsigned(2),
        ),
        (
            "artifact_subtitles_missing".to_owned(),
            CheckpointValue::Unsigned(0),
        ),
        (
            "artifact_file_size_bytes".to_owned(),
            CheckpointValue::Unsigned(file_size_bytes),
        ),
        (
            "artifact_duration_seconds".to_owned(),
            CheckpointValue::Unsigned(duration_seconds),
        ),
        (
            "artifact_processing_mode".to_owned(),
            CheckpointValue::String("vaapi-upscale".to_owned()),
        ),
        (
            "artifact_processing_seconds".to_owned(),
            CheckpointValue::Unsigned(processing_seconds),
        ),
    ]
    .into_iter()
    .collect()
}

#[tokio::test]
async fn create_persists_initial_task_and_transactional_outbox_record() {
    let (test_db, jobs, _) = setup().await;
    let job = jobs
        .create(operation_key(), new_job("durable-create"))
        .await
        .unwrap();

    let tasks = query(
        test_db.connection(),
        "SELECT ordinal, state FROM job_tasks WHERE job_id = \
         (SELECT id FROM jobs WHERE result_ref = 'durable-create')",
    )
    .await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].try_get::<i32>("", "ordinal").unwrap(), 0);
    assert_eq!(tasks[0].try_get::<String>("", "state").unwrap(), "pending");

    let outbox = query(
        test_db.connection(),
        "SELECT aggregate_id, event_type FROM outbox_events",
    )
    .await;
    assert_eq!(outbox.len(), 1);
    assert_eq!(
        outbox[0].try_get::<uuid::Uuid>("", "aggregate_id").unwrap(),
        job.id().into_uuid(),
    );
    assert_eq!(
        outbox[0].try_get::<String>("", "event_type").unwrap(),
        "job.created",
    );
    assert!(
        query(
            test_db.connection(),
            "SELECT id FROM tracking_subscriptions",
        )
        .await
        .is_empty(),
        "creating a download job must never enable tracking",
    );
}

#[tokio::test]
async fn duplicate_event_id_does_not_duplicate_transition_event_or_outbox() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("dedupe-event"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    let event = JobEvent::started(JobEventId::new());

    let first = leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            event.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let duplicate = leases
        .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(first.state(), JobState::Running);
    assert_eq!(duplicate.state(), JobState::Running);
    assert_eq!(
        query(test_db.connection(), "SELECT id FROM job_events")
            .await
            .len(),
        1
    );
    assert_eq!(
        query(
            test_db.connection(),
            "SELECT id FROM outbox_events WHERE event_type = 'job.started'",
        )
        .await
        .len(),
        1,
    );
    let notifications = query(
        test_db.connection(),
        "SELECT event_type, recipient, payload FROM notification_outbox",
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert_eq!(
        notifications[0]
            .try_get::<String>("", "event_type")
            .unwrap(),
        "started"
    );
    assert_eq!(
        notifications[0].try_get::<String>("", "recipient").unwrap(),
        "primary"
    );
    let payload = notifications[0]
        .try_get::<serde_json::Value>("", "payload")
        .unwrap();
    assert_eq!(payload["schema_version"], 2);
    assert_eq!(payload["media"]["provider"], "rezka");
    assert_eq!(payload["delivery_kind"], "card");
    assert!(!payload.to_string().contains("http"));
}

#[tokio::test]
async fn rezka_runner_events_create_each_success_notification_once() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(
        operation_key(),
        new_job("rezka://private-provider-reference"),
    )
    .await
    .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();

    let started = JobEvent::started(JobEventId::new());
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            started.clone(),
        )
        .await
        .unwrap();
    leases
        .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, started)
        .await
        .unwrap();

    for event in [
        JobEvent::stage_started(JobEventId::new(), 0, "resolve_manifest".to_owned(), 0).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "resolve_manifest".to_owned(),
            0,
            Default::default(),
        )
        .unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "media_pipeline".to_owned(), 1).unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 3).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "download".to_owned(),
            3,
            Default::default(),
        )
        .unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "media_pipeline".to_owned(),
            1,
            Default::default(),
        )
        .unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "execution".to_owned(), 2).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "execution".to_owned(),
            2,
            Default::default(),
        )
        .unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Completed, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let notifications = notification_rows(&test_db).await;
    let event_types = notifications
        .iter()
        .map(|row| row.try_get::<String>("", "event_type").unwrap())
        .collect::<Vec<_>>();
    assert_eq!(event_types, vec!["completed", "completed"]);
    for row in &notifications {
        assert_eq!(row.try_get::<String>("", "recipient").unwrap(), "primary");
        assert_eq!(
            row.try_get::<serde_json::Value>("", "payload").unwrap()["event_type"],
            "media.notification"
        );
    }

    let deliveries = SeaOrmNotificationOutbox::new(test_db.connection().clone())
        .lease_pending(
            NotificationId::new(),
            time::OffsetDateTime::now_utc() + time::Duration::seconds(1),
            time::Duration::seconds(30),
            10,
        )
        .await
        .unwrap();
    let completed = deliveries
        .iter()
        .find(|delivery| delivery.event_type() == NotificationEventType::Completed)
        .unwrap();
    assert!(
        completed.card_key().is_some(),
        "terminal completion edits the lifecycle card"
    );
    assert!(
        deliveries
            .iter()
            .all(|delivery| delivery.card_key().is_some()),
        "all structured deliveries retain the lifecycle card key"
    );
}

#[tokio::test]
async fn published_episode_is_completed_before_a_later_episode_retries() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("rezka://season-resume"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();

    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "media_pipeline".to_owned(), 1).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "media_pipeline".to_owned(),
            1,
            std::collections::BTreeMap::from([(
                "artifact_file_size_bytes".to_owned(),
                CheckpointValue::Unsigned(248_300_093),
            )]),
        )
        .unwrap(),
        JobEvent::stage_started(JobEventId::new(), 1, "media_pipeline".to_owned(), 1).unwrap(),
        JobEvent::stage_failed(
            JobEventId::new(),
            1,
            "media_pipeline".to_owned(),
            1,
            true,
            "source_transfer_transient".to_owned(),
        )
        .unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let tasks = query(
        test_db.connection(),
        "SELECT ordinal, state FROM job_tasks WHERE job_id = \
         (SELECT id FROM jobs WHERE result_ref = 'rezka://season-resume') ORDER BY ordinal",
    )
    .await;
    assert_eq!(tasks[0].try_get::<i32>("", "ordinal").unwrap(), 0);
    assert_eq!(
        tasks[0].try_get::<String>("", "state").unwrap(),
        "completed"
    );
    assert_eq!(tasks[1].try_get::<i32>("", "ordinal").unwrap(), 1);
    assert_eq!(tasks[1].try_get::<String>("", "state").unwrap(), "pending");
}

#[tokio::test]
async fn partial_completion_creates_plex_and_partial_notifications_once() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("partial-private-reference"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Partial, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let notifications = notification_rows(&test_db).await;
    let event_types = notifications
        .iter()
        .map(|row| row.try_get::<String>("", "event_type").unwrap())
        .collect::<Vec<_>>();
    assert_eq!(event_types, vec!["partial", "partial"]);
    for row in &notifications {
        assert_eq!(
            row.try_get::<serde_json::Value>("", "payload").unwrap()["state"],
            "partial"
        );
    }
}

#[tokio::test]
async fn successful_session_refresh_is_silent() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(
        operation_key(),
        new_job("selection:session-refresh:018f3f86-7b4c-7b4f-9b6a-6d62f45bb111"),
    )
    .await
    .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "execution".to_owned(), 1).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "execution".to_owned(),
            1,
            Default::default(),
        )
        .unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Completed, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let notifications = notification_rows(&test_db).await;
    assert!(notifications.is_empty());

    let deliveries = SeaOrmNotificationOutbox::new(test_db.connection().clone())
        .lease_pending(
            NotificationId::new(),
            time::OffsetDateTime::now_utc() + time::Duration::seconds(1),
            time::Duration::seconds(30),
            10,
        )
        .await
        .unwrap();
    assert!(deliveries.is_empty());
}

#[tokio::test]
async fn different_source_events_keep_only_the_latest_non_terminal_status() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("semantic-notification-dedupe"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "download".to_owned(),
            0,
            Default::default(),
        )
        .unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let counts = query(
        test_db.connection(),
        "SELECT event_type, count(*)::bigint AS count FROM notification_outbox \
         GROUP BY event_type ORDER BY event_type",
    )
    .await;
    assert_eq!(
        counts
            .iter()
            .map(|row| (
                row.try_get::<String>("", "event_type").unwrap(),
                row.try_get::<i64>("", "count").unwrap(),
            ))
            .collect::<Vec<_>>(),
        vec![("downloaded".to_owned(), 1)]
    );
}

#[tokio::test]
async fn progress_milestones_replace_the_same_initiator_card_across_retries() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("rezka://progress-dedupe"))
        .await
        .unwrap();

    // First lease: downloading starts, then fails retryably. The same card is
    // updated with curated recovery state instead of creating another row.
    let first = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        JobEvent::stage_failed(
            JobEventId::new(),
            0,
            "download".to_owned(),
            0,
            true,
            "network_timeout".to_owned(),
        )
        .unwrap(),
    ] {
        leases
            .report_event(operation_key(), first.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    // Recovered lease: the download stage restarts (a retry, deduped), completes,
    // and transcoding begins and restarts once (also deduped).
    let second = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "download".to_owned(),
            0,
            Default::default(),
        )
        .unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "transcode".to_owned(), 1).unwrap(),
    ] {
        leases
            .report_event(operation_key(), second.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let progress = query(
        test_db.connection(),
        "SELECT event_type, recipient, payload, generation FROM notification_outbox",
    )
    .await;
    assert_eq!(progress.len(), 1);
    assert_eq!(
        progress[0].try_get::<String>("", "event_type").unwrap(),
        "transcoding-started"
    );
    assert_eq!(
        progress[0].try_get::<String>("", "recipient").unwrap(),
        "primary"
    );
    assert_eq!(progress[0].try_get::<i64>("", "generation").unwrap(), 7);
    let payload = structured_payload(&progress[0], &["progress-dedupe"]);
    assert_eq!(payload["state"], "processing");
    assert_eq!(payload["stage"], "process");
}

#[tokio::test]
async fn non_terminal_job_notifications_coalesce_into_one_generation_ordered_card() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("coalesced-status-card"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();

    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "download".to_owned(),
            0,
            Default::default(),
        )
        .unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let rows = query(
        test_db.connection(),
        "SELECT event_type, payload, generation, delivered_at, dead_at
         FROM notification_outbox",
    )
    .await;
    assert_eq!(rows.len(), 1, "one job must keep one mutable status card");
    assert_eq!(
        rows[0].try_get::<String>("", "event_type").unwrap(),
        "downloaded"
    );
    assert_eq!(rows[0].try_get::<i64>("", "generation").unwrap(), 3);
    assert_eq!(
        rows[0].try_get::<serde_json::Value>("", "payload").unwrap()["state"],
        "processing"
    );
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
}

#[tokio::test]
async fn job_lifecycle_projects_one_terminal_card_and_one_final_push() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(
            operation_key(),
            new_job("selection:structured-lifecycle-card"),
        )
        .await
        .unwrap();
    test_db
        .connection()
        .execute_unprepared(
            "INSERT INTO search_executions (result_ref, payload) VALUES \
             ('selection:structured-lifecycle-card', \
              '{\"title\":\"Structured Show\",\"media_kind\":\"series\",\"season\":1,\"translation\":\"Studio Dub\",\"episodes\":[{\"season\":1,\"episode\":1}]}')",
        )
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();

    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Completed, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let rows = query(
        test_db.connection(),
        "SELECT event_type, payload, generation FROM notification_outbox ORDER BY event_type",
    )
    .await;
    assert_eq!(rows.len(), 2, "one mutable card and one final push");

    let card = rows
        .iter()
        .find(|row| {
            row.try_get::<serde_json::Value>("", "payload").unwrap()["delivery_kind"] == "card"
        })
        .unwrap();
    let card_payload = card.try_get::<serde_json::Value>("", "payload").unwrap();
    assert_eq!(card_payload["event_type"], "media.notification");
    assert_eq!(
        card_payload["card_key"],
        format!("media-job:{}", created.id())
    );
    assert_eq!(card_payload["state"], "completed");
    assert_eq!(card_payload["terminal"], true);
    assert!(card.try_get::<i64>("", "generation").unwrap() > 1);

    let push = rows
        .iter()
        .find(|row| {
            row.try_get::<serde_json::Value>("", "payload").unwrap()["delivery_kind"]
                == "final-push"
        })
        .unwrap();
    let push_payload = push.try_get::<serde_json::Value>("", "payload").unwrap();
    assert_eq!(
        push_payload["card_key"],
        format!("media-job:{}", created.id())
    );
    assert_eq!(push_payload["state"], "completed");
    assert_eq!(push_payload["terminal"], true);
    assert_eq!(push_payload["revision"], card_payload["revision"]);
}

#[tokio::test]
async fn detailed_notification_projects_exact_tracked_episode_and_published_artifact() {
    let (test_db, jobs, leases) = setup().await;
    let result_ref = "selection:tracking:00000000-0000-0000-0000-000000000777:2:8";
    let created = jobs
        .create(operation_key(), new_job(result_ref))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO search_executions (result_ref, payload) VALUES ($1, $2)",
            [
                result_ref.into(),
                serde_json::json!({
                    "title": "Клинки Хранителей",
                    "media_kind": "series",
                    "translation": "AniLibria",
                    "episodes": [{"season": 2, "episode": 8}]
                })
                .into(),
            ],
        ))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "media_pipeline".to_owned(), 1).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "media_pipeline".to_owned(),
            1,
            detailed_artifact_checkpoint("stereo", 440_401_920, 1_421, 252),
        )
        .unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let publishing = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox \
         WHERE payload->>'delivery_kind' = 'card'",
    )
    .await
    .remove(0)
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(publishing["state"], "publishing");
    assert_eq!(publishing["progress"]["current_episode"], 8);
    assert_eq!(publishing["progress"]["total_episodes"], 1);
    assert_eq!(publishing["result"]["publication"]["episode"], 8);
    assert_eq!(publishing["issue"]["code"], "plex_publish_recovering");
    assert_eq!(
        publishing["actions"],
        serde_json::json!(["retry", "details"])
    );

    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::transition(JobEventId::new(), JobState::Completed, None).unwrap(),
        )
        .await
        .unwrap();

    let rows = query(
        test_db.connection(),
        "SELECT aggregate_type, payload FROM notification_outbox ORDER BY id",
    )
    .await;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row.try_get::<String>("", "aggregate_type").unwrap() == "job"),
        "automatic tracking download must not create a separate discovery delivery",
    );
    let card = rows
        .iter()
        .map(|row| row.try_get::<serde_json::Value>("", "payload").unwrap())
        .find(|payload| payload["delivery_kind"] == "card")
        .unwrap();
    assert_eq!(card["card_key"], format!("media-job:{}", created.id()));
    assert_eq!(card["media"]["origin"], "tracked-episode");
    assert_eq!(card["progress"]["current_episode"], 8);
    assert_eq!(card["progress"]["total_episodes"], 1);
    assert_eq!(
        card["result"]["video"],
        serde_json::json!({
            "codec": "hevc",
            "profile": "Main",
            "width": 1920,
            "height": 1080
        })
    );
    assert_eq!(
        card["result"]["audio"],
        serde_json::json!({
            "language": "rus",
            "codec": "aac",
            "channels": 2,
            "channel_layout": "stereo",
            "title": "AniLibria"
        })
    );
    assert_eq!(card["result"]["subtitles"]["downloaded"], 2);
    assert_eq!(card["result"]["file_size_bytes"], 440_401_920);
    assert_eq!(card["result"]["duration_seconds"], 1_421);
    assert_eq!(
        card["result"]["processing"],
        serde_json::json!({"mode": "vaapi-upscale", "elapsed_seconds": 252})
    );
    assert_eq!(card["result"]["publication"]["library"], "tv-shows");
    assert_eq!(card["result"]["publication"]["season"], 2);
    assert_eq!(card["result"]["publication"]["episode"], 8);
}

#[tokio::test]
async fn detailed_notification_aggregates_only_consistent_measured_artifacts() {
    let (test_db, jobs, leases) = setup().await;
    let result_ref = "selection:detailed-season";
    jobs.create(operation_key(), new_job(result_ref))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO search_executions (result_ref, payload) VALUES ($1, $2)",
            [
                result_ref.into(),
                serde_json::json!({
                    "title": "Season Show",
                    "media_kind": "series",
                    "season": 1,
                    "translation": "AniLibria",
                    "episodes": [
                        {"season": 1, "episode": 1},
                        {"season": 1, "episode": 2}
                    ]
                })
                .into(),
            ],
        ))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    for (ordinal, checkpoint) in [
        (
            0,
            detailed_artifact_checkpoint("stereo", 400_000_000, 1_400, 200),
        ),
        (
            1,
            detailed_artifact_checkpoint("5.1", 500_000_000, 1_500, 250),
        ),
    ] {
        for event in [
            JobEvent::stage_started(JobEventId::new(), ordinal, "media_pipeline".to_owned(), 1)
                .unwrap(),
            JobEvent::stage_completed(
                JobEventId::new(),
                ordinal,
                "media_pipeline".to_owned(),
                1,
                checkpoint,
            )
            .unwrap(),
        ] {
            leases
                .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
                .await
                .unwrap();
        }
    }
    for state in [
        JobState::Publishing,
        JobState::PlexPending,
        JobState::Completed,
    ] {
        leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::transition(JobEventId::new(), state, None).unwrap(),
            )
            .await
            .unwrap();
    }

    let card = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox \
         WHERE payload->>'delivery_kind' = 'card'",
    )
    .await
    .remove(0)
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(card["progress"]["completed_episodes"], 2);
    assert_eq!(card["progress"]["total_episodes"], 2);
    assert_eq!(card["result"]["file_size_bytes"], 900_000_000_u64);
    assert_eq!(card["result"]["duration_seconds"], 2_900_u64);
    assert_eq!(card["result"]["subtitles"]["downloaded"], 4);
    assert_eq!(card["result"]["subtitles"]["missing"], 0);
    assert_eq!(card["result"]["processing"]["elapsed_seconds"], 450);
    assert_eq!(card["result"]["video"]["width"], 1920);
    assert!(
        card["result"].get("audio").is_none(),
        "conflicting measured audio layouts must omit aggregate audio",
    );
    assert_eq!(card["result"]["publication"]["season"], 1);
    assert!(card["result"]["publication"].get("episode").is_none());
}

#[tokio::test]
async fn detailed_notification_prowlarr_reports_original_without_unmeasured_probe_fields() {
    let (test_db, jobs, leases) = setup().await;
    let result_ref = "selection:detailed-prowlarr";
    jobs.create(
        operation_key(),
        new_job_for_provider(
            result_ref,
            PRIMARY_USER_ID,
            NotifyScope::Initiator,
            Provider::Prowlarr,
        ),
    )
    .await
    .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO search_executions (result_ref, payload) VALUES ($1, $2)",
            [
                result_ref.into(),
                serde_json::json!({
                    "title": "Torrent Movie",
                    "media_kind": "movie"
                })
                .into(),
            ],
        ))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Completed, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let card = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox \
         WHERE payload->>'delivery_kind' = 'card'",
    )
    .await
    .remove(0)
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(
        card["result"]["processing"],
        serde_json::json!({"mode": "original"})
    );
    assert_eq!(card["result"]["publication"]["library"], "movies");
    assert!(card["result"].get("video").is_none());
    assert!(card["result"].get("audio").is_none());
    assert!(!card.to_string().contains("vaapi"));
}

#[tokio::test]
async fn completed_prowlarr_season_counts_only_published_episode_tasks() {
    let (test_db, jobs, leases) = setup().await;
    let result_ref = "selection:prowlarr-completed-season";
    let created = jobs
        .create(
            operation_key(),
            new_job_for_provider(
                result_ref,
                PRIMARY_USER_ID,
                NotifyScope::Initiator,
                Provider::Prowlarr,
            ),
        )
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO search_executions (result_ref, payload) VALUES ($1, $2)",
            [
                result_ref.into(),
                serde_json::json!({
                    "title": "Avatar: The Last Airbender",
                    "media_kind": "series",
                    "season": 2
                })
                .into(),
            ],
        ))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    for ordinal in 1..=7 {
        for event in [
            JobEvent::stage_started(JobEventId::new(), ordinal, "plex_reconcile".to_owned(), 0)
                .unwrap(),
            JobEvent::stage_completed(
                JobEventId::new(),
                ordinal,
                "plex_reconcile".to_owned(),
                0,
                Default::default(),
            )
            .unwrap(),
        ] {
            leases
                .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
                .await
                .unwrap();
        }
    }
    for state in [
        JobState::Publishing,
        JobState::PlexPending,
        JobState::Completed,
    ] {
        leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::transition(JobEventId::new(), state, None).unwrap(),
            )
            .await
            .unwrap();
    }

    let card = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox \
         WHERE payload->>'delivery_kind' = 'card'",
    )
    .await
    .remove(0)
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(card["progress"]["completed_episodes"], 7);
    assert_eq!(card["progress"]["total_episodes"], 7);
    assert_eq!(card["media"]["job_id"], created.id().to_string());
}

#[tokio::test]
async fn detailed_notification_projects_retry_recovery_and_terminal_actions() {
    let (test_db, jobs, leases) = setup().await;
    let result_ref = "selection:detailed-recovery";
    jobs.create(operation_key(), new_job(result_ref))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO search_executions (result_ref, payload) VALUES ($1, $2)",
            [
                result_ref.into(),
                serde_json::json!({
                    "title": "Recovery Show",
                    "media_kind": "series",
                    "translation": "AniLibria",
                    "episodes": [{"season": 1, "episode": 7}]
                })
                .into(),
            ],
        ))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE job_stages SET attempt_count = 3 WHERE name = 'download'; \
             UPDATE runner_lifecycle SET sticky_attempt_count = 3 WHERE singleton = true",
        )
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_failed(
                JobEventId::new(),
                0,
                "download".to_owned(),
                0,
                true,
                "source_transfer_transient".to_owned(),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let recovering = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox \
         WHERE payload->>'delivery_kind' = 'card'",
    )
    .await
    .remove(0)
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(recovering["state"], "downloading");
    assert_eq!(recovering["progress"]["current_episode"], 7);
    assert_eq!(recovering["progress"]["connection_attempt"], 3);
    assert_eq!(recovering["progress"]["connection_attempt_limit"], 20);
    assert_eq!(recovering["progress"]["vpn_rotation_pending"], true);
    assert_eq!(recovering["issue"]["code"], "source_recovering");
    assert_eq!(
        recovering["issue"]["message"],
        "source transfer is being recovered"
    );
    assert!(!recovering.to_string().contains("source_transfer_transient"));

    test_db
        .connection()
        .execute_unprepared(
            "UPDATE runner_lifecycle SET state = 'ready', previous_ip = current_ip, \
             current_ip = '198.51.100.44', sticky_job_id = NULL, sticky_attempt_count = 0 \
             WHERE singleton = true",
        )
        .await
        .unwrap();
    let retry = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
    ] {
        leases
            .report_event(operation_key(), retry.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }
    test_db
        .connection()
        .execute_unprepared("UPDATE job_stages SET attempt_count = 20 WHERE name = 'download'")
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            retry.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_failed(
                JobEventId::new(),
                0,
                "download".to_owned(),
                0,
                true,
                "source_transfer_transient".to_owned(),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let failed = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox \
         WHERE payload->>'delivery_kind' = 'card'",
    )
    .await
    .remove(0)
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["progress"]["connection_attempt"], 20);
    assert_eq!(failed["progress"]["connection_attempt_limit"], 20);
    assert_eq!(
        failed["actions"],
        serde_json::json!(["retry", "search-alternative", "details"])
    );
}

#[tokio::test]
async fn notification_card_uses_canonical_specials_coordinates() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(
        operation_key(),
        new_job("selection:canonical-specials-card"),
    )
    .await
    .unwrap();
    test_db
        .connection()
        .execute_unprepared(
            "INSERT INTO search_executions (result_ref, payload) VALUES \
             ('selection:canonical-specials-card', \
              '{\"title\":\"Attack on Titan OVA-1\",\"media_kind\":\"series\",\"season\":1,\"translation\":\"Dub\",\"episodes\":[{\"season\":1,\"episode\":1}],\"episode_mappings\":[{\"provider\":{\"season\":1,\"episode\":1},\"canonical\":{\"season\":0,\"episode\":1},\"canonical_title\":\"Attack on Titan\"}]}')",
        )
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        )
        .await
        .unwrap();

    let card = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox WHERE payload->>'delivery_kind' = 'card'",
    )
    .await
    .remove(0)
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(card["media"]["title"], "Attack on Titan");
    assert_eq!(card["media"]["season"], 0);
    assert_eq!(card["progress"]["current_episode"], 1);

    let deliveries = SeaOrmNotificationOutbox::new(test_db.connection().clone())
        .lease_pending(
            NotificationId::new(),
            time::OffsetDateTime::now_utc() + time::Duration::seconds(1),
            time::Duration::seconds(30),
            10,
        )
        .await
        .expect("the canonical specials card must be dispatchable");
    let NotificationContent::Media(notification) = deliveries[0].content() else {
        panic!("expected a structured media notification");
    };
    assert_eq!(notification.media().title(), "Attack on Titan");
    assert_eq!(notification.media().season(), Some(0));
}

#[tokio::test]
async fn notification_card_sanitizes_prowlarr_release_title() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("selection:prowlarr-release-title"))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_unprepared(
            "INSERT INTO search_executions (result_ref, payload) VALUES \
             ('selection:prowlarr-release-title', \
              '{\"title\":\"[S02] | Mashle: Magic and Muscles | WEBRip 1080p\",\"media_kind\":\"series\",\"season\":2,\"episode\":7}')",
        )
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();

    let deliveries = SeaOrmNotificationOutbox::new(test_db.connection().clone())
        .lease_pending(
            NotificationId::new(),
            time::OffsetDateTime::now_utc() + time::Duration::seconds(1),
            time::Duration::seconds(30),
            10,
        )
        .await
        .expect("the Prowlarr notification must be dispatchable");
    let NotificationContent::Media(notification) = deliveries[0].content() else {
        panic!("expected a structured media notification");
    };
    assert_eq!(
        notification.media().title(),
        "[S02] - Mashle: Magic and Muscles - WEBRip 1080p"
    );
    assert_eq!(notification.progress().unwrap().current_episode(), Some(7));
}

#[tokio::test]
async fn partial_season_card_aggregates_task_states_and_episode_coordinates() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("selection:partial-season-card"))
        .await
        .unwrap();
    let episodes = (1..=12)
        .map(|episode| serde_json::json!({"season": 1, "episode": episode}))
        .collect::<Vec<_>>();
    test_db
        .connection()
        .execute_unprepared(&format!(
            "INSERT INTO search_executions (result_ref, payload) VALUES \
             ('selection:partial-season-card', '{{\"title\":\"Season Show\",\"media_kind\":\"series\",\"season\":1,\"episodes\":{}}}')",
            serde_json::Value::Array(episodes)
        ))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    for event in [
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }
    test_db
        .connection()
        .execute_unprepared(&format!(
            "UPDATE job_tasks SET state = 'completed' WHERE job_id = '{}'; \
             INSERT INTO job_tasks (id, job_id, ordinal, state) \
             SELECT gen_random_uuid(), '{}', ordinal, \
                    CASE WHEN ordinal = 11 THEN 'failed' ELSE 'completed' END \
             FROM generate_series(1, 11) AS ordinal",
            created.id(),
            created.id(),
        ))
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::transition(JobEventId::new(), JobState::Partial, None).unwrap(),
        )
        .await
        .unwrap();

    let card = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox WHERE payload->>'delivery_kind' = 'card'",
    )
    .await
    .remove(0)
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(card["state"], "partial");
    assert_eq!(card["progress"]["completed_episodes"], 11);
    assert_eq!(card["progress"]["total_episodes"], 12);
    assert_eq!(
        card["progress"]["missing_episodes"],
        serde_json::json!([{"season": 1, "episode": 12}])
    );
    assert_eq!(
        card["actions"],
        serde_json::json!(["retry-missing", "search-alternative", "details"])
    );
    assert!(card.get("issue").is_none());
}

#[tokio::test]
async fn storage_block_notifies_both_family_recipients_once() {
    let (test_db, jobs, leases) = setup().await;
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO search_executions (result_ref, payload) VALUES ($1, $2)",
            [
                "selection:blocked-private-reference".into(),
                serde_json::json!({
                    "source": "rezka",
                    "title": "Случайная любовь",
                    "media_kind": "series",
                    "season": 1,
                    "episode": 1,
                    "translation_id": 238,
                    "translation": "Оригинал (+субтитры)"
                })
                .into(),
            ],
        ))
        .await
        .unwrap();
    jobs.create(
        operation_key(),
        new_job_with_notifications(
            "selection:blocked-private-reference",
            SECONDARY_USER_ID,
            NotifyScope::Family,
        ),
    )
    .await
    .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "media_pipeline".to_owned(), 1).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "media_pipeline".to_owned(),
            1,
            [
                (
                    "storage_available_bytes".to_owned(),
                    CheckpointValue::Unsigned(23 * 1024 * 1024 * 1024),
                ),
                (
                    "storage_required_bytes".to_owned(),
                    CheckpointValue::Unsigned(24 * 1024 * 1024 * 1024),
                ),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::BlockedStorage, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let blocked = jobs
        .find_for_owner(lease.job().id(), SECONDARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(blocked.state(), JobState::BlockedStorage);
    assert!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
            .unwrap()
            .is_none()
    );

    let notifications = notification_rows(&test_db).await;
    assert_eq!(
        notifications
            .iter()
            .map(|row| (
                row.try_get::<String>("", "event_type").unwrap(),
                row.try_get::<String>("", "recipient").unwrap(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("blocked-storage".to_owned(), "primary".to_owned()),
            ("blocked-storage".to_owned(), "primary".to_owned()),
            ("blocked-storage".to_owned(), "secondary".to_owned()),
            ("blocked-storage".to_owned(), "secondary".to_owned()),
        ]
    );
    for row in &notifications {
        let payload = structured_payload(row, &["blocked-private-reference"]);
        assert_eq!(payload["state"], "needs-action");
        assert_eq!(payload["media"]["title"], "Случайная любовь");
        assert_eq!(payload["media"]["provider"], "rezka");
        assert_eq!(payload["media"]["translation"], "Оригинал (+субтитры)");
        assert_eq!(payload["issue"]["code"], "storage_blocked");
        assert_eq!(
            payload["progress"]["storage_available_bytes"],
            23 * 1024_u64 * 1024 * 1024
        );
        assert_eq!(
            payload["progress"]["storage_required_bytes"],
            24 * 1024_u64 * 1024 * 1024
        );
        assert_eq!(
            payload["actions"],
            serde_json::json!(["resume-storage", "details"])
        );
    }

    let deliveries = SeaOrmNotificationOutbox::new(test_db.connection().clone())
        .lease_pending(
            NotificationId::new(),
            time::OffsetDateTime::now_utc() + time::Duration::seconds(1),
            time::Duration::seconds(30),
            10,
        )
        .await
        .unwrap();
    assert_eq!(
        deliveries
            .iter()
            .filter(|delivery| delivery.event_type() == NotificationEventType::BlockedStorage)
            .count(),
        4
    );
    let status_keys = deliveries
        .iter()
        .map(|delivery| delivery.status_key().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(status_keys.len(), 1, "all events for one job edit one card");
}

#[tokio::test]
async fn storage_blocked_job_does_not_occupy_the_execution_slot() {
    let (test_db, jobs, leases) = setup().await;
    let blocked = jobs
        .create(operation_key(), new_job("selection:parked-storage"))
        .await
        .unwrap();
    let first = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::transition(JobEventId::new(), JobState::BlockedStorage, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), first.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let queued = jobs
        .create(operation_key(), new_job("selection:next-after-storage"))
        .await
        .unwrap();
    assert_eq!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await,
        Err(media_core::PortError::Conflict),
    );
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE runner_lifecycle SET state = 'ready', previous_ip = current_ip, \
             current_ip = '198.51.100.2', sticky_job_id = NULL, sticky_attempt_count = 0 \
             WHERE singleton = true",
        )
        .await
        .unwrap();
    let second = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(second.job().id(), queued.id());
    assert_ne!(second.job().id(), blocked.id());
    assert_eq!(
        jobs.find_for_owner(blocked.id(), PRIMARY_USER_ID)
            .await
            .unwrap()
            .unwrap()
            .state(),
        JobState::BlockedStorage
    );
}

#[tokio::test]
async fn terminal_stage_failure_creates_one_sanitized_failure_notification() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("failure-private-reference"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "execution".to_owned(), 2).unwrap(),
        JobEvent::stage_failed(
            JobEventId::new(),
            0,
            "execution".to_owned(),
            2,
            false,
            "provider_error:https://secret.example/token".to_owned(),
        )
        .unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let failed = query(
        test_db.connection(),
        "SELECT event_type, recipient, payload FROM notification_outbox \
         WHERE event_type = 'failed'",
    )
    .await;
    assert_eq!(failed.len(), 2);
    for row in failed {
        let payload = row.try_get::<serde_json::Value>("", "payload").unwrap();
        assert_eq!(payload["state"], "failed");
        assert!(!payload.to_string().contains("secret.example"));
        assert!(!payload.to_string().contains("token"));
    }
}

#[tokio::test]
async fn successful_runner_transition_chain_reaches_completed_and_releases_the_lease() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("successful-chain"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    for state in [
        JobState::Publishing,
        JobState::PlexPending,
        JobState::Completed,
    ] {
        leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::transition(JobEventId::new(), state, None).unwrap(),
            )
            .await
            .unwrap();
    }

    assert_eq!(
        jobs.find_for_owner(created.id(), PRIMARY_USER_ID)
            .await
            .unwrap()
            .unwrap()
            .state(),
        JobState::Completed,
    );
    assert!(
        query(test_db.connection(), "SELECT id FROM job_leases")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn retryable_stage_fails_terminally_on_third_attempt_and_releases_lease() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(
            operation_key(),
            new_job_for_provider(
                "retry-stage",
                PRIMARY_USER_ID,
                NotifyScope::Initiator,
                Provider::Prowlarr,
            ),
        )
        .await
        .unwrap();
    for attempt in 1..=3 {
        let lease = match leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
        {
            Ok(Some(lease)) => lease,
            Err(media_core::PortError::Conflict) => {
                test_db
                    .connection()
                    .execute_unprepared(&format!(
                        "UPDATE runner_lifecycle SET state = 'ready', previous_ip = current_ip, \
                         current_ip = '198.51.100.{}', sticky_job_id = NULL, \
                         sticky_attempt_count = 0 WHERE singleton = true",
                        attempt + 10
                    ))
                    .await
                    .unwrap();
                leases
                    .lease_next(
                        operation_key(),
                        RUNNER_CLIENT_ID,
                        time::Duration::seconds(60),
                    )
                    .await
                    .unwrap()
                    .unwrap()
            }
            other => panic!("unexpected lease result: {other:?}"),
        };
        leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::started(JobEventId::new()),
            )
            .await
            .unwrap();
        leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
            )
            .await
            .unwrap();
        let job = leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::stage_failed(
                    JobEventId::new(),
                    0,
                    "download".to_owned(),
                    0,
                    true,
                    "network_timeout".to_owned(),
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            job.state(),
            if attempt < 3 {
                JobState::Queued
            } else {
                JobState::Failed
            },
        );
        assert!(
            query(test_db.connection(), "SELECT id FROM job_leases")
                .await
                .is_empty()
        );
    }

    let stage = query(
        test_db.connection(),
        "SELECT state, attempt_count FROM job_stages WHERE name = 'download'",
    )
    .await;
    assert_eq!(stage[0].try_get::<String>("", "state").unwrap(), "failed");
    assert_eq!(stage[0].try_get::<i32>("", "attempt_count").unwrap(), 3);
    assert!(
        query(test_db.connection(), "SELECT id FROM job_leases")
            .await
            .is_empty()
    );
    let persisted = jobs
        .find_for_owner(created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.state(), JobState::Failed);
}

#[tokio::test]
async fn retryable_rezka_stage_fails_terminally_on_twentieth_attempt() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("rezka-retry-stage"))
        .await
        .unwrap();

    for attempt in 1..=20 {
        let lease = match leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
        {
            Ok(Some(lease)) => lease,
            Err(media_core::PortError::Conflict) => {
                test_db
                    .connection()
                    .execute_unprepared(&format!(
                        "UPDATE runner_lifecycle SET state = 'ready', previous_ip = current_ip, \
                         current_ip = '198.51.100.{}', sticky_job_id = NULL, \
                         sticky_attempt_count = 0 WHERE singleton = true",
                        attempt + 10
                    ))
                    .await
                    .unwrap();
                leases
                    .lease_next(
                        operation_key(),
                        RUNNER_CLIENT_ID,
                        time::Duration::seconds(60),
                    )
                    .await
                    .unwrap()
                    .unwrap()
            }
            other => panic!("unexpected lease result: {other:?}"),
        };
        leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::started(JobEventId::new()),
            )
            .await
            .unwrap();
        leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
            )
            .await
            .unwrap();
        let job = leases
            .report_event(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                JobEvent::stage_failed(
                    JobEventId::new(),
                    0,
                    "download".to_owned(),
                    0,
                    true,
                    "source_transfer_transient".to_owned(),
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            job.state(),
            if attempt < 20 {
                JobState::Queued
            } else {
                JobState::Failed
            },
        );
    }

    let stage = query(
        test_db.connection(),
        "SELECT state, attempt_count FROM job_stages WHERE name = 'download'",
    )
    .await;
    assert_eq!(stage[0].try_get::<String>("", "state").unwrap(), "failed");
    assert_eq!(stage[0].try_get::<i32>("", "attempt_count").unwrap(), 20);
    assert_eq!(
        jobs.find_for_owner(created.id(), PRIMARY_USER_ID)
            .await
            .unwrap()
            .unwrap()
            .state(),
        JobState::Failed,
    );
}

#[tokio::test]
async fn owner_retry_requeues_failed_work_idempotently() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(
            operation_key(),
            new_job_for_provider(
                "explicit-retry",
                PRIMARY_USER_ID,
                NotifyScope::Initiator,
                Provider::Prowlarr,
            ),
        )
        .await
        .unwrap();
    for _ in 0..3 {
        let lease = leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
            .unwrap()
            .unwrap();
        for event in [
            JobEvent::started(JobEventId::new()),
            JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
            JobEvent::stage_failed(
                JobEventId::new(),
                0,
                "download".to_owned(),
                0,
                true,
                "network_timeout".to_owned(),
            )
            .unwrap(),
        ] {
            leases
                .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
                .await
                .unwrap();
        }
    }

    let operation = operation_key();
    let retried = jobs
        .retry(operation, created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    let replay = jobs
        .retry(operation, created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(retried.state(), JobState::Queued);
    assert_eq!(replay, retried);
    assert!(
        jobs.retry(operation_key(), created.id(), SECONDARY_USER_ID)
            .await
            .unwrap()
            .is_none()
    );
    let stages = query(
        test_db.connection(),
        "SELECT state, attempt_count FROM job_stages WHERE name = 'download'",
    )
    .await;
    assert_eq!(stages[0].try_get::<String>("", "state").unwrap(), "pending");
    assert_eq!(stages[0].try_get::<i32>("", "attempt_count").unwrap(), 0);
    assert_eq!(
        query(
            test_db.connection(),
            "SELECT id FROM outbox_events WHERE event_type = 'job.retried'",
        )
        .await
        .len(),
        1,
    );
    assert_eq!(
        query(
            test_db.connection(),
            "SELECT notification_cycle FROM jobs WHERE id = \
             (SELECT id FROM jobs WHERE result_ref = 'explicit-retry')",
        )
        .await[0]
            .try_get::<i64>("", "notification_cycle")
            .unwrap(),
        2,
    );
}

#[tokio::test]
async fn owner_retry_requeues_needs_action_work_and_resets_running_task() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("resolved-identity"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "resolve_identity".to_owned(), 0).unwrap(),
        JobEvent::transition(
            JobEventId::new(),
            JobState::NeedsAction,
            Some(media_core::NeedsActionReason::IdentityAmbiguous),
        )
        .unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let retried = jobs
        .retry(operation_key(), created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(retried.state(), JobState::Queued);
    assert_eq!(retried.needs_action_reason(), None);
    let tasks = query(
        test_db.connection(),
        "SELECT state, attempt_count FROM job_tasks",
    )
    .await;
    assert_eq!(tasks[0].try_get::<String>("", "state").unwrap(), "pending");
    assert_eq!(tasks[0].try_get::<i32>("", "attempt_count").unwrap(), 0);
    let stages = query(
        test_db.connection(),
        "SELECT state, attempt_count FROM job_stages WHERE name = 'resolve_identity'",
    )
    .await;
    assert_eq!(stages[0].try_get::<String>("", "state").unwrap(), "pending");
    assert_eq!(stages[0].try_get::<i32>("", "attempt_count").unwrap(), 0);
}

#[tokio::test]
async fn owner_retry_requeues_storage_blocked_work_and_resets_pipeline_ledger() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("storage-resume"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "resolve_manifest".to_owned(), 0).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "resolve_manifest".to_owned(),
            0,
            Default::default(),
        )
        .unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "media_pipeline".to_owned(), 1).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "media_pipeline".to_owned(),
            1,
            Default::default(),
        )
        .unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::BlockedStorage, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let operation = operation_key();
    let retried = jobs
        .retry(operation, created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    let replay = jobs
        .retry(operation, created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(retried.state(), JobState::Queued);
    assert_eq!(replay, retried);
    let tasks = query(
        test_db.connection(),
        "SELECT state, attempt_count, checkpoint FROM job_tasks WHERE job_id = \
         (SELECT id FROM jobs WHERE result_ref = 'storage-resume')",
    )
    .await;
    assert_eq!(tasks[0].try_get::<String>("", "state").unwrap(), "pending");
    assert_eq!(tasks[0].try_get::<i32>("", "attempt_count").unwrap(), 0);
    assert_eq!(
        tasks[0]
            .try_get::<serde_json::Value>("", "checkpoint")
            .unwrap(),
        serde_json::json!({})
    );
    let stages = query(
        test_db.connection(),
        "SELECT state, attempt_count, checkpoint FROM job_stages ORDER BY ordinal",
    )
    .await;
    assert_eq!(stages.len(), 2);
    for stage in stages {
        assert_eq!(stage.try_get::<String>("", "state").unwrap(), "pending");
        assert_eq!(stage.try_get::<i32>("", "attempt_count").unwrap(), 0);
        assert_eq!(
            stage
                .try_get::<serde_json::Value>("", "checkpoint")
                .unwrap(),
            serde_json::json!({})
        );
    }
    assert!(
        jobs.retry(operation_key(), created.id(), SECONDARY_USER_ID)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn completed_job_retry_is_a_conflict() {
    let (_test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("completed-no-retry"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Completed, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    assert_eq!(
        jobs.retry(operation_key(), created.id(), PRIMARY_USER_ID)
            .await
            .unwrap_err(),
        media_core::PortError::Conflict,
    );
}

#[tokio::test]
async fn retry_can_replay_an_already_completed_stage_idempotently() {
    let (_test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("completed-stage-replay"))
        .await
        .unwrap();
    let first = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            first.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    for event in [
        JobEvent::stage_started(JobEventId::new(), 0, "resolve_manifest".to_owned(), 0).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "resolve_manifest".to_owned(),
            0,
            Default::default(),
        )
        .unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "execution".to_owned(), 2).unwrap(),
        JobEvent::stage_failed(
            JobEventId::new(),
            0,
            "execution".to_owned(),
            2,
            true,
            "execution_failed".to_owned(),
        )
        .unwrap(),
    ] {
        leases
            .report_event(operation_key(), first.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let second = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            second.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            second.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_started(JobEventId::new(), 0, "resolve_manifest".to_owned(), 0)
                .unwrap(),
        )
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            second.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_completed(
                JobEventId::new(),
                0,
                "resolve_manifest".to_owned(),
                0,
                Default::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn replayed_completed_wrapper_stage_can_fail_and_requeue() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("completed-wrapper-retry"))
        .await
        .unwrap();
    let first = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "execution".to_owned(), 2).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "execution".to_owned(),
            2,
            Default::default(),
        )
        .unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), first.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE job_leases SET created_at = now() - interval '2 seconds', \
             expires_at = now() - interval '1 second' WHERE id = $1",
            [first.lease_id().into_uuid().into()],
        ))
        .await
        .unwrap();

    let retry = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "execution".to_owned(), 2).unwrap(),
    ] {
        leases
            .report_event(operation_key(), retry.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }
    let replayed_stage = query(
        test_db.connection(),
        "SELECT state, attempt_count FROM job_stages WHERE name = 'execution'",
    )
    .await;
    assert_eq!(
        replayed_stage[0].try_get::<String>("", "state").unwrap(),
        "running"
    );
    assert_eq!(
        replayed_stage[0]
            .try_get::<i32>("", "attempt_count")
            .unwrap(),
        1,
        "successful Plex-pending replays must not consume failure attempts"
    );
    let requeued = leases
        .report_event(
            operation_key(),
            retry.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_failed(
                JobEventId::new(),
                0,
                "execution".to_owned(),
                2,
                true,
                "execution_failed".to_owned(),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(requeued.state(), JobState::Queued);
}

#[tokio::test]
async fn expired_runner_resumes_running_stage_from_its_durable_checkpoint() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("expired-stage"))
        .await
        .unwrap();
    let first = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            first.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            first.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        )
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            first.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_checkpoint(
                JobEventId::new(),
                0,
                "download".to_owned(),
                0,
                [(
                    "downloaded_bytes".to_owned(),
                    CheckpointValue::Unsigned(4096),
                )]
                .into_iter()
                .collect(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE job_leases SET created_at = now() - interval '2 seconds', \
             expires_at = now() - interval '1 second' WHERE id = $1",
            [first.lease_id().into_uuid().into()],
        ))
        .await
        .unwrap();

    let resumed = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    leases
        .report_event(
            operation_key(),
            resumed.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::started(JobEventId::new()),
        )
        .await
        .unwrap();
    leases
        .report_event(
            operation_key(),
            resumed.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        )
        .await
        .unwrap();

    let stage = query(
        test_db.connection(),
        "SELECT state, attempt_count, checkpoint FROM job_stages WHERE name = 'download'",
    )
    .await;
    assert_eq!(stage[0].try_get::<String>("", "state").unwrap(), "running");
    assert_eq!(stage[0].try_get::<i32>("", "attempt_count").unwrap(), 2);
    assert_eq!(
        stage[0]
            .try_get::<serde_json::Value>("", "checkpoint")
            .unwrap(),
        serde_json::json!({"downloaded_bytes": 4096}),
    );
}

#[tokio::test]
async fn active_cancel_is_cooperative_and_runner_acknowledgement_releases_the_lease() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("cooperative-cancel"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();

    let requested = jobs
        .cancel(operation_key(), created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requested.state(), JobState::CancelRequested);
    let heartbeat = leases
        .heartbeat(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(heartbeat.job().state(), JobState::CancelRequested);

    let cancelled = leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::transition(JobEventId::new(), JobState::Cancelled, None).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.state(), JobState::Cancelled);
    assert!(
        query(test_db.connection(), "SELECT id FROM job_leases")
            .await
            .is_empty()
    );
    let notifications = query(
        test_db.connection(),
        "SELECT payload FROM notification_outbox ORDER BY created_at",
    )
    .await;
    assert_eq!(
        notifications.len(),
        2,
        "cancel edits the card and sends one push"
    );
    let payloads = notifications
        .iter()
        .map(|row| row.try_get::<serde_json::Value>("", "payload").unwrap())
        .collect::<Vec<_>>();
    let card = payloads
        .iter()
        .find(|payload| payload["delivery_kind"] == "card")
        .unwrap();
    let push = payloads
        .iter()
        .find(|payload| payload["delivery_kind"] == "final-push")
        .unwrap();
    assert_eq!(card["state"], "cancelled");
    assert_eq!(card["terminal"], true);
    assert_eq!(push["revision"], card["revision"]);
}

#[tokio::test]
async fn job_detail_reports_the_running_stage_and_clears_it_when_idle() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("rezka://detail-stage"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }
    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_checkpoint(
                JobEventId::new(),
                0,
                "download".to_owned(),
                0,
                [
                    (
                        "kind".to_owned(),
                        CheckpointValue::String("direct".to_owned()),
                    ),
                    (
                        "state".to_owned(),
                        CheckpointValue::String("downloading".to_owned()),
                    ),
                    ("progress_percent".to_owned(), CheckpointValue::Unsigned(73)),
                    (
                        "downloaded_bytes".to_owned(),
                        CheckpointValue::Unsigned(4_402_341_478),
                    ),
                    (
                        "total_bytes".to_owned(),
                        CheckpointValue::Unsigned(6_012_954_214),
                    ),
                    (
                        "download_speed_bps".to_owned(),
                        CheckpointValue::Unsigned(19_293_798),
                    ),
                    ("eta_seconds".to_owned(), CheckpointValue::Unsigned(85)),
                    ("seeds".to_owned(), CheckpointValue::Unsigned(3)),
                    ("peers".to_owned(), CheckpointValue::Unsigned(1)),
                ]
                .into_iter()
                .collect(),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let progress_notifications = query(
        test_db.connection(),
        "SELECT event_type, recipient, payload, generation FROM notification_outbox",
    )
    .await;
    assert_eq!(progress_notifications.len(), 1);
    assert_eq!(
        progress_notifications[0]
            .try_get::<String>("", "event_type")
            .unwrap(),
        "download-progress"
    );
    assert_eq!(
        progress_notifications[0]
            .try_get::<String>("", "recipient")
            .unwrap(),
        "primary"
    );
    assert_eq!(
        progress_notifications[0]
            .try_get::<i64>("", "generation")
            .unwrap(),
        3
    );
    let payload = structured_payload(&progress_notifications[0], &[]);
    assert_eq!(payload["progress"]["percentage"], 73);
    assert_eq!(payload["progress"]["downloaded_bytes"], 4_402_341_478_u64);
    assert_eq!(payload["progress"]["total_bytes"], 6_012_954_214_u64);
    assert_eq!(payload["progress"]["download_speed_bps"], 19_293_798_u64);
    assert_eq!(payload["progress"]["eta_seconds"], 85);
    assert_eq!(payload["progress"]["seeds"], 3);
    assert_eq!(payload["progress"]["peers"], 1);
    assert_eq!(payload["progress"]["source_state"], "downloading");

    let detail = jobs
        .find_detail_for_owner(created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.current_stage.as_deref(), Some("download"));
    assert_eq!(detail.job.state(), JobState::Running);
    let progress = detail.progress.expect("running download progress");
    assert_eq!(progress.progress_percent, Some(73));
    assert_eq!(progress.downloaded_bytes, Some(4_402_341_478));
    assert_eq!(progress.total_bytes, Some(6_012_954_214));
    assert_eq!(progress.download_speed_bps, Some(19_293_798));
    assert_eq!(progress.eta_seconds, Some(85));

    test_db
        .connection()
        .execute_unprepared(
            "UPDATE job_stages SET checkpoint = \
             '{\"kind\":\"direct\",\"progress_percent\":101}'::jsonb \
             WHERE name = 'download'",
        )
        .await
        .unwrap();
    let malformed = jobs
        .find_detail_for_owner(created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(malformed.current_stage.as_deref(), Some("download"));
    assert_eq!(malformed.progress, None, "invalid progress is ignored");

    // Owner isolation still applies to the detail read.
    assert!(
        jobs.find_detail_for_owner(created.id(), SECONDARY_USER_ID)
            .await
            .unwrap()
            .is_none()
    );

    leases
        .report_event(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            JobEvent::stage_completed(
                JobEventId::new(),
                0,
                "download".to_owned(),
                0,
                Default::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let idle = jobs
        .find_detail_for_owner(created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        idle.current_stage, None,
        "a completed stage leaves no running stage to report",
    );
}

#[tokio::test]
async fn hls_progress_without_total_updates_only_the_initiators_card_without_a_fake_percent() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(
        operation_key(),
        new_job_with_notifications(
            "hls-progress-without-total",
            PRIMARY_USER_ID,
            NotifyScope::Family,
        ),
    )
    .await
    .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
        JobEvent::stage_checkpoint(
            JobEventId::new(),
            0,
            "download".to_owned(),
            0,
            [
                ("kind".to_owned(), CheckpointValue::String("hls".to_owned())),
                (
                    "downloaded_bytes".to_owned(),
                    CheckpointValue::Unsigned(734_003_200),
                ),
                (
                    "download_speed_bps".to_owned(),
                    CheckpointValue::Unsigned(8_388_608),
                ),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let rows = query(
        test_db.connection(),
        "SELECT event_type, recipient, payload FROM notification_outbox ORDER BY recipient",
    )
    .await;
    assert_eq!(rows.len(), 1, "active progress is initiator-only");
    let progress = rows
        .iter()
        .find(|row| row.try_get::<String>("", "event_type").unwrap() == "download-progress")
        .unwrap();
    assert_eq!(
        progress.try_get::<String>("", "recipient").unwrap(),
        "primary"
    );
    let payload = structured_payload(progress, &[]);
    assert_eq!(payload["progress"]["downloaded_bytes"], 734_003_200_u64);
    assert_eq!(payload["progress"]["download_speed_bps"], 8_388_608_u64);
    assert!(payload["progress"].get("percentage").is_none());
}

#[tokio::test]
async fn torrent_progress_card_includes_swarm_and_transfer_details() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(
        operation_key(),
        new_job_for_provider(
            "torrent-progress",
            PRIMARY_USER_ID,
            NotifyScope::Initiator,
            Provider::Prowlarr,
        ),
    )
    .await
    .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "torrent_monitor".to_owned(), 0).unwrap(),
        JobEvent::stage_checkpoint(
            JobEventId::new(),
            0,
            "torrent_monitor".to_owned(),
            0,
            [
                (
                    "kind".to_owned(),
                    CheckpointValue::String("torrent".to_owned()),
                ),
                ("progress_percent".to_owned(), CheckpointValue::Unsigned(42)),
                (
                    "downloaded_bytes".to_owned(),
                    CheckpointValue::Unsigned(4 * 1024 * 1024 * 1024),
                ),
                (
                    "total_bytes".to_owned(),
                    CheckpointValue::Unsigned(10 * 1024 * 1024 * 1024),
                ),
                (
                    "download_speed_bps".to_owned(),
                    CheckpointValue::Unsigned(16 * 1024 * 1024),
                ),
                ("eta_seconds".to_owned(), CheckpointValue::Unsigned(375)),
                ("seeds".to_owned(), CheckpointValue::Unsigned(12)),
                ("peers".to_owned(), CheckpointValue::Unsigned(4)),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let rows = query(
        test_db.connection(),
        "SELECT event_type, payload FROM notification_outbox",
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].try_get::<String>("", "event_type").unwrap(),
        "download-progress"
    );
    let payload = structured_payload(&rows[0], &[]);
    assert_eq!(payload["media"]["provider"], "prowlarr");
    assert_eq!(payload["progress"]["percentage"], 42);
    assert_eq!(
        payload["progress"]["downloaded_bytes"],
        4 * 1024_u64 * 1024 * 1024
    );
    assert_eq!(
        payload["progress"]["download_speed_bps"],
        16 * 1024_u64 * 1024
    );
}

#[tokio::test]
async fn job_detail_ignores_the_internal_execution_wrapper_stage() {
    let (_test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("rezka://execution-wrapper"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    // The runner wraps the whole task in an "execution" stage at a reserved high
    // ordinal, then reports the real phase underneath it. Both are running, and
    // the wrapper has the higher ordinal, so an unfiltered "latest running stage"
    // query would surface the wrapper instead of the meaningful phase.
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "execution".to_owned(), 1_000_000).unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "media_pipeline".to_owned(), 1).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let detail = jobs
        .find_detail_for_owner(created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        detail.current_stage.as_deref(),
        Some("media_pipeline"),
        "the real running phase is reported, not the internal execution wrapper",
    );
}

#[tokio::test]
async fn movie_transcode_milestone_persists_alongside_the_execution_wrapper() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("rezka://movie-transcode"))
        .await
        .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    // The exact task-0 sequence the runner emits for a movie / first episode. The
    // execution wrapper sits at a reserved high ordinal so the transcode
    // milestone at ordinal 2 no longer collides with it under the job_stages
    // UNIQUE(task_id, ordinal) constraint (which previously rolled back and
    // dropped the transcode milestone for every movie and first episode).
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "execution".to_owned(), 1_000_000).unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "resolve_manifest".to_owned(), 0).unwrap(),
        JobEvent::stage_completed(
            JobEventId::new(),
            0,
            "resolve_manifest".to_owned(),
            0,
            Default::default(),
        )
        .unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "media_pipeline".to_owned(), 1).unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "transcode".to_owned(), 2).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    let stages = query(
        test_db.connection(),
        "SELECT s.name FROM job_stages s JOIN job_tasks t ON s.task_id = t.id \
         WHERE t.job_id = (SELECT id FROM jobs WHERE result_ref = 'rezka://movie-transcode') \
         AND s.name = 'transcode' AND s.state = 'running'",
    )
    .await;
    assert_eq!(
        stages.len(),
        1,
        "the transcode milestone must persist rather than roll back on an ordinal collision",
    );

    let notifications = query(
        test_db.connection(),
        "SELECT recipient FROM notification_outbox WHERE event_type = 'transcoding-started'",
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert_eq!(
        notifications[0].try_get::<String>("", "recipient").unwrap(),
        "primary"
    );
}

#[tokio::test]
async fn family_job_routes_progress_to_initiator_but_terminal_events_to_both() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(
        operation_key(),
        new_job_with_notifications(
            "rezka://family-progress",
            SECONDARY_USER_ID,
            NotifyScope::Family,
        ),
    )
    .await
    .unwrap();
    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "media_pipeline".to_owned(), 1).unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 3).unwrap(),
        JobEvent::stage_started(JobEventId::new(), 0, "transcode".to_owned(), 2).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    // Progress milestones reach the initiator (Secondary) only, even though the
    // job is Family scope.
    let progress = query(
        test_db.connection(),
        "SELECT event_type, recipient FROM notification_outbox \
         WHERE event_type IN ('downloading-started', 'transcoding-started') \
         ORDER BY event_type, recipient",
    )
    .await;
    assert_eq!(
        progress
            .iter()
            .map(|row| (
                row.try_get::<String>("", "event_type").unwrap(),
                row.try_get::<String>("", "recipient").unwrap(),
            ))
            .collect::<Vec<_>>(),
        vec![("transcoding-started".to_owned(), "secondary".to_owned())],
    );

    for event in [
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Completed, None).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease.lease_id(), RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }

    // The terminal card keeps Family scope. Final pushes use the same recipients.
    let plex = query(
        test_db.connection(),
        "SELECT recipient FROM notification_outbox WHERE event_type = 'completed' \
         AND payload->>'delivery_kind' = 'card' ORDER BY recipient",
    )
    .await;
    assert_eq!(
        plex.iter()
            .map(|row| row.try_get::<String>("", "recipient").unwrap())
            .collect::<Vec<_>>(),
        vec!["primary".to_owned(), "secondary".to_owned()],
    );
}
