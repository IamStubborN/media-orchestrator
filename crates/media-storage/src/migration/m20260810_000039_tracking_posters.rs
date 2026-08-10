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
                    ADD COLUMN poster_url text,
                    ADD CONSTRAINT tracking_poster_url_check CHECK (
                        poster_url IS NULL OR (
                            length(poster_url) BETWEEN 1 AND 2048
                            AND poster_url ~ '^https://[^/@[:space:]]+([/?][^#[:space:]]*)?$'
                        )
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
                    DROP CONSTRAINT tracking_poster_url_check,
                    DROP COLUMN poster_url;
                "#,
            )
            .await?;
        Ok(())
    }
}
