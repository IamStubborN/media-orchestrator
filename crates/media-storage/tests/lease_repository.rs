mod support;

use std::sync::Arc;

use media_core::{
    PRIMARY_USER_ID, BootstrapClient, ClientId, ClientRole, ClientStore, CredentialDigest, JobEvent,
    JobEventId, JobId, JobState, JobStore, LeaseStore, NewJob, NotifyScope, PortError, Provider,
    RUNNER_CLIENT_ID,
};
use media_storage::{SeaOrmClientStore, SeaOrmJobStore, SeaOrmLeaseStore};
use sea_orm::{ConnectionTrait, Statement, TransactionTrait};
use support::{TestDatabase, operation_key, query};
use tokio::sync::Barrier;

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
                CredentialDigest::from([0x55; 32]),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let jobs = SeaOrmJobStore::new(test_db.connection().clone());
    let leases = SeaOrmLeaseStore::new(test_db.connection().clone());
    (test_db, jobs, leases)
}

#[tokio::test]
async fn non_ready_lifecycle_denies_new_leases_without_mutating_the_job() {
    let (test_db, jobs, leases) = setup().await;
    let job = jobs
        .create(operation_key(), new_job("selection:lifecycle-gate"))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE runner_lifecycle SET state = 'blocked', reason = 'vpn_rotation_failed' WHERE singleton = true",
        )
        .await
        .unwrap();

    assert!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60)
            )
            .await
            .unwrap()
            .is_none()
    );
    let stored = jobs
        .find_for_owner(job.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state(), JobState::Queued);
}

async fn return_job_to_queue(test_db: &TestDatabase, job_id: JobId) {
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "DELETE FROM job_leases WHERE job_id = $1",
            [job_id.into_uuid().into()],
        ))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE jobs SET state = 'queued', updated_at = now() WHERE id = $1",
            [job_id.into_uuid().into()],
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn lease_exposes_completed_task_ordinals_for_episode_resume() {
    let (test_db, jobs, leases) = setup().await;
    let job = jobs
        .create(operation_key(), new_job("selection:completed-task-resume"))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE job_tasks SET state = 'completed', completed_at = now() \
             WHERE job_id = $1 AND ordinal = 0",
            [job.id().into_uuid().into()],
        ))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO job_tasks (id, job_id, ordinal, state, completed_at) \
             VALUES (gen_random_uuid(), $1, 1, 'completed', now()), \
                    (gen_random_uuid(), $1, 2, 'pending', NULL)",
            [job.id().into_uuid().into()],
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

    assert_eq!(lease.completed_task_ordinals(), &[0, 1]);
}

#[tokio::test]
async fn same_job_gets_three_leases_on_one_vpn_session_then_requires_rotation() {
    let (test_db, jobs, leases) = setup().await;
    let job = jobs
        .create(operation_key(), new_job("selection:sticky-three"))
        .await
        .unwrap();

    for expected_attempt in 1..=3_i32 {
        let lease = leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
            .unwrap()
            .expect("the same job may retry on the sticky VPN session");
        assert_eq!(lease.job().id(), job.id());
        let row = query(
            test_db.connection(),
            "SELECT sticky_job_id, sticky_attempt_count FROM runner_lifecycle",
        )
        .await
        .pop()
        .unwrap();
        assert_eq!(
            row.try_get::<uuid::Uuid>("", "sticky_job_id").unwrap(),
            job.id().into_uuid(),
        );
        assert_eq!(
            row.try_get::<i32>("", "sticky_attempt_count").unwrap(),
            expected_attempt,
        );
        return_job_to_queue(&test_db, job.id()).await;
    }

    assert_eq!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await,
        Err(PortError::Conflict),
    );
    let lifecycle = query(
        test_db.connection(),
        "SELECT state, sticky_attempt_count FROM runner_lifecycle",
    )
    .await
    .pop()
    .unwrap();
    assert_eq!(
        lifecycle.try_get::<String>("", "state").unwrap(),
        "rotating"
    );
    assert_eq!(
        lifecycle
            .try_get::<i32>("", "sticky_attempt_count")
            .unwrap(),
        3,
    );
    assert_eq!(jobs.queue_status().await.unwrap().queued, 1);
    assert_eq!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await,
        Err(PortError::Conflict),
        "a lost rotation response must remain durable for the next request",
    );
}

