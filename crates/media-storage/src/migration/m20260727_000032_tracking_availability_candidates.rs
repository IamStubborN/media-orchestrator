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
                CREATE TABLE tracking_availability_candidates (
                    tracking_id uuid NOT NULL
                        REFERENCES tracking_subscriptions(id) ON DELETE CASCADE,
                    season integer NOT NULL,
                    episode integer NOT NULL,
                    first_seen_at timestamptz NOT NULL DEFAULT now(),
                    last_checked_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (tracking_id, season, episode),
                    CONSTRAINT tracking_availability_candidates_season_nonnegative
                        CHECK (season >= 0),
                    CONSTRAINT tracking_availability_candidates_episode_positive
                        CHECK (episode > 0)
                );

                CREATE INDEX tracking_availability_candidates_last_checked_idx
                    ON tracking_availability_candidates (last_checked_at);

                CREATE TEMPORARY TABLE inaccurate_prowlarr_discoveries
                ON COMMIT DROP
                AS
                WITH migration_clock AS (
                    SELECT to_timestamp(applied_at) AS started_at
                    FROM seaql_migrations
                    WHERE version =
                        'm20260727_000031_remove_older_season_backfill'
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
                CROSS JOIN migration_clock
                WHERE tracking.deleted_at IS NULL
                  AND tracking.download_provider_media_ref IS NULL
                  AND outbox.aggregate_type = 'tracking'
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.created_at >= migration_clock.started_at
                  AND (outbox.payload->'actions') ? 'prowlarr'
                  AND NOT ((outbox.payload->'actions') ? 'rezka');

                UPDATE tracking_subscriptions AS tracking
                SET known_episodes = COALESCE((
                        SELECT jsonb_agg(item.value ORDER BY item.ordinality)
                        FROM jsonb_array_elements(tracking.known_episodes)
                             WITH ORDINALITY AS item(value, ordinality)
                        WHERE NOT EXISTS (
                            SELECT 1
                            FROM inaccurate_prowlarr_discoveries AS stale
                            WHERE stale.tracking_id = tracking.id
                              AND stale.season =
                                  (item.value->>'season')::integer
                              AND stale.episode =
                                  (item.value->>'episode')::integer
                        )
                    ), '[]'::jsonb),
                    next_check_at = LEAST(tracking.next_check_at, now()),
                    updated_at = now()
                WHERE EXISTS (
                    SELECT 1
                    FROM inaccurate_prowlarr_discoveries AS stale
                    WHERE stale.tracking_id = tracking.id
                );

                INSERT INTO tracking_availability_candidates
                    (tracking_id, season, episode)
                SELECT stale.tracking_id, stale.season, stale.episode
                FROM inaccurate_prowlarr_discoveries AS stale
                WHERE stale.season > COALESCE((
                          SELECT max((known.value->>'season')::integer)
                          FROM tracking_subscriptions AS tracking
                          CROSS JOIN LATERAL
                              jsonb_array_elements(tracking.known_episodes)
                                  AS known(value)
                          WHERE tracking.id = stale.tracking_id
                      ), -1)
                   OR (
                       stale.season = COALESCE((
                           SELECT max((known.value->>'season')::integer)
                           FROM tracking_subscriptions AS tracking
                           CROSS JOIN LATERAL
                               jsonb_array_elements(tracking.known_episodes)
                                   AS known(value)
                           WHERE tracking.id = stale.tracking_id
                       ), -1)
                       AND stale.episode > COALESCE((
                           SELECT max((known.value->>'episode')::integer)
                           FROM tracking_subscriptions AS tracking
                           CROSS JOIN LATERAL
                               jsonb_array_elements(tracking.known_episodes)
                                   AS known(value)
                           WHERE tracking.id = stale.tracking_id
                             AND (known.value->>'season')::integer = stale.season
                       ), 0)
                   )
                ON CONFLICT (tracking_id, season, episode) DO NOTHING;

                DELETE FROM tracking_discoveries AS discovery
                USING inaccurate_prowlarr_discoveries AS stale
                WHERE discovery.id = stale.id;

                UPDATE notification_outbox AS outbox
                SET dead_at = CASE
                        WHEN outbox.delivered_at IS NULL THEN now()
                        ELSE outbox.dead_at
                    END,
                    last_error_code = format(
                        'prowlarr_coordinate_unverified:%s:%s',
                        stale.season,
                        stale.episode
                    )
                FROM inaccurate_prowlarr_discoveries AS stale
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
                CREATE TEMPORARY TABLE inaccurate_prowlarr_discoveries
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
                        '^prowlarr_coordinate_unverified:[0-9]+:[1-9][0-9]*$';

                INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
                SELECT id, tracking_id, season, episode
                FROM inaccurate_prowlarr_discoveries
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
                        FROM inaccurate_prowlarr_discoveries AS stale
                        WHERE stale.tracking_id = tracking.id
                          AND NOT EXISTS (
                              SELECT 1
                              FROM jsonb_array_elements(tracking.known_episodes)
                                  AS known(value)
                              WHERE (known.value->>'season')::integer = stale.season
                                AND (known.value->>'episode')::integer = stale.episode
                          )
                    ), '[]'::jsonb),
                    updated_at = now()
                WHERE EXISTS (
                    SELECT 1
                    FROM inaccurate_prowlarr_discoveries AS stale
                    WHERE stale.tracking_id = tracking.id
                );

                UPDATE notification_outbox
                SET dead_at = NULL,
                    last_error_code = NULL
                WHERE aggregate_type = 'tracking'
                  AND event_type = 'future-episode-found'
                  AND last_error_code ~
                        '^prowlarr_coordinate_unverified:[0-9]+:[1-9][0-9]*$';

                DROP TABLE tracking_availability_candidates;
                "#,
            )
            .await?;
        Ok(())
    }
}
