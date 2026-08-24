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
                CREATE OR REPLACE FUNCTION notification_source_choice_v1_valid(candidate jsonb)
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    SELECT COALESCE(
                        jsonb_typeof(candidate) = 'object'
                        AND candidate ?& ARRAY[
                            'event_type', 'schema_version', 'card_key', 'tracking_id',
                            'title', 'season', 'episode', 'actions'
                        ]
                        AND candidate - ARRAY[
                            'event_type', 'schema_version', 'card_key', 'tracking_id',
                            'title', 'season', 'episode', 'actions', 'poster_url',
                            'choice_set_id', 'choice_set_expires_at', 'rezka_count',
                            'prowlarr_count', 'season_complete'
                        ] = '{}'::jsonb
                        AND candidate->>'event_type' = 'media.source-choice'
                        AND candidate->'schema_version' = '1'::jsonb
                        AND jsonb_typeof(candidate->'card_key') = 'string'
                        AND length(candidate->>'card_key') BETWEEN 1 AND 96
                        AND candidate->>'card_key' ~ '^[A-Za-z0-9._:-]+$'
                        AND jsonb_typeof(candidate->'tracking_id') = 'string'
                        AND candidate->>'tracking_id' ~*
                            '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                        AND jsonb_typeof(candidate->'title') = 'string'
                        AND octet_length(candidate->>'title') BETWEEN 1 AND 256
                        AND btrim(candidate->>'title') <> ''
                        AND candidate->>'title' NOT LIKE '%://%'
                        AND notification_unsigned_integer_in_range(
                            candidate->'season', 0, 4294967295
                        )
                        AND notification_unsigned_integer_in_range(
                            candidate->'episode', 1, 4294967295
                        )
                        AND candidate->'actions' IN (
                            '["rezka"]'::jsonb,
                            '["prowlarr"]'::jsonb,
                            '["all", "rezka", "prowlarr"]'::jsonb
                        )
                        AND (
                            NOT candidate ?| ARRAY[
                                'choice_set_id', 'choice_set_expires_at', 'rezka_count',
                                'prowlarr_count'
                            ]
                            OR candidate ?& ARRAY[
                                'choice_set_id', 'choice_set_expires_at', 'rezka_count',
                                'prowlarr_count'
                            ]
                        )
                        AND (
                            NOT candidate ? 'poster_url'
                            OR (
                                jsonb_typeof(candidate->'poster_url') = 'string'
                                AND length(candidate->>'poster_url') BETWEEN 1 AND 2048
                                AND candidate->>'poster_url'
                                    ~ '^https://[^/@[:space:]]+([/?#][^[:space:]]*)?$'
                            )
                        )
                        AND (
                            NOT candidate ? 'choice_set_id'
                            OR (
                                jsonb_typeof(candidate->'choice_set_id') = 'string'
                                AND candidate->>'choice_set_id' ~*
                                    '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                            )
                        )
                        AND (
                            NOT candidate ? 'choice_set_expires_at'
                            OR (
                                jsonb_typeof(candidate->'choice_set_expires_at') = 'string'
                                AND length(candidate->>'choice_set_expires_at') BETWEEN 1 AND 64
                                AND candidate->>'choice_set_expires_at' NOT LIKE '%://%'
                            )
                        )
                        AND (
                            NOT candidate ? 'rezka_count'
                            OR notification_unsigned_integer_in_range(candidate->'rezka_count', 0, 1000)
                        )
                        AND (
                            NOT candidate ? 'prowlarr_count'
                            OR notification_unsigned_integer_in_range(candidate->'prowlarr_count', 0, 1000)
                        )
                        AND (
                            NOT candidate ? 'season_complete'
                            OR jsonb_typeof(candidate->'season_complete') = 'boolean'
                        ),
                        false
                    )
                $function$;
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
                UPDATE notification_outbox
                SET payload = payload - 'season_complete'
                WHERE payload->>'event_type' = 'media.source-choice'
                  AND payload->'schema_version' = '1'::jsonb;

                CREATE OR REPLACE FUNCTION notification_source_choice_v1_valid(candidate jsonb)
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    SELECT COALESCE(
                        jsonb_typeof(candidate) = 'object'
                        AND candidate ?& ARRAY[
                            'event_type', 'schema_version', 'card_key', 'tracking_id',
                            'title', 'season', 'episode', 'actions'
                        ]
                        AND candidate - ARRAY[
                            'event_type', 'schema_version', 'card_key', 'tracking_id',
                            'title', 'season', 'episode', 'actions', 'poster_url',
                            'choice_set_id', 'choice_set_expires_at', 'rezka_count',
                            'prowlarr_count'
                        ] = '{}'::jsonb
                        AND candidate->>'event_type' = 'media.source-choice'
                        AND candidate->'schema_version' = '1'::jsonb
                        AND jsonb_typeof(candidate->'card_key') = 'string'
                        AND length(candidate->>'card_key') BETWEEN 1 AND 96
                        AND candidate->>'card_key' ~ '^[A-Za-z0-9._:-]+$'
                        AND jsonb_typeof(candidate->'tracking_id') = 'string'
                        AND candidate->>'tracking_id' ~*
                            '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                        AND jsonb_typeof(candidate->'title') = 'string'
                        AND octet_length(candidate->>'title') BETWEEN 1 AND 256
                        AND btrim(candidate->>'title') <> ''
                        AND candidate->>'title' NOT LIKE '%://%'
                        AND notification_unsigned_integer_in_range(
                            candidate->'season', 0, 4294967295
                        )
                        AND notification_unsigned_integer_in_range(
                            candidate->'episode', 1, 4294967295
                        )
                        AND candidate->'actions' IN (
                            '["rezka"]'::jsonb,
                            '["prowlarr"]'::jsonb,
                            '["all", "rezka", "prowlarr"]'::jsonb
                        )
                        AND (
                            NOT candidate ?| ARRAY[
                                'choice_set_id', 'choice_set_expires_at', 'rezka_count',
                                'prowlarr_count'
                            ]
                            OR candidate ?& ARRAY[
                                'choice_set_id', 'choice_set_expires_at', 'rezka_count',
                                'prowlarr_count'
                            ]
                        )
                        AND (
                            NOT candidate ? 'poster_url'
                            OR (
                                jsonb_typeof(candidate->'poster_url') = 'string'
                                AND length(candidate->>'poster_url') BETWEEN 1 AND 2048
                                AND candidate->>'poster_url'
                                    ~ '^https://[^/@[:space:]]+([/?#][^[:space:]]*)?$'
                            )
                        )
                        AND (
                            NOT candidate ? 'choice_set_id'
                            OR (
                                jsonb_typeof(candidate->'choice_set_id') = 'string'
                                AND candidate->>'choice_set_id' ~*
                                    '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                            )
                        )
                        AND (
                            NOT candidate ? 'choice_set_expires_at'
                            OR (
                                jsonb_typeof(candidate->'choice_set_expires_at') = 'string'
                                AND length(candidate->>'choice_set_expires_at') BETWEEN 1 AND 64
                                AND candidate->>'choice_set_expires_at' NOT LIKE '%://%'
                            )
                        )
                        AND (
                            NOT candidate ? 'rezka_count'
                            OR notification_unsigned_integer_in_range(candidate->'rezka_count', 0, 1000)
                        )
                        AND (
                            NOT candidate ? 'prowlarr_count'
                            OR notification_unsigned_integer_in_range(candidate->'prowlarr_count', 0, 1000)
                        ),
                        false
                    )
                $function$;
                "#,
            )
            .await?;
        Ok(())
    }
}
