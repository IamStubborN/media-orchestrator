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
                    ADD COLUMN check_claim_token uuid,
                    ADD COLUMN check_claim_until timestamptz,
                    ADD COLUMN check_requested_at timestamptz,
                    ADD CONSTRAINT tracking_check_claim_pair CHECK (
                        (check_claim_token IS NULL) = (check_claim_until IS NULL)
                    );

                CREATE TABLE tracking_download_reservations (
                    tracking_id uuid NOT NULL REFERENCES tracking_subscriptions(id) ON DELETE CASCADE,
                    season integer NOT NULL CHECK (season > 0),
                    episode integer NOT NULL CHECK (episode > 0),
                    provider_media_ref text NOT NULL,
                    translation_id bigint NOT NULL CHECK (translation_id > 0),
                    download_season integer NOT NULL CHECK (download_season > 0),
                    created_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (tracking_id, season, episode)
                );

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
                DROP TABLE tracking_download_reservations;

                ALTER TABLE tracking_subscriptions
                    DROP CONSTRAINT tracking_check_claim_pair,
                    DROP COLUMN check_requested_at,
                    DROP COLUMN check_claim_until,
                    DROP COLUMN check_claim_token;
                "#,
            )
            .await?;
        Ok(())
    }
}
