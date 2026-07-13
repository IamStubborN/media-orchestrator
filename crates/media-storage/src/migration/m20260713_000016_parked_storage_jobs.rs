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
        let result = transaction
            .execute_unprepared(
                "DROP INDEX jobs_single_active_idx; \
                 CREATE UNIQUE INDEX jobs_single_active_idx ON jobs ((true)) \
                 WHERE state IN ('leased', 'running', 'cancel_requested', \
                 'publishing', 'plex_pending')",
            )
            .await
            .map(|_| ());
        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = transaction
            .execute_unprepared(
                "DROP INDEX jobs_single_active_idx; \
                 CREATE UNIQUE INDEX jobs_single_active_idx ON jobs ((true)) \
                 WHERE state IN ('leased', 'running', 'cancel_requested', \
                 'blocked_storage', 'publishing', 'plex_pending')",
            )
            .await
            .map(|_| ());
        finish_transaction(transaction, result).await
    }
}
