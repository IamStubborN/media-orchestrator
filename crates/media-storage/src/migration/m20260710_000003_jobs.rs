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
            CREATE TABLE jobs (
                id uuid PRIMARY KEY,
                owner_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
                provider text NOT NULL,
                result_ref text NOT NULL,
                state text NOT NULL,
                needs_action_reason text,
                notify_scope text NOT NULL,
                request_snapshot jsonb NOT NULL DEFAULT '{}'::jsonb,
                error_snapshot jsonb,
                attempt_count integer NOT NULL DEFAULT 0,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                started_at timestamptz,
                completed_at timestamptz,
                CONSTRAINT jobs_provider_check CHECK (provider IN ('rezka', 'prowlarr')),
                CONSTRAINT jobs_result_ref_not_blank CHECK (btrim(result_ref) <> ''),
                CONSTRAINT jobs_result_ref_length_check
                    CHECK (octet_length(result_ref) <= 65536),
                CONSTRAINT jobs_state_check CHECK (
                    state IN (
                        'queued', 'leased', 'running', 'cancel_requested',
                        'blocked_storage', 'publishing', 'plex_pending', 'needs_action',
                        'partial', 'completed', 'failed', 'cancelled'
                    )
                ),
                CONSTRAINT jobs_reason_check CHECK (
                    needs_action_reason IS NULL
                    OR needs_action_reason IN ('identity_ambiguous', 'plex_mismatch')
                ),
                CONSTRAINT jobs_state_reason_check CHECK (
                    (state = 'needs_action' AND needs_action_reason IS NOT NULL)
                    OR (state <> 'needs_action' AND needs_action_reason IS NULL)
                ),
                CONSTRAINT jobs_notify_scope_check
                    CHECK (notify_scope IN ('initiator', 'family')),
                CONSTRAINT jobs_attempt_count_check CHECK (attempt_count >= 0),
                CONSTRAINT jobs_request_snapshot_object_check
                    CHECK (jsonb_typeof(request_snapshot) = 'object'),
                CONSTRAINT jobs_error_snapshot_object_check CHECK (
                    error_snapshot IS NULL OR jsonb_typeof(error_snapshot) = 'object'
                )
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            CREATE TABLE job_tasks (
                id uuid PRIMARY KEY,
                job_id uuid NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                episode_id uuid REFERENCES episodes(id) ON DELETE SET NULL,
                ordinal integer NOT NULL,
                state text NOT NULL DEFAULT 'pending',
                attempt_count integer NOT NULL DEFAULT 0,
                checkpoint jsonb NOT NULL DEFAULT '{}'::jsonb,
                error_snapshot jsonb,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                started_at timestamptz,
                completed_at timestamptz,
                CONSTRAINT job_tasks_ordinal_check CHECK (ordinal >= 0),
                CONSTRAINT job_tasks_state_check CHECK (
                    state IN ('pending', 'running', 'completed', 'failed', 'skipped', 'cancelled')
                ),
                CONSTRAINT job_tasks_attempt_count_check CHECK (attempt_count >= 0),
                CONSTRAINT job_tasks_checkpoint_object_check
                    CHECK (jsonb_typeof(checkpoint) = 'object'),
                CONSTRAINT job_tasks_error_snapshot_object_check CHECK (
                    error_snapshot IS NULL OR jsonb_typeof(error_snapshot) = 'object'
                ),
                CONSTRAINT job_tasks_job_ordinal_key UNIQUE (job_id, ordinal),
                CONSTRAINT job_tasks_job_episode_key UNIQUE (job_id, episode_id)
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            CREATE TABLE job_stages (
                id uuid PRIMARY KEY,
                task_id uuid NOT NULL REFERENCES job_tasks(id) ON DELETE CASCADE,
                name text NOT NULL,
                ordinal integer NOT NULL,
                state text NOT NULL DEFAULT 'pending',
                attempt_count integer NOT NULL DEFAULT 0,
                checkpoint jsonb NOT NULL DEFAULT '{}'::jsonb,
                error_snapshot jsonb,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                started_at timestamptz,
                completed_at timestamptz,
                CONSTRAINT job_stages_name_not_blank CHECK (btrim(name) <> ''),
                CONSTRAINT job_stages_ordinal_check CHECK (ordinal >= 0),
                CONSTRAINT job_stages_state_check CHECK (
                    state IN ('pending', 'running', 'completed', 'failed', 'skipped', 'cancelled')
                ),
                CONSTRAINT job_stages_attempt_count_check CHECK (attempt_count >= 0),
                CONSTRAINT job_stages_checkpoint_object_check
                    CHECK (jsonb_typeof(checkpoint) = 'object'),
                CONSTRAINT job_stages_error_snapshot_object_check CHECK (
                    error_snapshot IS NULL OR jsonb_typeof(error_snapshot) = 'object'
                ),
                CONSTRAINT job_stages_task_ordinal_key UNIQUE (task_id, ordinal),
                CONSTRAINT job_stages_task_name_key UNIQUE (task_id, name)
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                "CREATE INDEX jobs_owner_created_at_idx ON jobs (owner_id, created_at)",
            )
            .await?;
            db.execute_unprepared(
                "CREATE INDEX jobs_state_created_at_idx ON jobs (state, created_at)",
            )
            .await?;
            db.execute_unprepared(
                "CREATE INDEX job_tasks_episode_id_idx ON job_tasks (episode_id) \
                 WHERE episode_id IS NOT NULL",
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
            db.execute_unprepared("DROP INDEX jobs_state_created_at_idx")
                .await?;
            db.execute_unprepared("DROP INDEX jobs_owner_created_at_idx")
                .await?;
            db.execute_unprepared("DROP TABLE job_stages").await?;
            db.execute_unprepared("DROP TABLE job_tasks").await?;
            db.execute_unprepared("DROP TABLE jobs").await?;
            Ok::<(), DbErr>(())
        }
        .await;

        finish_transaction(transaction, result).await
    }
}
