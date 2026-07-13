mod support;

use media_core::{
    PRIMARY_USER_ID, BootstrapClient, CheckpointValue, ClientRole, ClientStore, CredentialDigest,
    JobEvent, JobEventId, JobId, JobState, JobStore, LeaseStore, NewJob, NotificationEventType,
    NotificationId, NotifyScope, Provider, RUNNER_CLIENT_ID, SECONDARY_USER_ID,
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
    NewJob::new(
        JobId::new(),
        owner,
        Provider::Rezka,
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

fn assert_sanitized_message(row: &sea_orm::QueryResult, forbidden: &[&str]) {
    let payload = row.try_get::<serde_json::Value>("", "payload").unwrap();
    assert_eq!(
        payload.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["message"]
    );
    let message = payload["message"].as_str().unwrap();
    assert!(!message.contains("://"));
    for value in forbidden {
        assert!(
            !message.contains(value),
            "message leaked {value:?}: {message}"
        );
    }
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
    assert_eq!(
        payload.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["message"]
    );
    assert!(
        payload["message"]
            .as_str()
            .unwrap()
            .contains("Источник: Rezka")
    );
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
    assert_eq!(
        event_types,
        vec![
            "completed",
            "downloaded",
            "downloading-started",
            "encoding-complete",
            "plex-added",
            "started",
        ]
    );
    for row in &notifications {
        assert_eq!(row.try_get::<String>("", "recipient").unwrap(), "primary");
        assert_sanitized_message(row, &["private-provider-reference"]);
    }
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
    assert_eq!(
        event_types,
        vec![
            "downloaded",
            "encoding-complete",
            "partial",
            "plex-added",
            "started",
        ]
    );
    for row in &notifications {
        assert_sanitized_message(row, &["partial-private-reference"]);
    }
}

#[tokio::test]
async fn session_refresh_emits_only_session_lifecycle_notifications() {
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
    assert_eq!(
        notifications
            .iter()
            .map(|row| row.try_get::<String>("", "event_type").unwrap())
            .collect::<Vec<_>>(),
        vec!["session-refreshed", "started"],
    );
    for row in &notifications {
        assert_sanitized_message(row, &["plex", "download", "encoding"]);
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
    assert_eq!(deliveries.len(), 2);
    assert!(
        deliveries
            .iter()
            .any(|delivery| { delivery.event_type() == NotificationEventType::SessionRefreshed })
    );
}

#[tokio::test]
async fn different_source_events_dedupe_the_same_job_notification_type() {
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
        vec![
            ("downloaded".to_owned(), 1),
            ("downloading-started".to_owned(), 1),
            ("encoding-complete".to_owned(), 1),
            ("started".to_owned(), 1),
        ]
    );
}

#[tokio::test]
async fn progress_notifications_fire_once_per_phase_across_retries_and_to_initiator_only() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("rezka://progress-dedupe"))
        .await
        .unwrap();

    // First lease: downloading starts, then fails retryably. The stage start
    // produces one "downloading-started" notification; the retry must not add a
    // second one.
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

    let counts = query(
        test_db.connection(),
        "SELECT event_type, recipient, count(*)::bigint AS count FROM notification_outbox \
         WHERE event_type IN ('downloading-started', 'transcoding-started') \
         GROUP BY event_type, recipient ORDER BY event_type, recipient",
    )
    .await;
    assert_eq!(
        counts
            .iter()
            .map(|row| (
                row.try_get::<String>("", "event_type").unwrap(),
                row.try_get::<String>("", "recipient").unwrap(),
                row.try_get::<i64>("", "count").unwrap(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("downloading-started".to_owned(), "primary".to_owned(), 1),
            ("transcoding-started".to_owned(), "primary".to_owned(), 1),
        ],
        "each phase notifies its initiator exactly once, even across retries",
    );

    let progress = query(
        test_db.connection(),
        "SELECT event_type, payload FROM notification_outbox \
         WHERE event_type IN ('downloading-started', 'transcoding-started') \
         ORDER BY event_type",
    )
    .await;
    for row in &progress {
        assert_sanitized_message(row, &["progress-dedupe"]);
    }
    assert!(
        progress[0]
            .try_get::<serde_json::Value>("", "payload")
            .unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("Скачивание исходного видео началось")
    );
    assert!(
        progress[1]
            .try_get::<serde_json::Value>("", "payload")
            .unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("Перекодирование Rezka-видео через VAAPI началось")
    );
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
            // Terminal and lifecycle events keep the Family scope (both users).
            // Starting the media_pipeline umbrella only performs preflight and
            // must not claim that a real download has begun.
            ("blocked-storage".to_owned(), "primary".to_owned()),
            ("blocked-storage".to_owned(), "secondary".to_owned()),
            ("started".to_owned(), "primary".to_owned()),
            ("started".to_owned(), "secondary".to_owned()),
        ]
    );
    for row in &notifications {
        assert_sanitized_message(row, &["blocked-private-reference"]);
        let payload = row.try_get::<serde_json::Value>("", "payload").unwrap();
        let message = payload["message"].as_str().unwrap();
        assert!(message.contains("Медиа: Случайная любовь"));
        assert!(message.contains("Источник: Rezka"));
        assert!(message.contains("Что скачивается: сериал, серия S01E01"));
        assert!(message.contains("Перевод: Оригинал (+субтитры)"));
        assert!(message.contains("Качество: максимальное доступное"));
        assert!(message.contains("Куда попадёт: Plex / Сериалы (после staging и VAAPI)"));
        assert!(message.contains("Статус:"));
        assert!(message.contains("Этап:"));
        assert!(message.contains("Что дальше:"));
        if row.try_get::<String>("", "event_type").unwrap() == "blocked-storage" {
            assert!(message.contains("Свободно: 23.0 ГБ"));
            assert!(message.contains("Нужно: 24.0 ГБ"));
            assert!(message.contains("Не хватает: 1.0 ГБ"));
        }
        assert!(message.contains("Job ID:"));
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
        2
    );
}

