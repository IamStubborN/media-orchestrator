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
        let result = async {
            let db = &transaction;
            db.execute_unprepared(
                "ALTER TABLE notification_outbox DROP CONSTRAINT notification_recipient_check",
            )
            .await?;
            db.execute_unprepared(
                "WITH identities AS ( \
                     SELECT max(slug) FILTER (WHERE id = '00000000-0000-0000-0000-000000000001') AS primary_slug, \
                            max(slug) FILTER (WHERE id = '00000000-0000-0000-0000-000000000002') AS secondary_slug \
                     FROM users \
                 ) \
                 UPDATE notification_outbox AS outbox \
                 SET recipient = CASE \
                     WHEN outbox.recipient = identities.primary_slug THEN 'primary' \
                     WHEN outbox.recipient = identities.secondary_slug THEN 'secondary' \
                     ELSE outbox.recipient \
                 END \
                 FROM identities \
                 WHERE outbox.recipient IN (identities.primary_slug, identities.secondary_slug)",
            )
            .await?;
            db.execute_unprepared(
                "ALTER TABLE notification_outbox ADD CONSTRAINT notification_recipient_check \
                 CHECK (recipient IN ('primary', 'secondary'))",
            )
            .await?;
            db.execute_unprepared(
                "UPDATE users SET slug = 'primary', display_name = 'Primary', updated_at = now() \
                 WHERE id = '00000000-0000-0000-0000-000000000001'",
            )
            .await?;
            db.execute_unprepared(
                "UPDATE users SET slug = 'secondary', display_name = 'Secondary', updated_at = now() \
                 WHERE id = '00000000-0000-0000-0000-000000000002'",
            )
            .await?;
            db.execute_unprepared(
                "UPDATE api_clients SET name = 'Primary', updated_at = now() \
                 WHERE id = '00000000-0000-0000-0001-000000000001'",
            )
            .await?;
            db.execute_unprepared(
                "UPDATE api_clients SET name = 'Secondary', updated_at = now() \
                 WHERE id = '00000000-0000-0000-0001-000000000002'",
            )
            .await?;
            Ok::<(), DbErr>(())
        }
        .await;
        finish_transaction(transaction, result).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "identity rename cannot be reversed without a database backup".into(),
        ))
    }
}
