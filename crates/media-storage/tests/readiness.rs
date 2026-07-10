mod support;

use media_core::ReadinessPort;
use media_storage::{Migrator, SeaOrmReadiness};
use sea_orm_migration::MigratorTrait;
use support::TestDatabase;

#[tokio::test]
async fn readiness_requires_a_reachable_database_with_no_pending_migrations() {
    let test_db = TestDatabase::start().await;
    let readiness = SeaOrmReadiness::new(test_db.connection().clone());

    assert!(!readiness.is_ready().await.unwrap());

    Migrator::up(test_db.connection(), None).await.unwrap();
    assert!(readiness.is_ready().await.unwrap());

    Migrator::down(test_db.connection(), Some(1)).await.unwrap();
    assert!(!readiness.is_ready().await.unwrap());
}
