#![allow(dead_code)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr, QueryResult, Statement};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

use media_storage::Migrator;
use sea_orm_migration::MigratorTrait;

const POSTGRES_IMAGE: &str = "postgres";
const POSTGRES_TAG_AND_DIGEST: &str = concat!(
    "17-alpine@sha256:",
    "742f40ea20b9ff2ff31db5458d127452988a2164df9e17441e191f3b72252193"
);
const POSTGRES_PORT: u16 = 5432;

pub fn operation_key() -> media_core::OperationKey {
    let id = uuid::Uuid::new_v4();
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(id.as_bytes());
    bytes[16..].copy_from_slice(id.as_bytes());
    media_core::OperationKey::from_bytes(bytes)
}

pub struct TestDatabase {
    connection: DatabaseConnection,
    url: String,
    _container: ContainerAsync<GenericImage>,
}

impl TestDatabase {
    pub async fn start() -> Self {
        let container = GenericImage::new(POSTGRES_IMAGE, POSTGRES_TAG_AND_DIGEST)
            .with_exposed_port(POSTGRES_PORT.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "media_orchestrator")
            .with_env_var("POSTGRES_USER", "media")
            .with_env_var("POSTGRES_PASSWORD", "media-test-password")
            .start()
            .await
            .expect(
                "Docker must be available and able to run the pinned PostgreSQL 17 Alpine image",
            );
        let port = container
            .get_host_port_ipv4(POSTGRES_PORT.tcp())
            .await
            .expect("PostgreSQL test container must expose port 5432");
        let url =
            format!("postgres://media:media-test-password@127.0.0.1:{port}/media_orchestrator");
        let connection = Database::connect(&url)
            .await
            .expect("PostgreSQL test database must accept connections");

        Self {
            connection,
            url,
            _container: container,
        }
    }

    pub fn connection(&self) -> &DatabaseConnection {
        &self.connection
    }

    pub async fn connect(&self) -> DatabaseConnection {
        Database::connect(&self.url)
            .await
            .expect("an independent PostgreSQL connection pool must open")
    }

    pub async fn start_migrated() -> Self {
        let database = Self::start().await;
        Migrator::up(database.connection(), None)
            .await
            .expect("all explicit migrations must apply");
        database
    }
}

pub async fn query(db: &DatabaseConnection, sql: &str) -> Vec<QueryResult> {
    db.query_all_raw(Statement::from_string(db.get_database_backend(), sql))
        .await
        .expect("schema inspection query must succeed")
}

pub async fn execute(db: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    db.execute_unprepared(sql).await.map(|_| ())
}

pub async fn assert_rejected(db: &DatabaseConnection, sql: &str, constraint: &str) {
    let error = execute(db, sql)
        .await
        .expect_err("PostgreSQL must reject a row that violates a schema invariant");
    let message = error.to_string();
    assert!(
        message.contains(constraint),
        "expected database error to name constraint {constraint:?}, got: {message}"
    );
}
