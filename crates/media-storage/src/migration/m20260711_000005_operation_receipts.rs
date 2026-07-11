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
                r#"
                CREATE TABLE operation_receipts (
                    id uuid PRIMARY KEY,
                    operation_key bytea NOT NULL,
                    operation_kind text NOT NULL,
                    result_kind text NOT NULL,
                    result_snapshot jsonb,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT operation_receipts_key_length_check
                        CHECK (octet_length(operation_key) = 32),
                    CONSTRAINT operation_receipts_kind_check
                        CHECK (operation_kind IN ('create_job', 'lease_next', 'heartbeat')),
                    CONSTRAINT operation_receipts_result_kind_check
                        CHECK (result_kind IN ('pending', 'job', 'lease', 'none')),
                    CONSTRAINT operation_receipts_kind_result_check CHECK (
                        result_kind = 'pending'
                        OR (operation_kind = 'create_job' AND result_kind = 'job')
                        OR (
                            operation_kind IN ('lease_next', 'heartbeat')
                            AND result_kind IN ('lease', 'none')
                        )
                    ),
                    CONSTRAINT operation_receipts_result_shape_check CHECK (
                        (result_kind IN ('pending', 'none') AND result_snapshot IS NULL)
                        OR (
                            result_kind IN ('job', 'lease')
                            AND jsonb_typeof(result_snapshot) = 'object'
                        )
                    ),
                    CONSTRAINT operation_receipts_operation_key_key UNIQUE (operation_key)
                )
                "#,
            )
            .await
            .map(|_| ());

        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = transaction
            .execute_unprepared("DROP TABLE operation_receipts")
            .await
            .map(|_| ());

        finish_transaction(transaction, result).await
    }
}
