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
                "ALTER TABLE notification_outbox \
                 ADD COLUMN generation bigint NOT NULL DEFAULT 1, \
                 ADD CONSTRAINT notification_generation_positive CHECK (generation > 0), \
                 DROP CONSTRAINT notification_event_type_check, \
                 ADD CONSTRAINT notification_event_type_check CHECK (event_type IN (\
                    'started', 'choice-needed', 'downloading-started', 'download-progress', \
                    'downloaded', 'transcoding-started', 'encoding-complete', 'plex-added', \
                    'completed', 'session-refreshed', 'partial', 'blocked-storage', 'failed', \
                    'future-episode-found'\
                 ))",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE notification_outbox \
                 DROP CONSTRAINT notification_event_type_check, \
                 ADD CONSTRAINT notification_event_type_check CHECK (event_type IN (\
                    'started', 'choice-needed', 'downloading-started', 'downloaded', \
                    'transcoding-started', 'encoding-complete', 'plex-added', 'completed', \
                    'session-refreshed', 'partial', 'blocked-storage', 'failed', \
                    'future-episode-found'\
                 )), \
                 DROP CONSTRAINT notification_generation_positive, \
                 DROP COLUMN generation",
            )
            .await?;
        Ok(())
    }
}