#[tokio::test]
async fn switching_to_another_job_requires_rotation_immediately() {
    let (test_db, jobs, leases) = setup().await;
    let first = jobs
        .create(operation_key(), new_job("selection:first-vpn-job"))
        .await
        .unwrap();
    let second = jobs
        .create(operation_key(), new_job("selection:second-vpn-job"))
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
    assert_eq!(lease.job().id(), first.id());
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "DELETE FROM job_leases WHERE job_id = $1",
            [first.id().into_uuid().into()],
        ))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE jobs SET state = 'failed', completed_at = now(), updated_at = now() \
             WHERE id = $1",
            [first.id().into_uuid().into()],
        ))
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
        Err(PortError::Conflict),
    );
    let stored = jobs
        .find_for_owner(second.id(), PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state(), JobState::Queued);
    let lifecycle = query(test_db.connection(), "SELECT state FROM runner_lifecycle")
        .await
        .pop()
        .unwrap();
    assert_eq!(
        lifecycle.try_get::<String>("", "state").unwrap(),
        "rotating"
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_connections_create_exactly_one_active_lease() {
    let (test_db, jobs, first) = setup().await;
    jobs.create(operation_key(), new_job("race-job"))
        .await
        .unwrap();
    let second = SeaOrmLeaseStore::new(test_db.connect().await);
    let barrier = Arc::new(Barrier::new(2));
    let lease = |store: SeaOrmLeaseStore, barrier: Arc<Barrier>| async move {
        barrier.wait().await;
        store
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
            .unwrap()
    };

    let (left, right) = tokio::join!(lease(first, barrier.clone()), lease(second, barrier));
    assert_eq!(
        usize::from(left.is_some()) + usize::from(right.is_some()),
        1
    );
    assert_eq!(
        query(test_db.connection(), "SELECT id FROM job_leases")
            .await
            .len(),
        1
    );
    assert_eq!(jobs.queue_status().await.unwrap().queued, 0);
    assert!(jobs.queue_status().await.unwrap().active);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contender_claims_queued_job_after_advisory_lock_holder_rolls_back() {
    const LEASE_ADVISORY_LOCK: i64 = 0x4d45_4449_414c_5345;

    let (test_db, jobs, _leases) = setup().await;
    jobs.create(operation_key(), new_job("rollback-race-job"))
        .await
        .unwrap();
    let holder_connection = test_db.connect().await;
    let holder = holder_connection.begin().await.unwrap();
    holder
        .query_one_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [LEASE_ADVISORY_LOCK.into()],
        ))
        .await
        .unwrap();

    let contender_store = SeaOrmLeaseStore::new(test_db.connect().await);
    let contender = tokio::spawn(async move {
        contender_store
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
            .unwrap()
    });

    let mut observed_waiter = false;
    for _ in 0..50 {
        if contender.is_finished() {
            break;
        }
        let row = test_db
            .connection()
            .query_one_raw(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_locks \
                 WHERE locktype = 'advisory' AND NOT granted) AS waiting",
            ))
            .await
            .unwrap()
            .unwrap();
        if row.try_get::<bool>("", "waiting").unwrap() {
            observed_waiter = true;
            break;
        }
        tokio::task::yield_now().await;
    }

    assert!(
        observed_waiter,
        "the contender must wait for transaction-scoped serialization",
    );
    holder.rollback().await.unwrap();
    let lease = contender.await.unwrap();
    assert!(
        lease.is_some(),
        "a rolled-back lock holder must not cause a false empty-queue result",
    );
}

#[tokio::test]
async fn storage_rejects_ttl_outside_the_application_contract() {
    let (_test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("ttl-job"))
        .await
        .unwrap();

    let lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(30),
        )
        .await
        .unwrap()
        .expect("the inclusive 30 second minimum must be accepted");
    assert!(
        leases
            .heartbeat(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(300),
            )
            .await
            .unwrap()
            .is_some(),
        "the inclusive 300 second maximum must be accepted",
    );

    assert_eq!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(29),
            )
            .await,
        Err(PortError::Conflict),
    );
    assert_eq!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(301),
            )
            .await,
        Err(PortError::Conflict),
    );
    assert_eq!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(300) + time::Duration::nanoseconds(1),
            )
            .await,
        Err(PortError::Conflict),
    );
    assert_eq!(jobs.queue_status().await.unwrap().queued, 0);
}

