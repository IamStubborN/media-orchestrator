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
                ALTER TABLE jobs
                    ADD COLUMN notification_cycle bigint NOT NULL DEFAULT 1,
                    ADD CONSTRAINT jobs_notification_cycle_positive CHECK (notification_cycle > 0);

                ALTER TABLE notification_outbox
                    DROP CONSTRAINT notification_payload_check,
                    ADD CONSTRAINT notification_payload_check CHECK (
                        (
                            jsonb_typeof(payload) = 'object'
                            AND payload ? 'message'
                            AND payload - 'message' = '{}'::jsonb
                            AND jsonb_typeof(payload->'message') = 'string'
                            AND payload->>'message' NOT LIKE '%://%'
                        )
                        OR
                        (
                            jsonb_typeof(payload) = 'object'
                            AND payload ?& ARRAY['event_type', 'schema_version', 'delivery_kind', 'card_key',
                                                 'revision', 'lifecycle_cycle', 'terminal', 'state', 'media']
                            AND payload - ARRAY['event_type', 'schema_version', 'delivery_kind', 'card_key',
                                                 'revision', 'lifecycle_cycle', 'terminal', 'state', 'media',
                                                 'progress', 'stage', 'next_step', 'issue', 'actions'] = '{}'::jsonb
                            AND payload->>'event_type' = 'media.notification'
                            AND payload->'schema_version' = '2'::jsonb
                            AND payload->>'delivery_kind' IN ('card', 'final-push')
                            AND jsonb_typeof(payload->'card_key') = 'string'
                            AND length(payload->>'card_key') BETWEEN 1 AND 96
                            AND payload->>'card_key' ~ '^[A-Za-z0-9:-]+$'
                            AND jsonb_typeof(payload->'revision') = 'number'
                            AND (payload->>'revision')::bigint > 0
                            AND jsonb_typeof(payload->'lifecycle_cycle') = 'number'
                            AND (payload->>'lifecycle_cycle')::bigint > 0
                            AND jsonb_typeof(payload->'terminal') = 'boolean'
                            AND payload->>'state' IN ('queued', 'downloading', 'processing', 'publishing',
                                                      'completed', 'partial', 'failed', 'cancelled', 'needs-action')
                            AND jsonb_typeof(payload->'media') = 'object'
                            AND payload->'media' ?& ARRAY['job_id', 'title', 'kind', 'provider']
                            AND (payload->'media') - ARRAY['job_id', 'title', 'kind', 'provider', 'season', 'translation'] = '{}'::jsonb
                            AND jsonb_typeof(payload->'media'->'job_id') = 'string'
                            AND payload->'media'->>'job_id' ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                            AND jsonb_typeof(payload->'media'->'title') = 'string'
                            AND btrim(payload->'media'->>'title') <> ''
                            AND jsonb_typeof(payload->'media'->'provider') = 'string'
                            AND btrim(payload->'media'->>'provider') <> ''
                            AND payload->'media'->>'kind' IN ('movie', 'series')
                            AND (NOT (payload->'media' ? 'season') OR (jsonb_typeof(payload->'media'->'season') = 'number' AND (payload->'media'->>'season')::bigint > 0))
                            AND (NOT (payload->'media' ? 'translation') OR (jsonb_typeof(payload->'media'->'translation') = 'string' AND btrim(payload->'media'->>'translation') <> ''))
                            AND (NOT (payload ? 'progress') OR jsonb_typeof(payload->'progress') = 'object')
                            AND (NOT (payload ? 'stage') OR payload->>'stage' IN ('download', 'process', 'publish'))
                            AND (NOT (payload ? 'next_step') OR payload->>'next_step' IN ('download', 'process', 'publish', 'none'))
                            AND (NOT (payload ? 'issue') OR (jsonb_typeof(payload->'issue') = 'object' AND payload->'issue' ?& ARRAY['code', 'message'] AND (payload->'issue') - ARRAY['code', 'message'] = '{}'::jsonb AND jsonb_typeof(payload->'issue'->'code') = 'string' AND jsonb_typeof(payload->'issue'->'message') = 'string'))
                            AND (NOT (payload ? 'actions') OR jsonb_typeof(payload->'actions') = 'array')
                        )
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
                DELETE FROM notification_outbox WHERE payload->>'schema_version' = '2';

                ALTER TABLE notification_outbox
                    DROP CONSTRAINT notification_payload_check,
                    ADD CONSTRAINT notification_payload_check CHECK (
                        jsonb_typeof(payload) = 'object' AND payload ? 'message'
                        AND payload - 'message' = '{}'::jsonb
                        AND jsonb_typeof(payload->'message') = 'string'
                        AND payload->>'message' NOT LIKE '%://%'
                    );

                ALTER TABLE jobs
                    DROP CONSTRAINT jobs_notification_cycle_positive,
                    DROP COLUMN notification_cycle
                "#,
            )
            .await?;
        Ok(())
    }
}
