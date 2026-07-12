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
            db.execute_unprepared(r#"
                CREATE TABLE tracking_subscriptions (
                    id uuid PRIMARY KEY,
                    owner_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
                    provider text NOT NULL,
                    title text NOT NULL,
                    translation text NOT NULL,
                    known_episodes jsonb NOT NULL,
                    scope text NOT NULL,
                    next_check_at timestamptz NOT NULL DEFAULT now(),
                    created_operation_key bytea NOT NULL UNIQUE,
                    remove_operation_key bytea UNIQUE,
                    deleted_at timestamptz,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT tracking_provider_check CHECK (provider IN ('rezka', 'prowlarr')),
                    CONSTRAINT tracking_title_not_blank CHECK (btrim(title) <> ''),
                    CONSTRAINT tracking_translation_not_blank CHECK (btrim(translation) <> ''),
                    CONSTRAINT tracking_scope_check CHECK (scope IN ('personal', 'family')),
                    CONSTRAINT tracking_known_episodes_array CHECK (
                        jsonb_typeof(known_episodes) = 'array' AND jsonb_array_length(known_episodes) > 0
                    )
                )
            "#).await?;
            db.execute_unprepared(r#"
                CREATE TABLE tracking_discoveries (
                    id uuid PRIMARY KEY,
                    tracking_id uuid NOT NULL REFERENCES tracking_subscriptions(id) ON DELETE CASCADE,
                    season integer NOT NULL,
                    episode integer NOT NULL,
                    discovered_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT tracking_discovery_numbers_check CHECK (season > 0 AND episode > 0),
                    CONSTRAINT tracking_discovery_unique UNIQUE (tracking_id, season, episode)
                )
            "#).await?;
            db.execute_unprepared(r#"
                CREATE TABLE notification_outbox (
                    id uuid PRIMARY KEY,
                    aggregate_type text NOT NULL,
                    aggregate_id uuid NOT NULL,
                    event_type text NOT NULL,
                    recipient text NOT NULL,
                    source_dedupe_key bytea NOT NULL,
                    payload jsonb NOT NULL,
                    attempt_count integer NOT NULL DEFAULT 0,
                    next_attempt_at timestamptz NOT NULL DEFAULT now(),
                    lease_owner uuid,
                    lease_expires_at timestamptz,
                    delivered_at timestamptz,
                    last_error_code text,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT notification_aggregate_type_check CHECK (aggregate_type IN ('job', 'tracking')),
                    CONSTRAINT notification_event_type_check CHECK (event_type IN (
                        'started', 'choice-needed', 'downloaded', 'encoding-complete',
                        'plex-added', 'partial', 'failed', 'future-episode-found'
                    )),
                    CONSTRAINT notification_recipient_check CHECK (recipient IN ('primary', 'secondary')),
                    CONSTRAINT notification_payload_check CHECK (
                        jsonb_typeof(payload) = 'object' AND payload ? 'message'
                        AND payload - 'message' = '{}'::jsonb
                        AND jsonb_typeof(payload->'message') = 'string'
                    ),
                    CONSTRAINT notification_attempt_count_check CHECK (attempt_count >= 0),
                    CONSTRAINT notification_lease_pair_check CHECK (
                        (lease_owner IS NULL) = (lease_expires_at IS NULL)
                    ),
                    CONSTRAINT notification_source_recipient_unique UNIQUE (source_dedupe_key, recipient)
                )
            "#).await?;
            db.execute_unprepared(
                "CREATE INDEX tracking_due_idx ON tracking_subscriptions (next_check_at, created_at) WHERE deleted_at IS NULL",
            ).await?;
            db.execute_unprepared(
                "CREATE INDEX notification_pending_idx ON notification_outbox (next_attempt_at, created_at) WHERE delivered_at IS NULL",
            ).await?;
            Ok::<(), DbErr>(())
        }.await;
        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = async {
            let db = &transaction;
            db.execute_unprepared("DROP TABLE notification_outbox")
                .await?;
            db.execute_unprepared("DROP TABLE tracking_discoveries")
                .await?;
            db.execute_unprepared("DROP TABLE tracking_subscriptions")
                .await?;
            Ok::<(), DbErr>(())
        }
        .await;
        finish_transaction(transaction, result).await
    }
}
