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
            transaction.execute_unprepared(
                r#"
                ALTER TABLE api_clients DROP CONSTRAINT api_clients_role_check;
                ALTER TABLE api_clients DROP CONSTRAINT api_clients_role_user_check;
                ALTER TABLE api_clients DROP CONSTRAINT api_clients_fixed_identity_check;
                ALTER TABLE api_clients ADD CONSTRAINT api_clients_role_check
                    CHECK (role IN ('hermes', 'runner', 'lifecycle'));
                ALTER TABLE api_clients ADD CONSTRAINT api_clients_role_user_check CHECK (
                    (role = 'hermes' AND user_id IS NOT NULL)
                    OR (role IN ('runner', 'lifecycle') AND user_id IS NULL)
                );
                ALTER TABLE api_clients ADD CONSTRAINT api_clients_fixed_identity_check CHECK (
                    (id = '00000000-0000-0000-0001-000000000001' AND role = 'hermes' AND user_id = '00000000-0000-0000-0000-000000000001')
                    OR (id = '00000000-0000-0000-0001-000000000002' AND role = 'hermes' AND user_id = '00000000-0000-0000-0000-000000000002')
                    OR (id = '00000000-0000-0000-0002-000000000001' AND role = 'runner' AND user_id IS NULL)
                    OR (id = '00000000-0000-0000-0003-000000000001' AND role = 'lifecycle' AND user_id IS NULL)
                );

                CREATE TABLE runner_lifecycle (
                    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
                    state text NOT NULL CHECK (state IN ('ready', 'rotating', 'blocked')),
                    reason text,
                    previous_ip text,
                    current_ip text,
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT runner_lifecycle_reason_check CHECK (
                        (state = 'blocked' AND reason IS NOT NULL AND btrim(reason) <> '')
                        OR (state <> 'blocked' AND reason IS NULL)
                    )
                );
                INSERT INTO runner_lifecycle (singleton, state, reason)
                VALUES (true, 'blocked', 'lifecycle_not_initialized');
                "#,
            ).await?;
            Ok::<(), DbErr>(())
        }.await;
        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = async {
            transaction.execute_unprepared(
                r#"
                DROP TABLE runner_lifecycle;
                DELETE FROM api_clients WHERE id = '00000000-0000-0000-0003-000000000001';
                ALTER TABLE api_clients DROP CONSTRAINT api_clients_role_check;
                ALTER TABLE api_clients DROP CONSTRAINT api_clients_role_user_check;
                ALTER TABLE api_clients DROP CONSTRAINT api_clients_fixed_identity_check;
                ALTER TABLE api_clients ADD CONSTRAINT api_clients_role_check CHECK (role IN ('hermes', 'runner'));
                ALTER TABLE api_clients ADD CONSTRAINT api_clients_role_user_check CHECK (
                    (role = 'hermes' AND user_id IS NOT NULL) OR (role = 'runner' AND user_id IS NULL)
                );
                ALTER TABLE api_clients ADD CONSTRAINT api_clients_fixed_identity_check CHECK (
                    (id = '00000000-0000-0000-0001-000000000001' AND role = 'hermes' AND user_id = '00000000-0000-0000-0000-000000000001')
                    OR (id = '00000000-0000-0000-0001-000000000002' AND role = 'hermes' AND user_id = '00000000-0000-0000-0000-000000000002')
                    OR (id = '00000000-0000-0000-0002-000000000001' AND role = 'runner' AND user_id IS NULL)
                );
                "#,
            ).await?;
            Ok::<(), DbErr>(())
        }.await;
        finish_transaction(transaction, result).await
    }
}
