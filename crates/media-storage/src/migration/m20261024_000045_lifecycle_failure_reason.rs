use super::finish_transaction;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::TransactionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    // Ready and rotating rows keep the last VPN failure category as their reason;
    // blocked rows still require one.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = transaction
            .execute_unprepared(
                "ALTER TABLE runner_lifecycle \
                 DROP CONSTRAINT runner_lifecycle_reason_check, \
                 ADD CONSTRAINT runner_lifecycle_reason_check CHECK ( \
                     state <> 'blocked' OR (reason IS NOT NULL AND btrim(reason) <> '') \
                 )",
            )
            .await
            .map(|_| ());
        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = async {
            let db = &transaction;
            db.execute_unprepared(
                "UPDATE runner_lifecycle SET reason = NULL WHERE state <> 'blocked'",
            )
            .await?;
            db.execute_unprepared(
                "ALTER TABLE runner_lifecycle \
                 DROP CONSTRAINT runner_lifecycle_reason_check, \
                 ADD CONSTRAINT runner_lifecycle_reason_check CHECK ( \
                     (state = 'blocked' AND reason IS NOT NULL AND btrim(reason) <> '') \
                     OR (state <> 'blocked' AND reason IS NULL) \
                 )",
            )
            .await?;
            Ok(())
        }
        .await;
        finish_transaction(transaction, result).await
    }
}
