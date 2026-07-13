use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        replace_constraint(manager, true).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        replace_constraint(manager, false).await
    }
}

async fn replace_constraint(manager: &SchemaManager<'_>, completed: bool) -> Result<(), DbErr> {
    let completed = if completed { "'completed', " } else { "" };
    manager
        .get_connection()
        .execute_unprepared(&format!(
            "ALTER TABLE notification_outbox \
             DROP CONSTRAINT notification_event_type_check, \
             ADD CONSTRAINT notification_event_type_check CHECK (event_type IN (\
                'started', 'choice-needed', 'downloading-started', 'downloaded', \
                'transcoding-started', 'encoding-complete', 'plex-added', {completed}\
                'session-refreshed', 'partial', 'blocked-storage', 'failed', \
                'future-episode-found'\
             ))"
        ))
        .await?;
    Ok(())
}