#[tokio::test]
async fn active_lease_blocks_claim_and_heartbeat_requires_exact_live_owner() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("heartbeat-job"))
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

    let wrong_runner = ClientId::new();
    assert!(
        leases
            .heartbeat(
                operation_key(),
                lease.lease_id(),
                wrong_runner,
                time::Duration::seconds(60),
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        leases
            .heartbeat(
                operation_key(),
                media_core::LeaseId::new(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
            .unwrap()
            .is_none(),
        "a valid runner cannot renew a different lease ID",
    );
    let before = time::OffsetDateTime::now_utc() + time::Duration::seconds(55);
    let renewed = leases
        .heartbeat(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(renewed.expires_at() > before);

    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE job_leases SET created_at = now() - interval '2 seconds', \
             expires_at = now() - interval '1 second' WHERE id = $1",
            [lease.lease_id().into_uuid().into()],
        ))
        .await
        .unwrap();
    assert!(
        leases
            .heartbeat(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn expired_leases_requeue_recoverable_states_and_preserve_checkpoints() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("recoverable-job"))
        .await
        .unwrap();
    let mut lease = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE job_tasks SET state = 'running', \
             checkpoint = '{\"downloaded_bytes\":4096}'::jsonb \
             WHERE job_id = $1 AND ordinal = 0",
            [created.id().into_uuid().into()],
        ))
        .await
        .unwrap();

    for (index, state) in ["leased", "running", "publishing"].into_iter().enumerate() {
        test_db
            .connection()
            .execute_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE jobs SET state = $1 WHERE id = $2",
                [state.into(), created.id().into_uuid().into()],
            ))
            .await
            .unwrap();
        test_db
            .connection()
            .execute_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE job_leases SET created_at = now() - interval '2 seconds', \
                 expires_at = now() - interval '1 second' WHERE id = $1",
                [lease.lease_id().into_uuid().into()],
            ))
            .await
            .unwrap();
        let recovered = leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await;
        let recovered = if index == 2 {
            assert_eq!(recovered, Err(PortError::Conflict));
            test_db
                .connection()
                .execute_unprepared(
                    "UPDATE runner_lifecycle SET state = 'ready', reason = NULL, \
                     sticky_job_id = NULL, sticky_attempt_count = 0 \
                     WHERE singleton = true",
                )
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
        } else {
            recovered.unwrap().unwrap()
        };
        assert_eq!(recovered.job().id(), created.id());
        assert_eq!(recovered.job().state(), JobState::Leased);
        lease = recovered;
    }

    let checkpoint = query(
        test_db.connection(),
        "SELECT checkpoint FROM job_tasks WHERE ordinal = 0",
    )
    .await;
    assert_eq!(
        checkpoint[0]
            .try_get::<serde_json::Value>("", "checkpoint")
            .unwrap(),
        serde_json::json!({"downloaded_bytes": 4096}),
    );
}

#[tokio::test]
async fn expired_cancel_requested_job_becomes_cancelled_instead_of_being_stranded() {
    let (test_db, jobs, leases) = setup().await;
    let created = jobs
        .create(operation_key(), new_job("cancel-job"))
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
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE jobs SET state = 'cancel_requested' WHERE id = $1",
            [created.id().into_uuid().into()],
        ))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE job_leases SET created_at = now() - interval '2 seconds', \
             expires_at = now() - interval '1 second' WHERE id = $1",
            [lease.lease_id().into_uuid().into()],
        ))
        .await
        .unwrap();

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
    let state = query(
        test_db.connection(),
        "SELECT state FROM jobs WHERE result_ref = 'cancel-job'",
    )
    .await;
    assert_eq!(
        state[0].try_get::<String>("", "state").unwrap(),
        "cancelled"
    );
    assert!(
        query(test_db.connection(), "SELECT id FROM job_leases")
            .await
            .is_empty()
    );
    let notifications = query(
        test_db.connection(),
        "SELECT event_type, payload FROM notification_outbox \
         WHERE aggregate_id = (SELECT id FROM jobs WHERE result_ref = 'cancel-job')",
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert!(notifications.iter().all(|row| {
        let payload = row.try_get::<serde_json::Value>("", "payload").unwrap();
        row.try_get::<String>("", "event_type").unwrap() == "cancelled"
            && payload.get("state").and_then(serde_json::Value::as_str) == Some("cancelled")
            && payload["delivery_kind"] == "card"
    }));
    let outbox = query(
        test_db.connection(),
        "SELECT event_type, payload FROM outbox_events \
         WHERE aggregate_id = (SELECT id FROM jobs WHERE result_ref = 'cancel-job') \
         AND event_type = 'job.cancelled'",
    )
    .await;
    assert_eq!(outbox.len(), 1);
    assert_eq!(
        outbox[0]
            .try_get::<serde_json::Value>("", "payload")
            .unwrap(),
        serde_json::json!({"state": "cancelled"}),
    );
}

