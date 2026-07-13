use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        replace_constraints(manager, true).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        replace_constraints(manager, false).await
    }
}

async fn replace_constraints(
    manager: &SchemaManager<'_>,
    require_positive: bool,
) -> Result<(), DbErr> {
    let comparison = if require_positive { "> 0" } else { ">= 0" };
    manager
        .get_connection()
        .execute_unprepared(&format!(
            "ALTER TABLE episodes \
             DROP CONSTRAINT episodes_number_check, \
             DROP CONSTRAINT episodes_absolute_number_check, \
             ADD CONSTRAINT episodes_number_check CHECK (episode_number {comparison}), \
             ADD CONSTRAINT episodes_absolute_number_check \
                 CHECK (absolute_number IS NULL OR absolute_number {comparison}); \
             ALTER TABLE episode_provider_mappings \
             DROP CONSTRAINT episode_provider_mappings_episode_number_check, \
             ADD CONSTRAINT episode_provider_mappings_episode_number_check \
                 CHECK (provider_episode_number {comparison})"
        ))
        .await?;
    Ok(())
}
