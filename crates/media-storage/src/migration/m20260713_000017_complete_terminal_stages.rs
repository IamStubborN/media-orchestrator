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
        let result = transaction
            .execute_unprepared(
                "UPDATE job_stages AS stage \
                 SET state = 'completed', completed_at = COALESCE(stage.completed_at, job.completed_at, NOW()) \
                 FROM job_tasks AS task, jobs AS job \
                 WHERE stage.task_id = task.id \
                   AND task.job_id = job.id \
                   AND stage.state = 'running' \
                   AND job.state IN ('completed', 'partial')",
            )
            .await
            .map(|_| ());
        finish_transaction(transaction, result).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
