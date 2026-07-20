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
                    ADD CONSTRAINT jobs_notification_cycle_positive CHECK (
                        notification_cycle BETWEEN 1 AND 9223372036854775807
                    );

                CREATE FUNCTION notification_unsigned_integer_in_range(
                    value jsonb,
                    minimum numeric,
                    maximum numeric
                )
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    SELECT CASE
                        WHEN jsonb_typeof(value) = 'number' AND value #>> '{}' ~ '^[0-9]+$'
                        THEN (value #>> '{}')::numeric BETWEEN minimum AND maximum
                        ELSE false
                    END
                $function$;

                CREATE FUNCTION notification_payload_v2_valid(candidate jsonb)
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    SELECT
                        jsonb_typeof(candidate) = 'object'
                        AND candidate ?& ARRAY['event_type', 'schema_version', 'delivery_kind', 'card_key',
                                                 'revision', 'lifecycle_cycle', 'terminal', 'state', 'media']
                        AND candidate - ARRAY['event_type', 'schema_version', 'delivery_kind', 'card_key',
                                              'revision', 'lifecycle_cycle', 'terminal', 'state', 'media',
                                              'progress', 'stage', 'next_step', 'issue', 'actions'] = '{}'::jsonb
                        AND candidate->>'event_type' = 'media.notification'
                        AND candidate->'schema_version' = '2'::jsonb
                        AND candidate->>'delivery_kind' IN ('card', 'final-push')
                        AND jsonb_typeof(candidate->'card_key') = 'string'
                        AND length(candidate->>'card_key') BETWEEN 1 AND 96
                        AND candidate->>'card_key' ~ '^[A-Za-z0-9:-]+$'
                        AND notification_unsigned_integer_in_range(
                            candidate->'revision', 1, 9223372036854775807
                        )
                        AND notification_unsigned_integer_in_range(
                            candidate->'lifecycle_cycle', 1, 9223372036854775807
                        )
                        AND jsonb_typeof(candidate->'terminal') = 'boolean'
                        AND candidate->>'state' IN ('queued', 'downloading', 'processing', 'publishing',
                                                    'completed', 'partial', 'failed', 'cancelled', 'needs-action')
                        AND jsonb_typeof(candidate->'media') = 'object'
                        AND candidate->'media' ?& ARRAY['job_id', 'title', 'kind', 'provider']
                        AND (candidate->'media') - ARRAY['job_id', 'title', 'kind', 'provider', 'season', 'translation'] = '{}'::jsonb
                        AND jsonb_typeof(candidate->'media'->'job_id') = 'string'
                        AND candidate->'media'->>'job_id' ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                        AND jsonb_typeof(candidate->'media'->'title') = 'string'
                        AND btrim(candidate->'media'->>'title') <> ''
                        AND jsonb_typeof(candidate->'media'->'provider') = 'string'
                        AND btrim(candidate->'media'->>'provider') <> ''
                        AND candidate->'media'->>'kind' IN ('movie', 'series')
                        AND (
                            NOT (candidate->'media' ? 'season')
                            OR notification_unsigned_integer_in_range(
                                candidate->'media'->'season', 1, 4294967295
                            )
                        )
                        AND (
                            NOT (candidate->'media' ? 'translation')
                            OR (
                                jsonb_typeof(candidate->'media'->'translation') = 'string'
                                AND btrim(candidate->'media'->>'translation') <> ''
                            )
                        )
                        AND (
                            NOT (candidate ? 'progress') OR CASE
                                WHEN jsonb_typeof(candidate->'progress') = 'object' THEN
                                    (candidate->'progress') - ARRAY[
                                        'completed_episodes', 'total_episodes', 'current_episode',
                                        'missing_episodes', 'downloaded_bytes', 'download_speed_bps',
                                        'percentage'
                                    ] = '{}'::jsonb
                                    AND (
                                        NOT (candidate->'progress' ? 'completed_episodes')
                                        OR notification_unsigned_integer_in_range(
                                            candidate->'progress'->'completed_episodes', 0, 4294967295
                                        )
                                    )
                                    AND (
                                        NOT (candidate->'progress' ? 'total_episodes')
                                        OR notification_unsigned_integer_in_range(
                                            candidate->'progress'->'total_episodes', 1, 4294967295
                                        )
                                    )
                                    AND (
                                        NOT (candidate->'progress' ? 'current_episode')
                                        OR notification_unsigned_integer_in_range(
                                            candidate->'progress'->'current_episode', 1, 4294967295
                                        )
                                    )
                                    AND (
                                        NOT (candidate->'progress' ? 'completed_episodes')
                                        OR CASE
                                            WHEN candidate->'progress' ? 'total_episodes'
                                                 AND notification_unsigned_integer_in_range(candidate->'progress'->'completed_episodes', 0, 4294967295)
                                                 AND notification_unsigned_integer_in_range(candidate->'progress'->'total_episodes', 1, 4294967295)
                                            THEN (candidate->'progress'->>'completed_episodes')::numeric
                                                 <= (candidate->'progress'->>'total_episodes')::numeric
                                            ELSE false
                                        END
                                    )
                                    AND (
                                        NOT (candidate->'progress' ? 'current_episode')
                                        OR CASE
                                            WHEN candidate->'progress' ? 'total_episodes'
                                                 AND notification_unsigned_integer_in_range(candidate->'progress'->'current_episode', 1, 4294967295)
                                                 AND notification_unsigned_integer_in_range(candidate->'progress'->'total_episodes', 1, 4294967295)
                                            THEN (candidate->'progress'->>'current_episode')::numeric
                                                 <= (candidate->'progress'->>'total_episodes')::numeric
                                            ELSE false
                                        END
                                    )
                                    AND (
                                        NOT (candidate->'progress' ? 'missing_episodes') OR CASE
                                            WHEN jsonb_typeof(candidate->'progress'->'missing_episodes') = 'array'
                                            THEN NOT EXISTS (
                                                SELECT 1
                                                FROM jsonb_array_elements(
                                                    candidate->'progress'->'missing_episodes'
                                                ) AS missing_episode(value)
                                                WHERE jsonb_typeof(missing_episode.value) <> 'object'
                                                   OR NOT (missing_episode.value ?& ARRAY['season', 'episode'])
                                                   OR missing_episode.value - ARRAY['season', 'episode'] <> '{}'::jsonb
                                                   OR NOT notification_unsigned_integer_in_range(
                                                        missing_episode.value->'season', 1, 4294967295
                                                   )
                                                   OR NOT notification_unsigned_integer_in_range(
                                                        missing_episode.value->'episode', 1, 4294967295
                                                   )
                                            )
                                            ELSE false
                                        END
                                    )
                                    AND (
                                        NOT (candidate->'progress' ? 'downloaded_bytes')
                                        OR notification_unsigned_integer_in_range(
                                            candidate->'progress'->'downloaded_bytes', 0, 18446744073709551615
                                        )
                                    )
                                    AND (
                                        NOT (candidate->'progress' ? 'download_speed_bps')
                                        OR notification_unsigned_integer_in_range(
                                            candidate->'progress'->'download_speed_bps', 0, 18446744073709551615
                                        )
                                    )
                                    AND (
                                        NOT (candidate->'progress' ? 'percentage')
                                        OR notification_unsigned_integer_in_range(
                                            candidate->'progress'->'percentage', 0, 100
                                        )
                                    )
                                ELSE false
                            END
                        )
                        AND (NOT (candidate ? 'stage') OR candidate->>'stage' IN ('download', 'process', 'publish'))
                        AND (NOT (candidate ? 'next_step') OR candidate->>'next_step' IN ('download', 'process', 'publish', 'none'))
                        AND (
                            NOT (candidate ? 'issue') OR (
                                jsonb_typeof(candidate->'issue') = 'object'
                                AND candidate->'issue' ?& ARRAY['code', 'message']
                                AND (candidate->'issue') - ARRAY['code', 'message'] = '{}'::jsonb
                                AND jsonb_typeof(candidate->'issue'->'code') = 'string'
                                AND jsonb_typeof(candidate->'issue'->'message') = 'string'
                            )
                        )
                        AND (
                            NOT (candidate ? 'actions') OR CASE
                                WHEN jsonb_typeof(candidate->'actions') = 'array' THEN NOT EXISTS (
                                    SELECT 1
                                    FROM jsonb_array_elements(candidate->'actions') AS action(value)
                                    WHERE jsonb_typeof(action.value) <> 'string'
                                       OR action.value #>> '{}' NOT IN (
                                            'cancel', 'details', 'retry', 'retry-missing', 'resume-storage'
                                       )
                                )
                                ELSE false
                            END
                        )
                $function$;

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
                        OR notification_payload_v2_valid(payload)
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
                    DROP COLUMN notification_cycle;

                DROP FUNCTION notification_payload_v2_valid(jsonb);
                DROP FUNCTION notification_unsigned_integer_in_range(jsonb, numeric, numeric)
                "#,
            )
            .await?;
        Ok(())
    }
}
