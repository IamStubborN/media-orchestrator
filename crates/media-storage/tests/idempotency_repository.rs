mod support;

use media_core::{
    BootstrapClient, ClientRole, ClientStore, CredentialDigest, PRIMARY_CLIENT_ID, PRIMARY_USER_ID,
    PortError,
};
use media_storage::{
    ReservationGeneration, ReservationHandle, ReservationHandleError, ReservationRecord,
    SeaOrmClientStore, SeaOrmIdempotencyRepository, StoredResponseRecord,
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

async fn reserve_handle(
    repository: &SeaOrmIdempotencyRepository,
    key: &str,
    hash: [u8; 32],
    expires_at: time::OffsetDateTime,
) -> ReservationHandle {
    match repository
        .reserve(PRIMARY_CLIENT_ID, key, hash, expires_at)
        .await
        .unwrap()
    {
        ReservationRecord::Reserved(handle) => handle,
        other => panic!("unexpected reservation: {other:?}"),
    }
}

#[tokio::test]
async fn reservation_exposes_generation_scoped_lifecycle_handle() {
    let (_test_db, repository) = repository().await;
    let handle = reserve_handle(&repository, "generation-handle", HASH_A, future_expiry()).await;

    assert_eq!(handle.client_id(), PRIMARY_CLIENT_ID);
    assert_eq!(handle.key(), "generation-handle");
    assert_eq!(handle.request_hash(), &HASH_A);
    assert_ne!(handle.generation().as_uuid(), &uuid::Uuid::nil());
    repository.abort_in_progress(&handle).await.unwrap();
}

#[test]
fn reservation_handle_rehydrates_exact_typed_parts_and_rejects_invalid_identity() {
    let generation_uuid = uuid::Uuid::new_v4();
    let generation = ReservationGeneration::from_uuid(generation_uuid);
    let handle = ReservationHandle::rehydrate(
        PRIMARY_CLIENT_ID,
        "rehydrated".to_owned(),
        HASH_A,
        generation,
    )
    .unwrap();

    assert_eq!(handle.client_id(), PRIMARY_CLIENT_ID);
    assert_eq!(handle.key(), "rehydrated");
    assert_eq!(handle.request_hash(), &HASH_A);
    assert_eq!(handle.generation().as_uuid(), &generation_uuid);
    assert_eq!(
        ReservationHandle::rehydrate(PRIMARY_CLIENT_ID, String::new(), HASH_A, generation),
        Err(ReservationHandleError::EmptyKey)
    );
    assert_eq!(
        ReservationHandle::rehydrate(
            PRIMARY_CLIENT_ID,
            "nil-generation".to_owned(),
            HASH_A,
            ReservationGeneration::from_uuid(uuid::Uuid::nil()),
        ),
        Err(ReservationHandleError::NilGeneration)
    );
}

#[test]
fn stored_response_record_exposes_the_exact_storage_contract() {
    let response = StoredResponseRecord {
        status: 202,
        content_type: "application/json".to_owned(),
        body: br#"{"accepted":true}"#.to_vec(),
    };

    assert_eq!(response.status, 202);
    assert_eq!(response.content_type, "application/json");
    assert_eq!(response.body, br#"{"accepted":true}"#);
    let debug = format!("{response:?}");
    assert!(!debug.contains("application/json"));
    assert!(!debug.contains("accepted"));
}

#[tokio::test]
async fn completed_same_hash_replays_exact_response_and_changed_hash_conflicts() {
    let (_test_db, repository) = repository().await;
    let handle = reserve_handle(&repository, "create-1", HASH_A, future_expiry()).await;
    let response = StoredResponseRecord::new(
        201,
        "application/json; charset=utf-8".to_owned(),
        br#"{"job_id":"one"}"#.to_vec(),
    )
    .unwrap();
    repository
        .complete(&handle, response.clone())
        .await
        .unwrap();

    match repository
        .reserve(PRIMARY_CLIENT_ID, "create-1", HASH_A, future_expiry())
        .await
        .unwrap()
    {
        ReservationRecord::Replay {
            handle: replay_handle,
            response: replayed,
        } => {
            assert_eq!(replay_handle, handle);
            assert_eq!(replayed, response);
        }
        other => panic!("unexpected replay: {other:?}"),
    }
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "create-1", HASH_B, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Conflict,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_exact_completions_are_idempotent_but_different_responses_conflict() {
    use std::sync::Arc;

    let (test_db, first) = repository().await;
    let second = SeaOrmIdempotencyRepository::new(test_db.connect().await);
    let handle = Arc::new(reserve_handle(&first, "complete-race", HASH_A, future_expiry()).await);
    let response = StoredResponseRecord::new(
        201,
        "application/json".to_owned(),
        br#"{"job_id":"stable"}"#.to_vec(),
    )
    .unwrap();

    let left_handle = handle.clone();
    let left_response = response.clone();
    let left = tokio::spawn(async move { first.complete(&left_handle, left_response).await });
    let right_handle = handle.clone();
    let right_response = response.clone();
    let right = tokio::spawn(async move { second.complete(&right_handle, right_response).await });

    assert_eq!(left.await.unwrap(), Ok(()));
    assert_eq!(right.await.unwrap(), Ok(()));

    let repository = SeaOrmIdempotencyRepository::new(test_db.connect().await);
    assert_eq!(
        repository
            .complete(
                &handle,
                StoredResponseRecord::new(
                    202,
                    "application/json".to_owned(),
                    br#"{"job_id":"different"}"#.to_vec(),
                )
                .unwrap(),
            )
            .await,
        Err(PortError::Conflict),
    );
    assert!(matches!(
        repository
            .reserve(
                PRIMARY_CLIENT_ID,
                "complete-race",
                HASH_A,
                future_expiry(),
            )
            .await
            .unwrap(),
        ReservationRecord::Replay { response: replayed, .. } if replayed == response
    ));
}

#[tokio::test]
async fn no_content_response_round_trips_with_absent_content_type_representation() {
    let (_test_db, repository) = repository().await;
    let handle = reserve_handle(&repository, "empty-204", HASH_A, future_expiry()).await;
    let response = StoredResponseRecord::new(204, String::new(), Vec::new()).unwrap();

    repository
        .complete(&handle, response.clone())
        .await
        .unwrap();
    assert!(matches!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "empty-204", HASH_A, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Replay { response: replayed, .. } if replayed == response
    ));
}

#[tokio::test]
async fn committed_complete_survives_error_recovery_abort_and_requires_completed_discard() {
    let (_test_db, repository) = repository().await;
    let response = StoredResponseRecord::new(201, "application/json".to_owned(), vec![]).unwrap();
    let handle = reserve_handle(&repository, "discard", HASH_A, future_expiry()).await;
    repository
        .complete(&handle, response.clone())
        .await
        .unwrap();

    assert_eq!(
        repository.abort_in_progress(&handle).await,
        Err(PortError::Conflict),
        "a lost complete acknowledgement must not let recovery delete committed replay",
    );
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "discard", HASH_B, future_expiry())
            .await,
        Ok(ReservationRecord::Conflict),
    );

    let replay_handle = match repository
        .reserve(PRIMARY_CLIENT_ID, "discard", HASH_A, future_expiry())
        .await
        .unwrap()
    {
        ReservationRecord::Replay {
            handle,
            response: replayed,
        } => {
            assert_eq!(replayed, response);
            handle
        }
        other => panic!("unexpected replay: {other:?}"),
    };
    repository.discard_completed(&replay_handle).await.unwrap();
    reserve_handle(&repository, "discard", HASH_A, future_expiry()).await;
}

