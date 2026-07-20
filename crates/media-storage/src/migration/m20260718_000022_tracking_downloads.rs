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
                    ADD COLUMN download_provider_media_ref text,
                    ADD COLUMN download_translation_id bigint,
                    ADD COLUMN download_season integer,
                    ADD CONSTRAINT tracking_download_complete_check CHECK (
                        (download_provider_media_ref IS NULL AND download_translation_id IS NULL AND download_season IS NULL)
                        OR
                        (download_provider_media_ref IS NOT NULL AND download_translation_id IS NOT NULL AND download_season IS NOT NULL)
                    ),
                    ADD CONSTRAINT tracking_download_media_ref_check CHECK (
                        download_provider_media_ref IS NULL
                        OR (btrim(download_provider_media_ref) <> '' AND position('://' in download_provider_media_ref) = 0)
                    ),
                    ADD CONSTRAINT tracking_download_translation_check CHECK (
                        download_translation_id IS NULL OR download_translation_id > 0
                    ),
                    ADD CONSTRAINT tracking_download_season_check CHECK (
                        download_season IS NULL OR download_season > 0
                    )
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
                    DROP CONSTRAINT tracking_download_season_check,
                    DROP CONSTRAINT tracking_download_translation_check,
                    DROP CONSTRAINT tracking_download_media_ref_check,
                    DROP CONSTRAINT tracking_download_complete_check,
                    DROP COLUMN download_season,
                    DROP COLUMN download_translation_id,
                    DROP COLUMN download_provider_media_ref
                "#,
            )
            .await?;
        Ok(())
    }
}
