mod support;

use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, BootstrapClient, ClientRole, ClientStore, CredentialDigest,
    PortError,
};
use media_storage::{
    ReservationRecord, SeaOrmClientStore, SeaOrmIdempotencyRepository, StoredResponseRecord,
};
use support::TestDatabase;

const HASH_A: [u8; 32] = [0x11; 32];
const HASH_B: [u8; 32] = [0x22; 32];

async fn repository() -> (TestDatabase, SeaOrmIdempotencyRepository) {
    let test_db = TestDatabase::start_migrated().await;
    SeaOrmClientStore::new(test_db.connection().clone())
        .upsert_client(
            BootstrapClient::new(
                PRIMARY_CLIENT_ID,
                "hermes-primary".to_owned(),
                ClientRole::Hermes,
                Some(PRIMARY_USER_ID),
                CredentialDigest::from([0x44; 32]),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let repository = SeaOrmIdempotencyRepository::new(test_db.connection().clone());
    (test_db, repository)
}

fn future_expiry() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() + time::Duration::hours(24)
}

#[tokio::test]
async fn completed_same_hash_replays_exact_response_and_changed_hash_conflicts() {
    let (_test_db, repository) = repository().await;
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "create-1", HASH_A, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Reserved,
    );
    let response = StoredResponseRecord::new(
        201,
        "application/json; charset=utf-8".to_owned(),
        br#"{"job_id":"one"}"#.to_vec(),
    )
    .unwrap();
    repository
        .complete(PRIMARY_CLIENT_ID, "create-1", HASH_A, response.clone())
        .await
        .unwrap();

    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "create-1", HASH_A, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Replay(response),
    );
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "create-1", HASH_B, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Conflict,
    );
}

#[tokio::test]
async fn live_reservation_is_in_progress_abort_retries_and_expiry_replaces() {
    let (_test_db, repository) = repository().await;
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "live", HASH_A, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Reserved,
    );
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "live", HASH_A, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::InProgress,
    );
    repository
        .abort(PRIMARY_CLIENT_ID, "live", HASH_A)
        .await
        .unwrap();
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "live", HASH_B, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Reserved,
    );

    assert_eq!(
        repository
            .reserve(
                PRIMARY_CLIENT_ID,
                "expired",
                HASH_A,
                time::OffsetDateTime::now_utc() - time::Duration::seconds(1),
            )
            .await
            .unwrap(),
        ReservationRecord::Reserved,
    );
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "expired", HASH_B, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Reserved,
        "an expired key may be atomically reused with a different request",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_reservations_have_one_winner() {
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let (test_db, first) = repository().await;
    let second = SeaOrmIdempotencyRepository::new(test_db.connect().await);
    let barrier = Arc::new(Barrier::new(2));
    let reserve = |repository: SeaOrmIdempotencyRepository, barrier: Arc<Barrier>| async move {
        barrier.wait().await;
        repository
            .reserve(PRIMARY_CLIENT_ID, "race", HASH_A, future_expiry())
            .await
            .unwrap()
    };

    let (left, right) = tokio::join!(reserve(first, barrier.clone()), reserve(second, barrier));
    assert!(
        matches!(
            (left, right),
            (ReservationRecord::Reserved, ReservationRecord::InProgress)
                | (ReservationRecord::InProgress, ReservationRecord::Reserved)
        ),
        "exactly one request must own the reservation",
    );
}

#[tokio::test]
async fn semantic_misses_are_conflicts_but_database_failures_remain_infrastructure_errors() {
    let (test_db, repository) = repository().await;
    let response = StoredResponseRecord::new(201, "application/json".to_owned(), vec![]).unwrap();

    assert_eq!(
        repository
            .complete(PRIMARY_CLIENT_ID, "missing", HASH_A, response.clone(),)
            .await,
        Err(PortError::Conflict),
    );
    assert_eq!(
        repository.abort(PRIMARY_CLIENT_ID, "missing", HASH_A).await,
        Err(PortError::Conflict),
    );

    sea_orm::ConnectionTrait::execute_unprepared(
        test_db.connection(),
        "DROP TABLE idempotency_records",
    )
    .await
    .unwrap();
    assert_eq!(
        repository
            .complete(PRIMARY_CLIENT_ID, "missing", HASH_A, response)
            .await,
        Err(PortError::Infrastructure),
    );
    assert_eq!(
        repository.abort(PRIMARY_CLIENT_ID, "missing", HASH_A).await,
        Err(PortError::Infrastructure),
    );
}
