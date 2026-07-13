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
                "ALTER TABLE operation_receipts \
                 DROP CONSTRAINT operation_receipts_kind_result_check, \
                 DROP CONSTRAINT operation_receipts_kind_check, \
                 ADD CONSTRAINT operation_receipts_kind_check CHECK (operation_kind IN \
                 ('create_job', 'cancel_job', 'retry_job', 'lease_next', 'heartbeat', 'report_event')), \
                 ADD CONSTRAINT operation_receipts_kind_result_check CHECK ( \
                 result_kind = 'pending' \
                 OR (operation_kind = 'create_job' AND result_kind = 'job') \
                 OR (operation_kind IN ('cancel_job', 'retry_job', 'report_event') \
                     AND result_kind IN ('job', 'none')) \
                 OR (operation_kind IN ('lease_next', 'heartbeat') \
                     AND result_kind IN ('lease', 'none')))"
            )
            .await
            .map(|_| ());
        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = transaction
            .execute_unprepared(
                "DELETE FROM operation_receipts WHERE operation_kind = 'retry_job'; \
                 ALTER TABLE operation_receipts \
                 DROP CONSTRAINT operation_receipts_kind_result_check, \
                 DROP CONSTRAINT operation_receipts_kind_check, \
                 ADD CONSTRAINT operation_receipts_kind_check CHECK (operation_kind IN \
                 ('create_job', 'cancel_job', 'lease_next', 'heartbeat', 'report_event')), \
                 ADD CONSTRAINT operation_receipts_kind_result_check CHECK ( \
                 result_kind = 'pending' \
                 OR (operation_kind = 'create_job' AND result_kind = 'job') \
                 OR (operation_kind IN ('cancel_job', 'report_event') \
                     AND result_kind IN ('job', 'none')) \
                 OR (operation_kind IN ('lease_next', 'heartbeat') \
                     AND result_kind IN ('lease', 'none')))",
            )
            .await
            .map(|_| ());
        finish_transaction(transaction, result).await
    }
}