#[tokio::test]
async fn storage_blocked_job_does_not_occupy_the_execution_slot() {
    let (_test_db, jobs, leases) = setup().await;
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
    assert_eq!(failed.len(), 1);
    let message = failed[0]
        .try_get::<serde_json::Value>("", "payload")
        .unwrap()["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(message.contains("provider_error"));
    assert_sanitized_message(
        &failed[0],
        &["secret.example", "token", "failure-private-reference"],
    );
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
        .create(operation_key(), new_job("retry-stage"))
        .await
        .unwrap();
    for attempt in 1..=3 {
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
async fn owner_retry_requeues_failed_work_idempotently() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("explicit-retry"))
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
}

#[tokio::test]
async fn job_detail_reports_the_running_stage_and_clears_it_when_idle() {
    let (_test_db, jobs, leases) = setup().await;
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

    let detail = jobs
        .find_detail_for_owner(created.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.current_stage.as_deref(), Some("download"));
    assert_eq!(detail.job.state(), JobState::Running);

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
        JobEvent::transition(JobEventId::new(), JobState::Publishing, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::PlexPending, None).unwrap(),
        JobEvent::transition(JobEventId::new(), JobState::Completed, None).unwrap(),
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
        vec![
            ("downloading-started".to_owned(), "secondary".to_owned()),
            ("transcoding-started".to_owned(), "secondary".to_owned()),
        ],
    );

    // A terminal Plex event keeps the Family scope: both recipients.
    let plex = query(
        test_db.connection(),
        "SELECT recipient FROM notification_outbox WHERE event_type = 'plex-added' \
         ORDER BY recipient",
    )
    .await;
    assert_eq!(
        plex.iter()
            .map(|row| row.try_get::<String>("", "recipient").unwrap())
            .collect::<Vec<_>>(),
        vec!["primary".to_owned(), "secondary".to_owned()],
    );
}
