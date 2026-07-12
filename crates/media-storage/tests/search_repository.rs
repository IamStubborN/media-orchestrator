mod support;

use media_core::{PRIMARY_USER_ID, SECONDARY_USER_ID};
use media_storage::{SeaOrmSearchRepository, SearchSessionRecord};
use support::TestDatabase;

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