#[tokio::test]
async fn failed_queued_to_leased_update_rolls_back_the_inserted_lease() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("trigger-job"))
        .await
        .unwrap();
    test_db
        .connection()
        .execute_unprepared(
            "CREATE FUNCTION suppress_leased_update() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN IF NEW.state = 'leased' THEN RETURN NULL; END IF; RETURN NEW; END $$; \
             CREATE TRIGGER suppress_leased_update BEFORE UPDATE ON jobs \
             FOR EACH ROW EXECUTE FUNCTION suppress_leased_update()",
        )
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
        Err(PortError::Infrastructure),
    );
    assert!(
        query(test_db.connection(), "SELECT id FROM job_leases")
            .await
            .is_empty()
    );
    assert_eq!(jobs.queue_status().await.unwrap().queued, 1);
}

#[tokio::test]
async fn oversized_persisted_job_is_rejected_before_lease_mutations_commit() {
    let (test_db, jobs, leases) = setup().await;
    test_db
        .connection()
        .execute_unprepared(
            "ALTER TABLE jobs DROP CONSTRAINT jobs_result_ref_length_check; \
             INSERT INTO jobs (id, owner_id, provider, result_ref, state, notify_scope) \
             VALUES ('00000000-0000-0040-0000-000000000001', \
             '00000000-0000-0000-0000-000000000001', 'rezka', repeat('x', 65537), \
             'queued', 'initiator')",
        )
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
        Err(PortError::Infrastructure),
    );
    assert!(
        query(test_db.connection(), "SELECT id FROM job_leases")
            .await
            .is_empty()
    );
    let row = query(
        test_db.connection(),
        "SELECT state, attempt_count FROM jobs \
         WHERE id = '00000000-0000-0040-0000-000000000001'",
    )
    .await;
    assert_eq!(row[0].try_get::<String>("", "state").unwrap(), "queued");
    assert_eq!(row[0].try_get::<i32>("", "attempt_count").unwrap(), 0);
    assert_eq!(jobs.queue_status().await.unwrap().queued, 1);
}

#[tokio::test]
async fn oversized_persisted_job_rolls_back_heartbeat_extension() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("oversized-heartbeat"))
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
    test_db
        .connection()
        .execute_unprepared(
            "ALTER TABLE jobs DROP CONSTRAINT jobs_result_ref_length_check; \
             UPDATE jobs SET result_ref = repeat('x', 65537) \
             WHERE result_ref = 'oversized-heartbeat'",
        )
        .await
        .unwrap();
    let before = query(
        test_db.connection(),
        "SELECT expires_at FROM job_leases WHERE slot = 1",
    )
    .await[0]
        .try_get::<time::OffsetDateTime>("", "expires_at")
        .unwrap();

    assert_eq!(
        leases
            .heartbeat(
                operation_key(),
                lease.lease_id(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(300),
            )
            .await,
        Err(PortError::Infrastructure),
    );
    let after = query(
        test_db.connection(),
        "SELECT expires_at FROM job_leases WHERE slot = 1",
    )
    .await[0]
        .try_get::<time::OffsetDateTime>("", "expires_at")
        .unwrap();
    assert_eq!(after, before);
}

#[tokio::test]
async fn repeated_lease_operation_returns_the_original_lease_without_claiming_again() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("durable-lease-first"))
        .await
        .unwrap();
    jobs.create(operation_key(), new_job("durable-lease-second"))
        .await
        .unwrap();
    let key = operation_key();

    let first = leases
        .lease_next(key, RUNNER_CLIENT_ID, time::Duration::seconds(60))
        .await
        .unwrap()
        .unwrap();
    drop(leases);
    let replayed = SeaOrmLeaseStore::new(test_db.connect().await)
        .lease_next(key, RUNNER_CLIENT_ID, time::Duration::seconds(300))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(replayed, first);
    let states = query(
        test_db.connection(),
        "SELECT state FROM jobs ORDER BY result_ref",
    )
    .await
    .into_iter()
    .map(|row| row.try_get::<String>("", "state").unwrap())
    .collect::<Vec<_>>();
    assert_eq!(states, ["leased", "queued"]);
}

