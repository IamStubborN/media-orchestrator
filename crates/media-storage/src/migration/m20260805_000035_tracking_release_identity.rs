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
                    ADD COLUMN release_source text,
                    ADD COLUMN release_source_id bigint,
                    ADD CONSTRAINT tracking_release_identity_pair_check CHECK (
                        (release_source IS NULL) = (release_source_id IS NULL)
                    ),
                    ADD CONSTRAINT tracking_release_source_check CHECK (
                        release_source IS NULL OR release_source = 'tvmaze'
                    ),
                    ADD CONSTRAINT tracking_release_source_id_positive_check CHECK (
                        release_source_id IS NULL OR release_source_id > 0
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
                ALTER TABLE tracking_subscriptions
                    DROP CONSTRAINT tracking_release_source_id_positive_check,
                    DROP CONSTRAINT tracking_release_source_check,
                    DROP CONSTRAINT tracking_release_identity_pair_check,
                    DROP COLUMN release_source_id,
                    DROP COLUMN release_source;
                "#,
            )
            .await?;
        Ok(())
    }
}
