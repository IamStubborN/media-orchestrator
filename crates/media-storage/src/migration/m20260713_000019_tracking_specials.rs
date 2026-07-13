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

async fn replace_constraint(
    manager: &SchemaManager<'_>,
    allow_specials: bool,
) -> Result<(), DbErr> {
    let season_check = if allow_specials {
        "season >= 0"
    } else {
        "season > 0"
    };
    manager
        .get_connection()
        .execute_unprepared(&format!(
            "ALTER TABLE tracking_discoveries \
             DROP CONSTRAINT tracking_discovery_numbers_check, \
             ADD CONSTRAINT tracking_discovery_numbers_check \
             CHECK ({season_check} AND episode > 0)"
        ))
        .await?;
    Ok(())
}
