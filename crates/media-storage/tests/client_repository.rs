mod support;

use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, BootstrapClient, ClientRole, ClientStore, CredentialDigest,
    PortError, RUNNER_CLIENT_ID, SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
use media_storage::{Migrator, SeaOrmClientStore};
use sea_orm_migration::MigratorTrait;
use support::{TestDatabase, execute};

fn digest(byte: u8) -> CredentialDigest {
    CredentialDigest::from([byte; 32])
}

fn primary_client(digest: CredentialDigest) -> BootstrapClient {
    BootstrapClient::new(
        PRIMARY_CLIENT_ID,
        "hermes-primary".to_owned(),
        ClientRole::Hermes,
        Some(PRIMARY_USER_ID),
        digest,
    )
    .unwrap()
}

fn secondary_client(digest: CredentialDigest) -> BootstrapClient {
    BootstrapClient::new(
        SECONDARY_CLIENT_ID,
        "hermes-secondary".to_owned(),
        ClientRole::Hermes,
        Some(SECONDARY_USER_ID),
        digest,
    )
    .unwrap()
}

#[tokio::test]
async fn fixed_client_upsert_resolves_the_seeded_user_and_authenticates() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmClientStore::new(test_db.connection().clone());
    let primary_digest = digest(0x11);
    let secondary_digest = digest(0x12);

    assert_eq!(store.find_by_digest(primary_digest).await.unwrap(), None);
    store
        .upsert_client(primary_client(primary_digest))
        .await
        .unwrap();
    store
        .upsert_client(secondary_client(secondary_digest))
        .await
        .unwrap();

    let primary = store
        .find_by_digest(primary_digest)
        .await
        .unwrap()
        .expect("enabled fixed client must authenticate");
    assert_eq!(primary.client_id(), PRIMARY_CLIENT_ID);
    assert_eq!(primary.user_id(), Some(PRIMARY_USER_ID));
    assert_eq!(primary.role(), ClientRole::Hermes);

    let secondary = store
        .find_by_digest(secondary_digest)
        .await
        .unwrap()
        .expect("enabled fixed client must authenticate");
    assert_eq!(secondary.client_id(), SECONDARY_CLIENT_ID);
    assert_eq!(secondary.user_id(), Some(SECONDARY_USER_ID));
    assert_eq!(secondary.role(), ClientRole::Hermes);
}

#[tokio::test]
async fn upsert_rotates_the_digest_without_leaving_the_old_credential_valid() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmClientStore::new(test_db.connection().clone());
    let old_digest = digest(0x21);
    let new_digest = digest(0x22);

    store
        .upsert_client(primary_client(old_digest))
        .await
        .unwrap();
    store
        .upsert_client(primary_client(new_digest))
        .await
        .unwrap();

    assert_eq!(store.find_by_digest(old_digest).await.unwrap(), None);
    assert_eq!(
        store
            .find_by_digest(new_digest)
            .await
            .unwrap()
            .unwrap()
            .user_id(),
        Some(PRIMARY_USER_ID),
    );
}

#[tokio::test]
async fn disabled_clients_are_rejected_and_runner_identity_has_no_user() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmClientStore::new(test_db.connection().clone());
    let hermes_digest = digest(0x31);
    let runner_digest = digest(0x32);

    store
        .upsert_client(primary_client(hermes_digest))
        .await
        .unwrap();
    execute(
        test_db.connection(),
        "UPDATE api_clients SET enabled = false WHERE id = \
         '00000000-0000-0000-0001-000000000001'",
    )
    .await
    .unwrap();
    assert_eq!(store.find_by_digest(hermes_digest).await.unwrap(), None);

    let runner = BootstrapClient::new(
        RUNNER_CLIENT_ID,
        "download-runner".to_owned(),
        ClientRole::Runner,
        None,
        runner_digest,
    )
    .unwrap();
    store.upsert_client(runner).await.unwrap();
    let actor = store.find_by_digest(runner_digest).await.unwrap().unwrap();
    assert_eq!(actor.client_id(), RUNNER_CLIENT_ID);
    assert_eq!(actor.user_id(), None);
    assert_eq!(actor.role(), ClientRole::Runner);
}

#[tokio::test]
async fn digest_collisions_are_atomic_and_errors_never_reveal_credentials() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmClientStore::new(test_db.connection().clone());
    let primary_digest = digest(0x41);
    let runner_digest = digest(0x42);

    store
        .upsert_client(primary_client(primary_digest))
        .await
        .unwrap();
    store
        .upsert_client(
            BootstrapClient::new(
                RUNNER_CLIENT_ID,
                "download-runner".to_owned(),
                ClientRole::Runner,
                None,
                runner_digest,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let error = store
        .upsert_client(primary_client(runner_digest))
        .await
        .unwrap_err();
    assert_eq!(error, PortError::Conflict);
    assert_eq!(format!("{error:?}"), "Conflict");
    assert_eq!(format!("{error}"), "persistence conflict");
    assert!(
        store.find_by_digest(primary_digest).await.unwrap().is_some(),
        "a failed rotation must preserve the prior credential",
    );
}

#[tokio::test]
async fn clients_outside_the_fixed_bootstrap_set_are_rejected() {
    let test_db = TestDatabase::start().await;
    Migrator::up(test_db.connection(), None).await.unwrap();
    let store = SeaOrmClientStore::new(test_db.connection().clone());
    let unknown = BootstrapClient::new(
        media_core::ClientId::new(),
        "unknown".to_owned(),
        ClientRole::Runner,
        None,
        digest(0x51),
    )
    .unwrap();

    assert_eq!(store.upsert_client(unknown).await, Err(PortError::Conflict));
}
