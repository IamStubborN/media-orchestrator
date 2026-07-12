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
            CREATE TABLE search_sessions (
                id uuid PRIMARY KEY,
                owner_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                payload jsonb NOT NULL,
                expires_at timestamptz NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT search_sessions_payload_object CHECK (jsonb_typeof(payload) = 'object')
            );
            CREATE INDEX search_sessions_owner_expiry_idx ON search_sessions (owner_id, expires_at);
            CREATE TABLE search_executions (
                result_ref text PRIMARY KEY,
                payload jsonb NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT search_executions_ref_check CHECK (
                    char_length(result_ref) BETWEEN 11 AND 128 AND result_ref LIKE 'selection:%'
                ),
                CONSTRAINT search_executions_payload_object CHECK (jsonb_typeof(payload) = 'object')
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
            .execute_unprepared("DROP TABLE search_executions; DROP TABLE search_sessions")
            .await
            .map(|_| ());
        finish_transaction(transaction, result).await
    }
}
