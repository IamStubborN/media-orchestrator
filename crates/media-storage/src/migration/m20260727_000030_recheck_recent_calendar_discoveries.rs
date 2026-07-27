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
                CREATE TEMPORARY TABLE recent_unverified_calendar_discoveries
                ON COMMIT DROP
                AS
                WITH migration_window AS (
                    SELECT
                        date_trunc('day', to_timestamp(source_choice.applied_at))
                            AS started_at,
                        to_timestamp(availability_gate.applied_at) + interval '1 second'
                            AS ended_at
                    FROM seaql_migrations AS source_choice
                    JOIN seaql_migrations AS availability_gate ON true
                    WHERE source_choice.version =
                            'm20260727_000028_source_choice_notifications'
                      AND availability_gate.version =
                            'm20260727_000029_availability_gated_tracking'
                )
                SELECT DISTINCT
                    discovery.id,
                    discovery.tracking_id,
                    discovery.season,
                    discovery.episode
                FROM tracking_discoveries AS discovery
                JOIN tracking_subscriptions AS tracking
                  ON tracking.id = discovery.tracking_id
                JOIN notification_outbox AS outbox
                  ON outbox.source_dedupe_key = uuid_send(discovery.id)
                CROSS JOIN migration_window
                WHERE tracking.deleted_at IS NULL
                  AND tracking.download_provider_media_ref IS NULL
                  AND outbox.aggregate_type = 'tracking'
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.created_at >= migration_window.started_at
                  AND outbox.created_at < migration_window.ended_at;

                UPDATE tracking_subscriptions AS tracking
                SET known_episodes = (
                        SELECT jsonb_agg(item.value ORDER BY item.ordinality)
                        FROM jsonb_array_elements(tracking.known_episodes)
                             WITH ORDINALITY AS item(value, ordinality)
                        WHERE NOT EXISTS (
                            SELECT 1
                            FROM recent_unverified_calendar_discoveries AS stale
                            WHERE stale.tracking_id = tracking.id
                              AND stale.season = (item.value->>'season')::integer
                              AND stale.episode = (item.value->>'episode')::integer
                        )
                    ),
                    next_check_at = LEAST(tracking.next_check_at, now()),
                    updated_at = now()
                WHERE EXISTS (
                    SELECT 1
                    FROM recent_unverified_calendar_discoveries AS stale
                    WHERE stale.tracking_id = tracking.id
                );

                DELETE FROM tracking_discoveries AS discovery
                USING recent_unverified_calendar_discoveries AS stale
                WHERE discovery.id = stale.id;

                UPDATE notification_outbox AS outbox
                SET last_error_code = format(
                        'availability_unverified:%s:%s',
                        stale.season,
                        stale.episode
                    )
                FROM recent_unverified_calendar_discoveries AS stale
                WHERE outbox.source_dedupe_key = uuid_send(stale.id)
                  AND outbox.delivered_at IS NOT NULL
                  AND outbox.dead_at IS NULL;
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
                CREATE TEMPORARY TABLE recent_unverified_calendar_discoveries
                ON COMMIT DROP
                AS
                SELECT DISTINCT
                    encode(outbox.source_dedupe_key, 'hex')::uuid AS id,
                    outbox.aggregate_id AS tracking_id,
                    COALESCE(
                        (outbox.payload->>'season')::integer,
                        split_part(outbox.last_error_code, ':', 2)::integer
                    ) AS season,
                    COALESCE(
                        (outbox.payload->>'episode')::integer,
                        split_part(outbox.last_error_code, ':', 3)::integer
                    ) AS episode
                FROM notification_outbox AS outbox
                WHERE outbox.aggregate_type = 'tracking'
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.delivered_at IS NOT NULL
                  AND outbox.dead_at IS NULL
                  AND outbox.last_error_code ~
                        '^availability_unverified:[0-9]+:[1-9][0-9]*$';

                INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
                SELECT id, tracking_id, season, episode
                FROM recent_unverified_calendar_discoveries
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
                        FROM recent_unverified_calendar_discoveries AS stale
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
                    FROM recent_unverified_calendar_discoveries AS stale
                    WHERE stale.tracking_id = tracking.id
                );

                UPDATE notification_outbox
                SET last_error_code = NULL
                WHERE aggregate_type = 'tracking'
                  AND event_type = 'future-episode-found'
                  AND delivered_at IS NOT NULL
                  AND dead_at IS NULL
                  AND last_error_code ~
                        '^availability_unverified:[0-9]+:[1-9][0-9]*$';
                "#,
            )
            .await?;
        Ok(())
    }
}