#[tokio::test]
async fn repeated_empty_lease_operation_stays_empty_after_work_arrives() {
    let (test_db, jobs, leases) = setup().await;
    let key = operation_key();

    assert!(
        leases
            .lease_next(key, RUNNER_CLIENT_ID, time::Duration::seconds(60))
            .await
            .unwrap()
            .is_none()
    );
    jobs.create(operation_key(), new_job("arrived-after-empty-result"))
        .await
        .unwrap();
    drop(leases);

    assert!(
        SeaOrmLeaseStore::new(test_db.connect().await)
            .lease_next(key, RUNNER_CLIENT_ID, time::Duration::seconds(60))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(jobs.queue_status().await.unwrap().queued, 1);
}

#[tokio::test]
async fn repeated_heartbeat_operation_returns_its_original_expiry_after_a_later_heartbeat() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("durable-heartbeat"))
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
    let key = operation_key();
    let first = leases
        .heartbeat(
            key,
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .unwrap();
    let later = leases
        .heartbeat(
            operation_key(),
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(300),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(later.expires_at() > first.expires_at());
    drop(leases);

    let replayed = SeaOrmLeaseStore::new(test_db.connect().await)
        .heartbeat(
            key,
            lease.lease_id(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(300),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replayed, first);
}

async fn start_download_stage(leases: &SeaOrmLeaseStore, lease_id: media_core::LeaseId) {
    for event in [
        JobEvent::started(JobEventId::new()),
        JobEvent::stage_started(JobEventId::new(), 0, "download".to_owned(), 0).unwrap(),
    ] {
        leases
            .report_event(operation_key(), lease_id, RUNNER_CLIENT_ID, event)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn transient_stream_failure_does_not_burn_sticky_into_rotating() {
    let (test_db, jobs, leases) = setup().await;
    let job = jobs
        .create(operation_key(), new_job("selection:transient-same-ip"))
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
        .expect("first lease");
    start_download_stage(&leases, lease.lease_id()).await;
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

    let row = query(
        test_db.connection(),
        "SELECT state, sticky_attempt_count, sticky_job_id, reason FROM runner_lifecycle",
    )
    .await
    .pop()
    .unwrap();
    assert_eq!(row.try_get::<String>("", "state").unwrap(), "ready");
    assert_eq!(row.try_get::<i32>("", "sticky_attempt_count").unwrap(), 0);
    assert!(
        row.try_get::<Option<uuid::Uuid>>("", "sticky_job_id")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        row.try_get::<Option<String>>("", "reason")
            .unwrap()
            .as_deref(),
        Some("retry_same_ip:source_transfer_transient"),
    );

    // Same job can lease again without rotating.
    let next = leases
        .lease_next(
            operation_key(),
            RUNNER_CLIENT_ID,
            time::Duration::seconds(60),
        )
        .await
        .unwrap()
        .expect("transient failure must not require rotation");
    assert_eq!(next.job().id(), job.id());
}

#[tokio::test]
async fn rezka_reject_marks_lifecycle_rotating() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(operation_key(), new_job("selection:rotate-worthy"))
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
        .expect("first lease");
    start_download_stage(&leases, lease.lease_id()).await;
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
                "rezka_provider_rejected".to_owned(),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let row = query(
        test_db.connection(),
        "SELECT state, reason FROM runner_lifecycle",
    )
    .await
    .pop()
    .unwrap();
    assert_eq!(row.try_get::<String>("", "state").unwrap(), "rotating");
    assert_eq!(
        row.try_get::<Option<String>>("", "reason")
            .unwrap()
            .as_deref(),
        Some("rotate_worthy:rezka_provider_rejected"),
    );
    assert_eq!(
        leases
            .lease_next(
                operation_key(),
                RUNNER_CLIENT_ID,
                time::Duration::seconds(60),
            )
            .await,
        Err(PortError::Conflict),
    );
}
