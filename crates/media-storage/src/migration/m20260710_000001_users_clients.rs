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
                r#"
            CREATE TABLE users (
                id uuid PRIMARY KEY,
                slug text NOT NULL UNIQUE,
                display_name text NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT users_slug_not_blank CHECK (btrim(slug) <> ''),
                CONSTRAINT users_display_name_not_blank CHECK (btrim(display_name) <> '')
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            CREATE TABLE api_clients (
                id uuid PRIMARY KEY,
                name text NOT NULL UNIQUE,
                role text NOT NULL,
                user_id uuid REFERENCES users(id) ON DELETE RESTRICT,
                credential_digest bytea NOT NULL UNIQUE,
                enabled boolean NOT NULL DEFAULT true,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT api_clients_name_not_blank CHECK (btrim(name) <> ''),
                CONSTRAINT api_clients_role_check CHECK (role IN ('hermes', 'runner')),
                CONSTRAINT api_clients_digest_length_check
                    CHECK (octet_length(credential_digest) = 32),
                CONSTRAINT api_clients_role_user_check CHECK (
                    (role = 'hermes' AND user_id IS NOT NULL)
                    OR (role = 'runner' AND user_id IS NULL)
                ),
                CONSTRAINT api_clients_fixed_identity_check CHECK (
                    (
                        id = '00000000-0000-0000-0001-000000000001'
                        AND role = 'hermes'
                        AND user_id = '00000000-0000-0000-0000-000000000001'
                    )
                    OR (
                        id = '00000000-0000-0000-0001-000000000002'
                        AND role = 'hermes'
                        AND user_id = '00000000-0000-0000-0000-000000000002'
                    )
                    OR (
                        id = '00000000-0000-0000-0002-000000000001'
                        AND role = 'runner'
                        AND user_id IS NULL
                    )
                )
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            INSERT INTO users (id, slug, display_name)
            VALUES
                ('00000000-0000-0000-0000-000000000001', 'primary', 'Primary'),
                ('00000000-0000-0000-0000-000000000002', 'secondary', 'Secondary')
            "#,
            )
            .await?;

            Ok::<(), DbErr>(())
        }
        .await;

        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = async {
            transaction
                .execute_unprepared("DROP TABLE api_clients")
                .await?;
            transaction.execute_unprepared("DROP TABLE users").await?;
            Ok::<(), DbErr>(())
        }
        .await;

        finish_transaction(transaction, result).await
    }
}
