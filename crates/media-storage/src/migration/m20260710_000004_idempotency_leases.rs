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

            db.execute_unprepared(
                r#"
            CREATE TABLE idempotency_records (
                id uuid PRIMARY KEY,
                client_id uuid NOT NULL REFERENCES api_clients(id) ON DELETE CASCADE,
                idempotency_key text NOT NULL,
                request_hash bytea NOT NULL,
                status text NOT NULL,
                response_status smallint,
                response_content_type text,
                response_body bytea,
                expires_at timestamptz NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT idempotency_records_key_not_blank
                    CHECK (btrim(idempotency_key) <> ''),
                CONSTRAINT idempotency_records_request_hash_length_check
                    CHECK (octet_length(request_hash) = 32),
                CONSTRAINT idempotency_records_status_check
                    CHECK (status IN ('in_progress', 'completed')),
                CONSTRAINT idempotency_records_response_status_check CHECK (
                    response_status IS NULL OR response_status BETWEEN 100 AND 599
                ),
                CONSTRAINT idempotency_records_response_shape_check CHECK (
                    status NOT IN ('in_progress', 'completed')
                    OR (
                        status = 'in_progress'
                        AND response_status IS NULL
                        AND response_content_type IS NULL
                        AND response_body IS NULL
                    )
                    OR (
                        status = 'completed'
                        AND response_status IS NOT NULL
                        AND response_content_type IS NOT NULL
                        AND btrim(response_content_type) <> ''
                        AND response_body IS NOT NULL
                    )
                ),
                CONSTRAINT idempotency_records_client_key_key
                    UNIQUE (client_id, idempotency_key)
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            CREATE TABLE job_leases (
                id uuid PRIMARY KEY,
                slot smallint NOT NULL,
                job_id uuid NOT NULL UNIQUE REFERENCES jobs(id) ON DELETE CASCADE,
                runner_client_id uuid NOT NULL REFERENCES api_clients(id) ON DELETE RESTRICT,
                expires_at timestamptz NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT job_leases_slot_check CHECK (slot = 1),
                CONSTRAINT job_leases_slot_key UNIQUE (slot),
                CONSTRAINT job_leases_expiry_check CHECK (expires_at > created_at)
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                "CREATE INDEX idempotency_records_expires_at_idx \
             ON idempotency_records (expires_at)",
            )
            .await?;
            db.execute_unprepared(
                "CREATE INDEX job_leases_expires_at_idx ON job_leases (expires_at)",
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
            db.execute_unprepared("DROP INDEX job_leases_expires_at_idx")
                .await?;
            db.execute_unprepared("DROP INDEX idempotency_records_expires_at_idx")
                .await?;
            db.execute_unprepared("DROP TABLE job_leases").await?;
            db.execute_unprepared("DROP TABLE idempotency_records")
                .await?;
            Ok::<(), DbErr>(())
        }
        .await;

        finish_transaction(transaction, result).await
    }
}
