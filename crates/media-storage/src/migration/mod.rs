mod m20260710_000001_users_clients;
mod m20260710_000002_media_identity;
mod m20260710_000003_jobs;
mod m20260710_000004_idempotency_leases;
mod m20260711_000005_operation_receipts;
mod m20260712_000006_job_orchestration;
mod m20260712_000007_tracking_notifications;

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::DatabaseTransaction;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260710_000001_users_clients::Migration),
            Box::new(m20260710_000002_media_identity::Migration),
            Box::new(m20260710_000003_jobs::Migration),
            Box::new(m20260710_000004_idempotency_leases::Migration),
            Box::new(m20260711_000005_operation_receipts::Migration),
            Box::new(m20260712_000006_job_orchestration::Migration),
            Box::new(m20260712_000007_tracking_notifications::Migration),
        ]
    }
}

/// Finishes the migration body's nested transaction explicitly.
///
/// Each migration also opts into SeaORM's outer transaction so its ledger row
/// remains atomic with the committed DDL.
async fn finish_transaction(
    transaction: DatabaseTransaction,
    result: Result<(), DbErr>,
) -> Result<(), DbErr> {
    match result {
        Ok(()) => transaction.commit().await,
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(DbErr::Migration(format!(
                "migration operation failed: {error}; explicit rollback failed: {rollback_error}"
            ))),
        },
    }
}
