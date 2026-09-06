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
            // Leftover calendar-id duplicates (same owner + TVmaze id, notify-only)
            // are soft-deleted before the unique index is created. Ops may already
            // have cleaned live rows; this keeps the migration safe if leftovers remain.
            // The oldest active row per (owner_id, release_source, release_source_id)
            // is kept.
            db.execute_unprepared(
                r#"
                UPDATE tracking_subscriptions AS victim
                SET deleted_at = COALESCE(victim.deleted_at, now()),
                    updated_at = now()
                FROM (
                    SELECT id,
                           ROW_NUMBER() OVER (
                               PARTITION BY owner_id, release_source, release_source_id
                               ORDER BY created_at ASC, id ASC
                           ) AS rn
                    FROM tracking_subscriptions
                    WHERE deleted_at IS NULL
                      AND release_source_id IS NOT NULL
                      AND download_provider_media_ref IS NULL
                ) AS ranked
                WHERE victim.id = ranked.id
                  AND ranked.rn > 1
                "#,
            )
            .await?;
            db.execute_unprepared(
                r#"
                CREATE UNIQUE INDEX tracking_release_identity_active_unique
                    ON tracking_subscriptions (owner_id, release_source, release_source_id)
                    WHERE deleted_at IS NULL
                      AND release_source_id IS NOT NULL
                      AND download_provider_media_ref IS NULL
                "#,
            )
            .await?;
            db.execute_unprepared(
                r#"
                ALTER TABLE tracking_subscriptions
                    ADD COLUMN check_last_error text,
                    ADD COLUMN check_failure_count integer NOT NULL DEFAULT 0,
                    ADD CONSTRAINT tracking_subscriptions_check_failure_count_nonnegative
                        CHECK (check_failure_count >= 0)
                "#,
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
            db.execute_unprepared(
                r#"
                ALTER TABLE tracking_subscriptions
                    DROP CONSTRAINT tracking_subscriptions_check_failure_count_nonnegative,
                    DROP COLUMN check_failure_count,
                    DROP COLUMN check_last_error
                "#,
            )
            .await?;
            db.execute_unprepared("DROP INDEX IF EXISTS tracking_release_identity_active_unique")
                .await?;
            Ok::<(), DbErr>(())
        }
        .await;
        finish_transaction(transaction, result).await
    }
}
