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
                "ALTER TABLE jobs                  DROP CONSTRAINT jobs_reason_check,                  ADD CONSTRAINT jobs_reason_check CHECK (                     needs_action_reason IS NULL                     OR needs_action_reason IN (                         'identity_ambiguous',                         'plex_mismatch',                         'no_matching_episodes'                     )                  )",
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
                "UPDATE jobs SET needs_action_reason = 'plex_mismatch'                  WHERE needs_action_reason = 'no_matching_episodes'",
            )
            .await?;
            db.execute_unprepared(
                "ALTER TABLE jobs                  DROP CONSTRAINT jobs_reason_check,                  ADD CONSTRAINT jobs_reason_check CHECK (                     needs_action_reason IS NULL                     OR needs_action_reason IN (                         'identity_ambiguous',                         'plex_mismatch'                     )                  )",
            )
            .await?;
            Ok(())
        }
        .await;
        finish_transaction(transaction, result).await
    }
}
