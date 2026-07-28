use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE tracking_subscriptions
                    ADD COLUMN last_checked_at timestamptz,
                    ADD COLUMN check_status text NOT NULL DEFAULT 'never',
                    ADD CONSTRAINT tracking_subscriptions_check_status_valid
                        CHECK (check_status IN (
                            'never',
                            'no_new_episode',
                            'awaiting_source',
                            'episode_found',
                            'download_queued',
                            'release_error',
                            'source_error'
                        ));

                UPDATE tracking_subscriptions
                SET next_check_at = LEAST(next_check_at, now() + interval '1 hour'),
                    updated_at = now()
                WHERE deleted_at IS NULL
                  AND download_provider_media_ref IS NULL;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE tracking_subscriptions
                    DROP CONSTRAINT tracking_subscriptions_check_status_valid,
                    DROP COLUMN check_status,
                    DROP COLUMN last_checked_at;
                "#,
            )
            .await?;
        Ok(())
    }
}
