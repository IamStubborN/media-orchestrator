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
                ALTER TABLE runner_lifecycle
                    ADD COLUMN sticky_job_id uuid,
                    ADD COLUMN sticky_attempt_count integer NOT NULL DEFAULT 0,
                    ADD CONSTRAINT runner_lifecycle_sticky_attempt_count_check CHECK (
                        sticky_attempt_count BETWEEN 0 AND 3
                    ),
                    ADD CONSTRAINT runner_lifecycle_sticky_job_check CHECK (
                        (sticky_job_id IS NULL AND sticky_attempt_count = 0)
                        OR (sticky_job_id IS NOT NULL AND sticky_attempt_count > 0)
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
                ALTER TABLE runner_lifecycle
                    DROP CONSTRAINT runner_lifecycle_sticky_job_check,
                    DROP CONSTRAINT runner_lifecycle_sticky_attempt_count_check,
                    DROP COLUMN sticky_attempt_count,
                    DROP COLUMN sticky_job_id
                "#,
            )
            .await?;
        Ok(())
    }
}
