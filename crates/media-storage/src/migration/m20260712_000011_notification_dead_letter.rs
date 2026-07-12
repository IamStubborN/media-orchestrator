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

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = async {
            let db = &transaction;
            db.execute_unprepared("ALTER TABLE notification_outbox ADD COLUMN dead_at timestamptz")
                .await?;
            db.execute_unprepared(
                "ALTER TABLE notification_outbox \
                 ADD CONSTRAINT notification_terminal_exclusive_check \
                 CHECK (delivered_at IS NULL OR dead_at IS NULL)",
            )
            .await?;
            db.execute_unprepared("DROP INDEX notification_pending_idx")
                .await?;
            db.execute_unprepared(
                "CREATE INDEX notification_pending_idx ON notification_outbox \
                 (next_attempt_at, created_at) WHERE delivered_at IS NULL AND dead_at IS NULL",
            )
            .await?;
            Ok::<(), DbErr>(())
        }
        .await;
        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = async {
            let db = &transaction;
            db.execute_unprepared("DROP INDEX notification_pending_idx")
                .await?;
            db.execute_unprepared(
                "CREATE INDEX notification_pending_idx ON notification_outbox \
                 (next_attempt_at, created_at) WHERE delivered_at IS NULL",
            )
            .await?;
            db.execute_unprepared(
                "ALTER TABLE notification_outbox \
                 DROP CONSTRAINT notification_terminal_exclusive_check",
            )
            .await?;
            db.execute_unprepared("ALTER TABLE notification_outbox DROP COLUMN dead_at")
                .await?;
            Ok::<(), DbErr>(())
        }
        .await;
        finish_transaction(transaction, result).await
    }
}
