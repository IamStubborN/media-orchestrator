mod support;

use media_core::{
    PRIMARY_USER_ID, JobId, JobState, JobStore, NewJob, NotifyScope, Provider, SECONDARY_USER_ID,
};
use media_storage::SeaOrmJobStore;
use support::TestDatabase;

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

    let created = store.create(primary_job.clone()).await.unwrap();
    store.create(secondary_job.clone()).await.unwrap();

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
        .create(new_job(PRIMARY_USER_ID, Provider::Rezka, "first"))
        .await
        .unwrap();
    store
        .create(new_job(SECONDARY_USER_ID, Provider::Prowlarr, "second"))
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
