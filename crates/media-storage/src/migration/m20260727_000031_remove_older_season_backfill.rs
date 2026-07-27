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
                CREATE TEMPORARY TABLE accidental_older_season_discoveries
                ON COMMIT DROP
                AS
                WITH migration_clock AS (
                    SELECT to_timestamp(applied_at) AS started_at
                    FROM seaql_migrations
                    WHERE version =
                        'm20260727_000030_recheck_recent_calendar_discoveries'
                ),
                tracked_seasons AS (
                    SELECT
                        tracking.id,
                        max((known.value->>'season')::integer) AS season
                    FROM tracking_subscriptions AS tracking
                    CROSS JOIN LATERAL
                        jsonb_array_elements(tracking.known_episodes) AS known(value)
                    WHERE tracking.deleted_at IS NULL
                      AND tracking.download_provider_media_ref IS NULL
                    GROUP BY tracking.id
                )
                SELECT DISTINCT
                    discovery.id,
                    discovery.tracking_id,
                    discovery.season,
                    discovery.episode
                FROM tracking_discoveries AS discovery
                JOIN tracked_seasons AS tracked
                  ON tracked.id = discovery.tracking_id
                JOIN notification_outbox AS outbox
                  ON outbox.source_dedupe_key = uuid_send(discovery.id)
                CROSS JOIN migration_clock
                WHERE discovery.season < tracked.season
                  AND outbox.aggregate_type = 'tracking'
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.created_at >= migration_clock.started_at;

                UPDATE tracking_subscriptions AS tracking
                SET known_episodes = COALESCE((
                        SELECT jsonb_agg(item.value ORDER BY item.ordinality)
                        FROM jsonb_array_elements(tracking.known_episodes)
                             WITH ORDINALITY AS item(value, ordinality)
                        WHERE NOT EXISTS (
                            SELECT 1
                            FROM accidental_older_season_discoveries AS stale
                            WHERE stale.tracking_id = tracking.id
                              AND stale.season = (item.value->>'season')::integer
                              AND stale.episode = (item.value->>'episode')::integer
                        )
                    ), '[]'::jsonb),
                    updated_at = now()
                WHERE EXISTS (
                    SELECT 1
                    FROM accidental_older_season_discoveries AS stale
                    WHERE stale.tracking_id = tracking.id
                );

                DELETE FROM tracking_discoveries AS discovery
                USING accidental_older_season_discoveries AS stale
                WHERE discovery.id = stale.id;

                UPDATE notification_outbox AS outbox
                SET dead_at = CASE
                        WHEN outbox.delivered_at IS NULL THEN now()
                        ELSE outbox.dead_at
                    END,
                    last_error_code = format(
                        'superseded_older_season:%s:%s',
                        stale.season,
                        stale.episode
                    )
                FROM accidental_older_season_discoveries AS stale
                WHERE outbox.source_dedupe_key = uuid_send(stale.id);
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
                CREATE TEMPORARY TABLE accidental_older_season_discoveries
                ON COMMIT DROP
                AS
                SELECT DISTINCT
                    encode(outbox.source_dedupe_key, 'hex')::uuid AS id,
                    outbox.aggregate_id AS tracking_id,
                    split_part(outbox.last_error_code, ':', 2)::integer AS season,
                    split_part(outbox.last_error_code, ':', 3)::integer AS episode
                FROM notification_outbox AS outbox
                WHERE outbox.aggregate_type = 'tracking'
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.last_error_code ~
                        '^superseded_older_season:[0-9]+:[1-9][0-9]*$';

                INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
                SELECT id, tracking_id, season, episode
                FROM accidental_older_season_discoveries
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
                        FROM accidental_older_season_discoveries AS stale
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
                    FROM accidental_older_season_discoveries AS stale
                    WHERE stale.tracking_id = tracking.id
                );

                UPDATE notification_outbox
                SET dead_at = NULL,
                    last_error_code = NULL
                WHERE aggregate_type = 'tracking'
                  AND event_type = 'future-episode-found'
                  AND last_error_code ~
                        '^superseded_older_season:[0-9]+:[1-9][0-9]*$';
                "#,
            )
            .await?;
        Ok(())
    }
}
