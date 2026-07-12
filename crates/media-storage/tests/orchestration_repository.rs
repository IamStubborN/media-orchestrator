mod support;

use media_core::{
    PRIMARY_USER_ID, BootstrapClient, CheckpointValue, ClientRole, ClientStore, CredentialDigest,
    JobEvent, JobEventId, JobId, JobState, JobStore, LeaseStore, NewJob, NotifyScope, Provider,
    RUNNER_CLIENT_ID,
};
use media_storage::{SeaOrmClientStore, SeaOrmJobStore, SeaOrmLeaseStore};
use sea_orm::{ConnectionTrait, Statement};
use support::{TestDatabase, operation_key, query};

async fn setup() -> (TestDatabase, SeaOrmJobStore, SeaOrmLeaseStore) {
    let test_db = TestDatabase::start_migrated().await;
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
    NewJob::new(
        JobId::new(),
        PRIMARY_USER_ID,
        Provider::Rezka,
        reference.to_owned(),
        NotifyScope::Initiator,
    )
    .unwrap()
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
    assert!(!payload.to_string().contains("rezka"));
    assert!(!payload.to_string().contains("http"));
}

#[tokio::test]
async fn retryable_stage_fails_terminally_on_third_attempt_and_releases_lease() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("retry-stage"))
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

    for attempt in 1..=3 {
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
                JobState::Running
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