#[tokio::test]
async fn live_reservation_is_in_progress_abort_retries_and_expiry_replaces() {
    let (_test_db, repository) = repository().await;
    let live = reserve_handle(&repository, "live", HASH_A, future_expiry()).await;
    assert!(matches!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "live", HASH_A, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::InProgress(handle) if handle == live
    ));
    assert_eq!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "live", HASH_B, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Conflict,
        "a changed request cannot take over a live in-progress reservation",
    );
    repository.abort_in_progress(&live).await.unwrap();
    reserve_handle(&repository, "live", HASH_B, future_expiry()).await;

    let expired = reserve_handle(
        &repository,
        "expired",
        HASH_A,
        time::OffsetDateTime::now_utc() - time::Duration::seconds(1),
    )
    .await;
    let replacement = reserve_handle(&repository, "expired", HASH_A, future_expiry()).await;
    assert_ne!(expired.generation(), replacement.generation());
    assert_eq!(
        repository.abort_in_progress(&expired).await,
        Err(PortError::Conflict),
        "a stale delayed abort must not delete a replacement generation",
    );
    let response = StoredResponseRecord::new(204, String::new(), Vec::new()).unwrap();
    repository
        .complete(&replacement, response.clone())
        .await
        .unwrap();
    assert_eq!(
        repository.discard_completed(&expired).await,
        Err(PortError::Conflict),
        "a stale delayed discard must not delete a completed replacement generation",
    );
    assert!(matches!(
        repository
            .reserve(PRIMARY_CLIENT_ID, "expired", HASH_A, future_expiry())
            .await
            .unwrap(),
        ReservationRecord::Replay { response: replayed, .. } if replayed == response
    ));
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
            (
                ReservationRecord::Reserved(_),
                ReservationRecord::InProgress(_)
            ) | (
                ReservationRecord::InProgress(_),
                ReservationRecord::Reserved(_)
            )
        ),
        "exactly one request must own the reservation",
    );
}

#[tokio::test]
async fn semantic_misses_are_conflicts_but_database_failures_remain_infrastructure_errors() {
    let (test_db, repository) = repository().await;
    let response = StoredResponseRecord::new(201, "application/json".to_owned(), vec![]).unwrap();
    let stale = reserve_handle(&repository, "missing", HASH_A, future_expiry()).await;
    repository.abort_in_progress(&stale).await.unwrap();

    assert_eq!(
        repository.complete(&stale, response.clone()).await,
        Err(PortError::Conflict),
    );
    assert_eq!(
        repository.abort_in_progress(&stale).await,
        Err(PortError::Conflict),
    );
    assert_eq!(
        repository.discard_completed(&stale).await,
        Err(PortError::Conflict),
    );

    sea_orm::ConnectionTrait::execute_unprepared(
        test_db.connection(),
        "DROP TABLE idempotency_records",
    )
    .await
    .unwrap();
    assert_eq!(
        repository.complete(&stale, response).await,
        Err(PortError::Infrastructure),
    );
    assert_eq!(
        repository.abort_in_progress(&stale).await,
        Err(PortError::Infrastructure),
    );
    assert_eq!(
        repository.discard_completed(&stale).await,
        Err(PortError::Infrastructure),
    );
}
