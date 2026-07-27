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
                            'title', 'season', 'episode', 'actions'
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
                        ),
                        false
                    )
                $function$;

                CREATE TEMPORARY TABLE availability_unverified_discoveries
                ON COMMIT DROP
                AS
                SELECT DISTINCT
                    discovery.id,
                    discovery.tracking_id,
                    discovery.season,
                    discovery.episode
                FROM notification_outbox AS outbox
                JOIN tracking_discoveries AS discovery
                  ON outbox.source_dedupe_key = uuid_send(discovery.id)
                WHERE outbox.aggregate_type = 'tracking'
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.delivered_at IS NULL
                  AND outbox.dead_at IS NULL
                  AND notification_source_choice_v1_valid(outbox.payload);

                UPDATE tracking_subscriptions AS tracking
                SET known_episodes = (
                        SELECT jsonb_agg(item.value ORDER BY item.ordinality)
                        FROM jsonb_array_elements(tracking.known_episodes)
                             WITH ORDINALITY AS item(value, ordinality)
                        WHERE NOT EXISTS (
                            SELECT 1
                            FROM availability_unverified_discoveries AS stale
                            WHERE stale.tracking_id = tracking.id
                              AND stale.season = (item.value->>'season')::integer
                              AND stale.episode = (item.value->>'episode')::integer
                        )
                    ),
                    next_check_at = LEAST(tracking.next_check_at, now()),
                    updated_at = now()
                WHERE EXISTS (
                    SELECT 1
                    FROM availability_unverified_discoveries AS stale
                    WHERE stale.tracking_id = tracking.id
                );

                DELETE FROM tracking_discoveries AS discovery
                USING availability_unverified_discoveries AS stale
                WHERE discovery.id = stale.id;

                UPDATE notification_outbox AS outbox
                SET dead_at = now(),
                    lease_owner = NULL,
                    lease_expires_at = NULL,
                    last_error_code = 'availability_unverified'
                WHERE outbox.aggregate_type = 'tracking'
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.delivered_at IS NULL
                  AND outbox.dead_at IS NULL
                  AND notification_source_choice_v1_valid(outbox.payload);
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
                CREATE TEMPORARY TABLE availability_unverified_discoveries
                ON COMMIT DROP
                AS
                SELECT DISTINCT
                    encode(outbox.source_dedupe_key, 'hex')::uuid AS id,
                    outbox.aggregate_id AS tracking_id,
                    (outbox.payload->>'season')::integer AS season,
                    (outbox.payload->>'episode')::integer AS episode
                FROM notification_outbox AS outbox
                WHERE outbox.aggregate_type = 'tracking'
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.delivered_at IS NULL
                  AND outbox.dead_at IS NOT NULL
                  AND outbox.last_error_code = 'availability_unverified'
                  AND notification_source_choice_v1_valid(outbox.payload);

                INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
                SELECT id, tracking_id, season, episode
                FROM availability_unverified_discoveries
                ON CONFLICT (tracking_id, season, episode) DO NOTHING;

                UPDATE tracking_subscriptions AS tracking
                SET known_episodes = tracking.known_episodes || COALESCE((
                        SELECT jsonb_agg(
                            jsonb_build_object(
                                'season', stale.season,
                                'episode', stale.episode
                            )
                            ORDER BY stale.season, stale.episode
                        )
                        FROM availability_unverified_discoveries AS stale
                        WHERE stale.tracking_id = tracking.id
                          AND NOT EXISTS (
                              SELECT 1
                              FROM jsonb_array_elements(tracking.known_episodes) AS known(value)
                              WHERE (known.value->>'season')::integer = stale.season
                                AND (known.value->>'episode')::integer = stale.episode
                          )
                    ), '[]'::jsonb),
                    updated_at = now()
                WHERE EXISTS (
                    SELECT 1
                    FROM availability_unverified_discoveries AS stale
                    WHERE stale.tracking_id = tracking.id
                );

                UPDATE notification_outbox
                SET payload = jsonb_set(
                        payload,
                        '{actions}',
                        '["all", "rezka", "prowlarr"]'::jsonb
                    ),
                    dead_at = NULL,
                    last_error_code = NULL,
                    next_attempt_at = now()
                WHERE aggregate_type = 'tracking'
                  AND event_type = 'future-episode-found'
                  AND delivered_at IS NULL
                  AND last_error_code = 'availability_unverified';

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
                            'title', 'season', 'episode', 'actions'
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
                        AND candidate->'actions' =
                            '["all", "rezka", "prowlarr"]'::jsonb,
                        false
                    )
                $function$;
                "#,
            )
            .await?;
        Ok(())
    }
}
