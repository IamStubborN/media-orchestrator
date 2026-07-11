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
                "ALTER TABLE operation_receipts \
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
            .await?;
            db.execute_unprepared(
                "CREATE UNIQUE INDEX jobs_single_active_idx ON jobs ((true)) \
                 WHERE state IN ('leased', 'running', 'cancel_requested', \
                 'blocked_storage', 'publishing', 'plex_pending')",
            )
            .await?;
            db.execute_unprepared(
                r#"
                CREATE TABLE job_events (
                    id uuid PRIMARY KEY,
                    job_id uuid NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                    lease_id uuid,
                    runner_client_id uuid REFERENCES api_clients(id) ON DELETE RESTRICT,
                    event_type text NOT NULL,
                    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT job_events_type_not_blank CHECK (btrim(event_type) <> ''),
                    CONSTRAINT job_events_payload_object_check CHECK (jsonb_typeof(payload) = 'object')
                )
                "#,
            )
            .await?;
            db.execute_unprepared(
                r#"
                CREATE TABLE outbox_events (
                    id uuid PRIMARY KEY,
                    aggregate_type text NOT NULL,
                    aggregate_id uuid NOT NULL,
                    event_type text NOT NULL,
                    dedupe_key bytea NOT NULL UNIQUE,
                    payload jsonb NOT NULL,
                    attempt_count integer NOT NULL DEFAULT 0,
                    next_attempt_at timestamptz NOT NULL DEFAULT now(),
                    published_at timestamptz,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT outbox_aggregate_type_not_blank CHECK (btrim(aggregate_type) <> ''),
                    CONSTRAINT outbox_event_type_not_blank CHECK (btrim(event_type) <> ''),
                    CONSTRAINT outbox_payload_object_check CHECK (jsonb_typeof(payload) = 'object'),
                    CONSTRAINT outbox_attempt_count_check CHECK (attempt_count >= 0)
                )
                "#,
            )
            .await?;
            db.execute_unprepared(
                "CREATE INDEX job_events_job_created_idx ON job_events (job_id, created_at)",
            )
            .await?;
            db.execute_unprepared(
                "CREATE INDEX outbox_pending_idx ON outbox_events (next_attempt_at, created_at) \
                 WHERE published_at IS NULL",
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
            db.execute_unprepared("DROP INDEX outbox_pending_idx")
                .await?;
            db.execute_unprepared("DROP INDEX job_events_job_created_idx")
                .await?;
            db.execute_unprepared("DROP TABLE outbox_events").await?;
            db.execute_unprepared("DROP TABLE job_events").await?;
            db.execute_unprepared("DROP INDEX jobs_single_active_idx")
                .await?;
            db.execute_unprepared(
                "ALTER TABLE operation_receipts \
                 DROP CONSTRAINT operation_receipts_kind_result_check, \
                 DROP CONSTRAINT operation_receipts_kind_check, \
                 ADD CONSTRAINT operation_receipts_kind_check CHECK (operation_kind IN \
                 ('create_job', 'lease_next', 'heartbeat')), \
                 ADD CONSTRAINT operation_receipts_kind_result_check CHECK ( \
                 result_kind = 'pending' \
                 OR (operation_kind = 'create_job' AND result_kind = 'job') \
                 OR (operation_kind IN ('lease_next', 'heartbeat') \
                     AND result_kind IN ('lease', 'none')))",
            )
            .await?;
            Ok::<(), DbErr>(())
        }
        .await;
        finish_transaction(transaction, result).await
    }
}
