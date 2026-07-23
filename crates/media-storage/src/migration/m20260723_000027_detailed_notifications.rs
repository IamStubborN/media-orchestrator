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
                CREATE FUNCTION notification_payload_v2_valid_detailed(candidate jsonb)
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    WITH without_result AS (
                        SELECT candidate - 'result' AS value
                    ),
                    without_origin AS (
                        SELECT CASE
                            WHEN jsonb_typeof(value->'media') = 'object'
                            THEN jsonb_set(value, '{media}', (value->'media') - 'origin')
                            ELSE value
                        END AS value
                        FROM without_result
                    ),
                    without_progress_extensions AS (
                        SELECT CASE
                            WHEN jsonb_typeof(value->'progress') = 'object'
                            THEN jsonb_set(
                                value,
                                '{progress}',
                                (value->'progress') - ARRAY[
                                    'connection_attempt', 'connection_attempt_limit',
                                    'vpn_rotation_pending', 'storage_available_bytes',
                                    'storage_required_bytes'
                                ]
                            )
                            ELSE value
                        END AS value
                        FROM without_origin
                    ),
                    normalized AS (
                        SELECT CASE
                            WHEN jsonb_typeof(value->'actions') = 'array'
                            THEN jsonb_set(
                                value,
                                '{actions}',
                                COALESCE(
                                    (
                                        SELECT jsonb_agg(
                                            CASE
                                                WHEN action.value = '"search-alternative"'::jsonb
                                                THEN '"details"'::jsonb
                                                ELSE action.value
                                            END
                                        )
                                        FROM jsonb_array_elements(value->'actions') AS action(value)
                                    ),
                                    '[]'::jsonb
                                )
                            )
                            ELSE value
                        END AS value
                        FROM without_progress_extensions
                    )
                    SELECT COALESCE(
                        jsonb_typeof(candidate) = 'object'
                        AND (
                            NOT (candidate ? 'result') OR (
                                jsonb_typeof(candidate->'result') = 'object'
                                AND (candidate->'result') - ARRAY[
                                    'video', 'audio', 'subtitles', 'file_size_bytes',
                                    'duration_seconds', 'processing', 'publication'
                                ] = '{}'::jsonb
                                AND (
                                    NOT (candidate->'result' ? 'video') OR (
                                        jsonb_typeof(candidate->'result'->'video') = 'object'
                                        AND candidate->'result'->'video' ?& ARRAY['codec', 'width', 'height']
                                        AND (candidate->'result'->'video') - ARRAY[
                                            'codec', 'profile', 'width', 'height'
                                        ] = '{}'::jsonb
                                        AND jsonb_typeof(candidate->'result'->'video'->'codec') = 'string'
                                        AND btrim(candidate->'result'->'video'->>'codec') <> ''
                                        AND (
                                            NOT (candidate->'result'->'video' ? 'profile')
                                            OR (
                                                jsonb_typeof(candidate->'result'->'video'->'profile') = 'string'
                                                AND btrim(candidate->'result'->'video'->>'profile') <> ''
                                            )
                                        )
                                        AND notification_unsigned_integer_in_range(
                                            candidate->'result'->'video'->'width', 1, 4294967295
                                        )
                                        AND notification_unsigned_integer_in_range(
                                            candidate->'result'->'video'->'height', 1, 4294967295
                                        )
                                    )
                                )
                                AND (
                                    NOT (candidate->'result' ? 'audio') OR (
                                        jsonb_typeof(candidate->'result'->'audio') = 'object'
                                        AND candidate->'result'->'audio' ? 'codec'
                                        AND (candidate->'result'->'audio') - ARRAY[
                                            'language', 'codec', 'channels', 'channel_layout', 'title'
                                        ] = '{}'::jsonb
                                        AND jsonb_typeof(candidate->'result'->'audio'->'codec') = 'string'
                                        AND btrim(candidate->'result'->'audio'->>'codec') <> ''
                                        AND (
                                            NOT (candidate->'result'->'audio' ? 'language')
                                            OR (
                                                jsonb_typeof(candidate->'result'->'audio'->'language') = 'string'
                                                AND btrim(candidate->'result'->'audio'->>'language') <> ''
                                            )
                                        )
                                        AND (
                                            NOT (candidate->'result'->'audio' ? 'channels')
                                            OR notification_unsigned_integer_in_range(
                                                candidate->'result'->'audio'->'channels', 0, 4294967295
                                            )
                                        )
                                        AND (
                                            NOT (candidate->'result'->'audio' ? 'channel_layout')
                                            OR (
                                                jsonb_typeof(candidate->'result'->'audio'->'channel_layout') = 'string'
                                                AND btrim(candidate->'result'->'audio'->>'channel_layout') <> ''
                                            )
                                        )
                                        AND (
                                            NOT (candidate->'result'->'audio' ? 'title')
                                            OR (
                                                jsonb_typeof(candidate->'result'->'audio'->'title') = 'string'
                                                AND btrim(candidate->'result'->'audio'->>'title') <> ''
                                            )
                                        )
                                    )
                                )
                                AND (
                                    NOT (candidate->'result' ? 'subtitles') OR (
                                        jsonb_typeof(candidate->'result'->'subtitles') = 'object'
                                        AND candidate->'result'->'subtitles' ?& ARRAY['downloaded', 'missing']
                                        AND (candidate->'result'->'subtitles') - ARRAY['downloaded', 'missing'] = '{}'::jsonb
                                        AND notification_unsigned_integer_in_range(
                                            candidate->'result'->'subtitles'->'downloaded', 0, 4294967295
                                        )
                                        AND notification_unsigned_integer_in_range(
                                            candidate->'result'->'subtitles'->'missing', 0, 4294967295
                                        )
                                    )
                                )
                                AND (
                                    NOT (candidate->'result' ? 'file_size_bytes')
                                    OR notification_unsigned_integer_in_range(
                                        candidate->'result'->'file_size_bytes', 0, 18446744073709551615
                                    )
                                )
                                AND (
                                    NOT (candidate->'result' ? 'duration_seconds')
                                    OR notification_unsigned_integer_in_range(
                                        candidate->'result'->'duration_seconds', 0, 18446744073709551615
                                    )
                                )
                                AND (
                                    NOT (candidate->'result' ? 'processing') OR (
                                        jsonb_typeof(candidate->'result'->'processing') = 'object'
                                        AND candidate->'result'->'processing' ? 'mode'
                                        AND (candidate->'result'->'processing') - ARRAY['mode', 'elapsed_seconds'] = '{}'::jsonb
                                        AND candidate->'result'->'processing'->>'mode' IN ('vaapi-upscale', 'original')
                                        AND (
                                            NOT (candidate->'result'->'processing' ? 'elapsed_seconds')
                                            OR notification_unsigned_integer_in_range(
                                                candidate->'result'->'processing'->'elapsed_seconds',
                                                0, 18446744073709551615
                                            )
                                        )
                                    )
                                )
                                AND (
                                    NOT (candidate->'result' ? 'publication') OR (
                                        jsonb_typeof(candidate->'result'->'publication') = 'object'
                                        AND candidate->'result'->'publication' ?& ARRAY['library', 'title']
                                        AND (candidate->'result'->'publication') - ARRAY[
                                            'library', 'title', 'season', 'episode'
                                        ] = '{}'::jsonb
                                        AND candidate->'result'->'publication'->>'library' IN ('movies', 'tv-shows')
                                        AND jsonb_typeof(candidate->'result'->'publication'->'title') = 'string'
                                        AND btrim(candidate->'result'->'publication'->>'title') <> ''
                                        AND (
                                            NOT (candidate->'result'->'publication' ? 'season')
                                            OR notification_unsigned_integer_in_range(
                                                candidate->'result'->'publication'->'season', 0, 4294967295
                                            )
                                        )
                                        AND (
                                            NOT (candidate->'result'->'publication' ? 'episode')
                                            OR notification_unsigned_integer_in_range(
                                                candidate->'result'->'publication'->'episode', 0, 4294967295
                                            )
                                        )
                                    )
                                )
                            )
                        )
                        AND (
                            NOT (candidate #> '{media,origin}' IS NOT NULL)
                            OR candidate #> '{media,origin}' = '"tracked-episode"'::jsonb
                        )
                        AND (
                            NOT (candidate #> '{progress,connection_attempt}' IS NOT NULL)
                            OR notification_unsigned_integer_in_range(
                                candidate #> '{progress,connection_attempt}', 1, 4294967295
                            )
                        )
                        AND (
                            NOT (candidate #> '{progress,connection_attempt_limit}' IS NOT NULL)
                            OR notification_unsigned_integer_in_range(
                                candidate #> '{progress,connection_attempt_limit}', 1, 4294967295
                            )
                        )
                        AND (
                            NOT (candidate #> '{progress,vpn_rotation_pending}' IS NOT NULL)
                            OR jsonb_typeof(candidate #> '{progress,vpn_rotation_pending}') = 'boolean'
                        )
                        AND (
                            NOT (candidate #> '{progress,storage_available_bytes}' IS NOT NULL)
                            OR notification_unsigned_integer_in_range(
                                candidate #> '{progress,storage_available_bytes}', 0, 18446744073709551615
                            )
                        )
                        AND (
                            NOT (candidate #> '{progress,storage_required_bytes}' IS NOT NULL)
                            OR notification_unsigned_integer_in_range(
                                candidate #> '{progress,storage_required_bytes}', 0, 18446744073709551615
                            )
                        )
                        AND (
                            (candidate #> '{progress,storage_available_bytes}' IS NULL)
                            = (candidate #> '{progress,storage_required_bytes}' IS NULL)
                        )
                        AND (
                            NOT (
                                candidate #> '{progress,connection_attempt}' IS NOT NULL
                                AND candidate #> '{progress,connection_attempt_limit}' IS NOT NULL
                            )
                            OR CASE
                                WHEN notification_unsigned_integer_in_range(
                                    candidate #> '{progress,connection_attempt}', 1, 4294967295
                                )
                                AND notification_unsigned_integer_in_range(
                                    candidate #> '{progress,connection_attempt_limit}', 1, 4294967295
                                )
                                THEN (candidate #>> '{progress,connection_attempt}')::numeric
                                     <= (candidate #>> '{progress,connection_attempt_limit}')::numeric
                                ELSE false
                            END
                        )
                        AND (
                            notification_payload_v2_valid(normalized.value)
                            OR notification_payload_v2_valid_episode_number(normalized.value)
                            OR notification_payload_v2_valid_specials(normalized.value)
                        ),
                        false
                    )
                    FROM normalized
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
                        OR notification_payload_v2_valid_episode_number(payload)
                        OR notification_payload_v2_valid_specials(payload)
                        OR notification_payload_v2_valid_detailed(payload)
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
                WITH without_result AS (
                    SELECT id, payload - 'result' AS payload
                    FROM notification_outbox
                    WHERE payload->>'schema_version' = '2'
                ),
                without_origin AS (
                    SELECT id, CASE
                        WHEN jsonb_typeof(payload->'media') = 'object'
                        THEN jsonb_set(payload, '{media}', (payload->'media') - 'origin')
                        ELSE payload
                    END AS payload
                    FROM without_result
                ),
                without_progress_extensions AS (
                    SELECT id, CASE
                        WHEN jsonb_typeof(payload->'progress') = 'object'
                        THEN jsonb_set(
                            payload,
                            '{progress}',
                            (payload->'progress') - ARRAY[
                                'connection_attempt', 'connection_attempt_limit',
                                'vpn_rotation_pending', 'storage_available_bytes',
                                'storage_required_bytes'
                            ]
                        )
                        ELSE payload
                    END AS payload
                    FROM without_origin
                ),
                normalized AS (
                    SELECT id, CASE
                        WHEN jsonb_typeof(payload->'actions') = 'array'
                        THEN jsonb_set(
                            payload,
                            '{actions}',
                            COALESCE(
                                (
                                    SELECT jsonb_agg(action.value)
                                    FROM jsonb_array_elements(payload->'actions') AS action(value)
                                    WHERE action.value <> '"search-alternative"'::jsonb
                                ),
                                '[]'::jsonb
                            )
                        )
                        ELSE payload
                    END AS payload
                    FROM without_progress_extensions
                )
                UPDATE notification_outbox AS outbox
                SET payload = normalized.payload
                FROM normalized
                WHERE outbox.id = normalized.id;

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
                        OR notification_payload_v2_valid_episode_number(payload)
                        OR notification_payload_v2_valid_specials(payload)
                    );

                DROP FUNCTION notification_payload_v2_valid_detailed(jsonb);
                "#,
            )
            .await?;
        Ok(())
    }
}
