mod support;

use media_core::{
    JobId, JobStore, NewJob, NotifyScope, PRIMARY_USER_ID, Provider, SECONDARY_USER_ID,
};
use media_storage::{SeaOrmJobStore, SeaOrmSearchRepository, SearchSessionRecord};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
use support::{TestDatabase, operation_key};

#[tokio::test]
async fn search_sessions_are_owner_scoped_and_execution_payloads_round_trip_separately() {
    let database = TestDatabase::start_migrated().await;
    let repository = SeaOrmSearchRepository::new(database.connection().clone());
    let id = uuid::Uuid::new_v4();
    let expires_at = time::OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
    let payload = serde_json::json!({
        "request": {"source":"prowlarr","query":"Movie"},
        "results": [{"public":{"title":"Movie"},"private":{"uri":"magnet:?secret"}}]
    });

    repository
        .insert_session(SearchSessionRecord {
            id,
            owner: PRIMARY_USER_ID,
            payload: payload.clone(),
            expires_at,
        })
        .await
        .unwrap();

    assert_eq!(
        repository
            .session_for_owner(id, SECONDARY_USER_ID)
            .await
            .unwrap(),
        None
    );
    let stored = repository
        .session_for_owner(id, PRIMARY_USER_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.payload, payload);
    assert_eq!(stored.expires_at, expires_at);
    assert!(!format!("{stored:?}").contains("magnet:?secret"));

    let execution = serde_json::json!({
        "source":"prowlarr",
        "info_hash":"0123456789abcdef0123456789abcdef01234567",
        "uri":"magnet:?secret"
    });
    repository
        .insert_execution("selection:repository-test", execution.clone())
        .await
        .unwrap();
    assert_eq!(
        repository
            .execution_for("selection:repository-test")
            .await
            .unwrap(),
        Some(execution)
    );
    assert_eq!(
        repository.execution_for("selection:missing").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn execution_is_replaceable_until_job_creation_and_fenced_afterward() {
    let database = TestDatabase::start_migrated().await;
    let repository = SeaOrmSearchRepository::new(database.connection().clone());
    let jobs = SeaOrmJobStore::new(database.connection().clone());
    let result_ref = "selection:tracking:replaceable";
    let old = serde_json::json!({"source":"rezka","translation_id":1});
    let patched = serde_json::json!({"source":"rezka","translation_id":2});
    let too_late = serde_json::json!({"source":"rezka","translation_id":3});

    repository.insert_execution(result_ref, old).await.unwrap();
    repository
        .insert_execution(result_ref, patched.clone())
        .await
        .unwrap();
    assert_eq!(
        repository.execution_for(result_ref).await.unwrap(),
        Some(patched.clone()),
        "a failed create_job attempt must not pin the pre-patch execution"
    );

    jobs.create(
        operation_key(),
        NewJob::new(
            JobId::new(),
            PRIMARY_USER_ID,
            Provider::Rezka,
            result_ref.to_owned(),
            NotifyScope::Initiator,
        )
        .unwrap(),
    )
    .await
    .unwrap();
    repository
        .insert_execution(result_ref, too_late)
        .await
        .unwrap();

    assert_eq!(
        repository.execution_for(result_ref).await.unwrap(),
        Some(patched),
        "an execution referenced by a durable job must remain immutable"
    );
}

#[tokio::test]
async fn concurrent_job_commit_fences_an_execution_update_started_before_commit() {
    let database = TestDatabase::start_migrated().await;
    let repository = SeaOrmSearchRepository::new(database.connection().clone());
    let result_ref = "selection:tracking:concurrent-fence";
    let original = serde_json::json!({"source":"rezka","translation_id":1});
    let replacement = serde_json::json!({"source":"rezka","translation_id":2});
    repository
        .insert_execution(result_ref, original.clone())
        .await
        .unwrap();

    let job_connection = database.connect().await;
    let job_transaction = job_connection.begin().await.unwrap();
    job_transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO jobs (id, owner_id, provider, result_ref, state, notify_scope) \
             VALUES ($1, $2, 'rezka', $3, 'queued', 'initiator')",
            [
                JobId::new().into_uuid().into(),
                PRIMARY_USER_ID.into_uuid().into(),
                result_ref.into(),
            ],
        ))
        .await
        .unwrap();

    let concurrent_repository = repository.clone();
    let update = tokio::spawn(async move {
        concurrent_repository
            .insert_execution(result_ref, replacement)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    job_transaction.commit().await.unwrap();
    update.await.unwrap().unwrap();

    assert_eq!(
        repository.execution_for(result_ref).await.unwrap(),
        Some(original),
        "a job that wins the commit race must make its execution immutable"
    );
}
