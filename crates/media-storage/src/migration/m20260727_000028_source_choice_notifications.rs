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
                CREATE FUNCTION notification_source_choice_v1_valid(candidate jsonb)
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
                        AND candidate->'actions' = '["all", "rezka", "prowlarr"]'::jsonb,
                        false
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
                        OR notification_payload_v2_valid_episode_number(payload)
                        OR notification_payload_v2_valid_specials(payload)
                        OR notification_payload_v2_valid_detailed(payload)
                        OR notification_source_choice_v1_valid(payload)
                    );

                UPDATE notification_outbox AS outbox
                SET payload = jsonb_build_object(
                    'event_type', 'media.source-choice',
                    'schema_version', 1,
                    'card_key', format(
                        'tracking:%s:%s:%s',
                        tracking.id,
                        discovery.season,
                        discovery.episode
                    ),
                    'tracking_id', tracking.id::text,
                    'title', tracking.title,
                    'season', discovery.season,
                    'episode', discovery.episode,
                    'actions', '["all", "rezka", "prowlarr"]'::jsonb
                )
                FROM tracking_subscriptions AS tracking
                JOIN tracking_discoveries AS discovery
                  ON discovery.tracking_id = tracking.id
                WHERE outbox.aggregate_type = 'tracking'
                  AND outbox.aggregate_id = tracking.id
                  AND outbox.event_type = 'future-episode-found'
                  AND outbox.delivered_at IS NULL
                  AND outbox.dead_at IS NULL
                  AND outbox.payload ? 'message'
                  AND outbox.source_dedupe_key = uuid_send(discovery.id);
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
                SET payload = jsonb_build_object(
                    'message',
                    format(
                        E'📺 Новая серия доступна\n\n🎬 %s\n🔔 S%sE%s\n\n➡️ Выберите источник: All, Rezka, Prowlarr',
                        payload->>'title',
                        lpad(payload->>'season', 2, '0'),
                        lpad(payload->>'episode', 2, '0')
                    )
                )
                WHERE notification_source_choice_v1_valid(payload);

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
                    );

                DROP FUNCTION notification_source_choice_v1_valid(jsonb);
                "#,
            )
            .await?;
        Ok(())
    }
}
