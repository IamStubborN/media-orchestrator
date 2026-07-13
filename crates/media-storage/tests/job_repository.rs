mod support;

use media_core::{
    PRIMARY_USER_ID, JobId, JobState, JobStore, NewJob, NotifyScope, Provider,
    RunnerLifecycleState, SECONDARY_USER_ID,
};
use media_storage::{SeaOrmJobStore, SeaOrmOperationReceiptRepository};
use sea_orm::ConnectionTrait;
use support::{TestDatabase, operation_key, query};

fn new_job(owner: media_core::UserId, provider: Provider, reference: &str) -> NewJob {
    NewJob::new(
        JobId::new(),
        owner,
        provider,
        reference.to_owned(),
        NotifyScope::Initiator,
    )
    .unwrap()
}

#[tokio::test]
async fn jobs_round_trip_as_domain_values_and_reads_are_owner_scoped_in_sql() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmJobStore::new(test_db.connection().clone());
    let primary_job = new_job(PRIMARY_USER_ID, Provider::Rezka, "rezka:selection:1");
    let secondary_job = new_job(SECONDARY_USER_ID, Provider::Prowlarr, "prowlarr:result:2");

    let created = store
        .create(operation_key(), primary_job.clone())
        .await
        .unwrap();
    store
        .create(operation_key(), secondary_job.clone())
        .await
        .unwrap();

    assert_eq!(created.id(), primary_job.id());
    assert_eq!(created.owner_id(), PRIMARY_USER_ID);
    assert_eq!(created.state(), JobState::Queued);
    assert_eq!(created.provider(), Provider::Rezka);
    assert_eq!(created.result_ref(), "rezka:selection:1");
    assert_eq!(
        store
            .find_for_owner(primary_job.id(), PRIMARY_USER_ID)
            .await
            .unwrap(),
        Some(created),
    );
    assert_eq!(
        store
            .find_for_owner(primary_job.id(), SECONDARY_USER_ID)
            .await
            .unwrap(),
        None,
        "a valid job ID must not bypass owner isolation",
    );
}

#[tokio::test]
async fn queue_status_counts_only_queued_jobs_and_reports_a_live_lease() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmJobStore::new(test_db.connection().clone());
    let first = store
        .create(
            operation_key(),
            new_job(PRIMARY_USER_ID, Provider::Rezka, "first"),
        )
        .await
        .unwrap();
    store
        .create(
            operation_key(),
            new_job(SECONDARY_USER_ID, Provider::Prowlarr, "second"),
        )
        .await
        .unwrap();

    assert_eq!(store.queue_status().await.unwrap().queued, 2);
    sea_orm::ConnectionTrait::execute_unprepared(
        test_db.connection(),
        &format!(
            "UPDATE jobs SET state = 'completed', completed_at = now() WHERE id = '{}'",
            first.id().into_uuid()
        ),
    )
    .await
    .unwrap();

    let status = store.queue_status().await.unwrap();
    assert_eq!(status.queued, 1);
    assert!(!status.active);
}

#[tokio::test]
async fn queue_status_exposes_the_durable_runner_blocking_reason() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmJobStore::new(test_db.connection().clone());
    test_db
        .connection()
        .execute_unprepared(
            "UPDATE runner_lifecycle SET state = 'blocked', reason = 'vpn_rotation_failed' \
             WHERE singleton = true",
        )
        .await
        .unwrap();

    let status = store.queue_status().await.unwrap();

    assert_eq!(status.runner_state, RunnerLifecycleState::Blocked);
    assert_eq!(status.blocked_reason.as_deref(), Some("vpn_rotation_failed"));
}

#[tokio::test]
async fn repeated_operation_key_returns_the_original_job_without_a_second_insert() {
    let test_db = TestDatabase::start_migrated().await;
    let first_store = SeaOrmJobStore::new(test_db.connection().clone());
    let key = operation_key();
    let first_input = new_job(PRIMARY_USER_ID, Provider::Rezka, "durable-create");

    let first = first_store.create(key, first_input).await.unwrap();
    drop(first_store);

    let second_input = new_job(PRIMARY_USER_ID, Provider::Rezka, "durable-create");
    let replayed = SeaOrmJobStore::new(test_db.connect().await)
        .create(key, second_input)
        .await
        .unwrap();

    assert_eq!(replayed, first);
    assert_eq!(
        query(test_db.connection(), "SELECT id FROM jobs")
            .await
            .len(),
        1,
    );
}

#[tokio::test]
async fn operation_completion_query_reports_only_completed_receipts() {
    let test_db = TestDatabase::start_migrated().await;
    let jobs = SeaOrmJobStore::new(test_db.connection().clone());
    let receipts = SeaOrmOperationReceiptRepository::new(test_db.connection().clone());
    let missing = operation_key();
    assert!(!receipts.is_completed(missing).await.unwrap());

    let pending = operation_key();
    test_db
        .connection()
        .execute_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO operation_receipts \
             (id, operation_key, operation_kind, result_kind) \
             VALUES ($1, $2, 'create_job', 'pending')",
            [
                uuid::Uuid::new_v4().into(),
                pending.as_bytes().to_vec().into(),
            ],
        ))
        .await
        .unwrap();
    assert!(!receipts.is_completed(pending).await.unwrap());

    let completed = operation_key();
    jobs.create(
        completed,
        new_job(PRIMARY_USER_ID, Provider::Rezka, "completion-query"),
    )
    .await
    .unwrap();
    assert!(receipts.is_completed(completed).await.unwrap());
}
