mod support;

use std::collections::HashMap;

use media_core::{
    JobId, JobState, JobStore, MetricsSource, NewJob, NotifyScope, PRIMARY_USER_ID, Provider,
    SECONDARY_USER_ID,
};
use media_storage::{SeaOrmJobStore, SeaOrmMetricsSource};
use support::{TestDatabase, operation_key};

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

async fn insert_notification(database: &sea_orm::DatabaseConnection, recipient: &str, dead: bool) {
    let dead_at = if dead { "now()" } else { "NULL" };
    sea_orm::ConnectionTrait::execute_unprepared(
        database,
        &format!(
            "INSERT INTO notification_outbox \
             (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, \
              payload, dead_at) \
             VALUES (gen_random_uuid(), 'tracking', gen_random_uuid(), 'started', '{recipient}', \
              decode(md5('{recipient}'), 'hex'), '{{\"message\": \"hi\"}}'::jsonb, {dead_at})",
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn snapshot_reports_job_state_counts_and_notification_outbox_totals() {
    let test_db = TestDatabase::start_migrated().await;
    let jobs = SeaOrmJobStore::new(test_db.connection().clone());
    let metrics = SeaOrmMetricsSource::new(test_db.connection().clone());

    let empty = metrics.snapshot().await.unwrap();
    assert!(empty.jobs_by_state.is_empty());
    assert_eq!(empty.notifications_pending, 0);
    assert_eq!(empty.notifications_dead, 0);

    let completed = jobs
        .create(
            operation_key(),
            new_job(PRIMARY_USER_ID, Provider::Rezka, "a"),
        )
        .await
        .unwrap();
    jobs.create(
        operation_key(),
        new_job(SECONDARY_USER_ID, Provider::Prowlarr, "b"),
    )
    .await
    .unwrap();
    sea_orm::ConnectionTrait::execute_unprepared(
        test_db.connection(),
        &format!(
            "UPDATE jobs SET state = 'completed', completed_at = now() WHERE id = '{}'",
            completed.id().into_uuid()
        ),
    )
    .await
    .unwrap();

    insert_notification(test_db.connection(), "primary", false).await;
    insert_notification(test_db.connection(), "secondary", true).await;

    let snapshot = metrics.snapshot().await.unwrap();
    let counts: HashMap<JobState, u64> = snapshot.jobs_by_state.into_iter().collect();
    assert_eq!(counts.get(&JobState::Queued), Some(&1));
    assert_eq!(counts.get(&JobState::Completed), Some(&1));
    assert_eq!(counts.len(), 2, "only states with rows are returned");
    assert_eq!(snapshot.notifications_pending, 1);
    assert_eq!(snapshot.notifications_dead, 1);
}
