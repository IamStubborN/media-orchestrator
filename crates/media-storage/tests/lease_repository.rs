mod support;

use std::sync::Arc;

use media_core::{
    PRIMARY_USER_ID, BootstrapClient, ClientId, ClientRole, ClientStore, CredentialDigest, JobId,
    JobState, JobStore, LeaseStore, NewJob, NotifyScope, PortError, Provider, RUNNER_CLIENT_ID,
};
use media_storage::{SeaOrmClientStore, SeaOrmJobStore, SeaOrmLeaseStore};
use sea_orm::{ConnectionTrait, Statement};
use support::{TestDatabase, query};
use tokio::sync::Barrier;

async fn setup() -> (TestDatabase, SeaOrmJobStore, SeaOrmLeaseStore) {
    let test_db = TestDatabase::start_migrated().await;
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
    jobs.create(new_job("race-job")).await.unwrap();
    let second = SeaOrmLeaseStore::new(test_db.connect().await);
    let barrier = Arc::new(Barrier::new(2));
    let lease = |store: SeaOrmLeaseStore, barrier: Arc<Barrier>| async move {
        barrier.wait().await;
        store
            .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(60))
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

#[tokio::test]
async fn storage_rejects_ttl_outside_the_application_contract() {
    let (_test_db, jobs, leases) = setup().await;
    jobs.create(new_job("ttl-job")).await.unwrap();

    assert_eq!(
        leases
            .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(29))
            .await,
        Err(PortError::Conflict),
    );
    assert_eq!(
        leases
            .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(301))
            .await,
        Err(PortError::Conflict),
    );
    assert_eq!(
        leases
            .lease_next(
                RUNNER_CLIENT_ID,
                time::Duration::seconds(300) + time::Duration::nanoseconds(1),
            )
            .await,
        Err(PortError::Conflict),
    );
    assert_eq!(jobs.queue_status().await.unwrap().queued, 1);
}

#[tokio::test]
async fn active_lease_blocks_claim_and_heartbeat_requires_exact_live_owner() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(new_job("heartbeat-job")).await.unwrap();
    let lease = leases
        .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(60))
        .await
        .unwrap()
        .unwrap();
    assert!(
        leases
            .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(60))
            .await
            .unwrap()
            .is_none()
    );

    let wrong_runner = ClientId::new();
    assert!(
        leases
            .heartbeat(lease.lease_id(), wrong_runner, time::Duration::seconds(60),)
            .await
            .unwrap()
            .is_none()
    );
    let before = time::OffsetDateTime::now_utc() + time::Duration::seconds(55);
    let renewed = leases
        .heartbeat(
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
    let created = jobs.create(new_job("recoverable-job")).await.unwrap();
    let mut lease = leases
        .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(60))
        .await
        .unwrap()
        .unwrap();
    let task_id = uuid::Uuid::new_v4();
    test_db
        .connection()
        .execute_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO job_tasks (id, job_id, ordinal, state, checkpoint) \
             VALUES ($1, $2, 0, 'running', '{\"downloaded_bytes\":4096}'::jsonb)",
            [task_id.into(), created.id().into_uuid().into()],
        ))
        .await
        .unwrap();

    for state in ["leased", "running", "publishing"] {
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
            .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(60))
            .await
            .unwrap()
            .unwrap();
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
    let created = jobs.create(new_job("cancel-job")).await.unwrap();
    let lease = leases
        .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(60))
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
            .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(60))
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
}

#[tokio::test]
async fn failed_queued_to_leased_update_rolls_back_the_inserted_lease() {
    let (test_db, jobs, leases) = setup().await;
    jobs.create(new_job("trigger-job")).await.unwrap();
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
            .lease_next(RUNNER_CLIENT_ID, time::Duration::seconds(60))
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
