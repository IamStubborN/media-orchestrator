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
                CREATE FUNCTION notification_payload_v2_valid_transfer_details(candidate jsonb)
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    SELECT COALESCE(
                        jsonb_typeof(candidate->'progress') = 'object'
                        AND candidate->'progress' ?| ARRAY[
                            'total_bytes', 'eta_seconds', 'seeds', 'peers', 'source_state'
                        ]
                        AND (
                            notification_payload_v2_valid(
                                jsonb_set(
                                    candidate,
                                    '{progress}',
                                    (candidate->'progress') - ARRAY[
                                        'total_bytes', 'eta_seconds', 'seeds', 'peers', 'source_state'
                                    ]
                                )
                            )
                            OR notification_payload_v2_valid_episode_number(
                                jsonb_set(
                                    candidate,
                                    '{progress}',
                                    (candidate->'progress') - ARRAY[
                                        'total_bytes', 'eta_seconds', 'seeds', 'peers', 'source_state'
                                    ]
                                )
                            )
                            OR notification_payload_v2_valid_specials(
                                jsonb_set(
                                    candidate,
                                    '{progress}',
                                    (candidate->'progress') - ARRAY[
                                        'total_bytes', 'eta_seconds', 'seeds', 'peers', 'source_state'
                                    ]
                                )
                            )
                            OR notification_payload_v2_valid_detailed(
                                jsonb_set(
                                    candidate,
                                    '{progress}',
                                    (candidate->'progress') - ARRAY[
                                        'total_bytes', 'eta_seconds', 'seeds', 'peers', 'source_state'
                                    ]
                                )
                            )
                        )
                        AND (
                            NOT (candidate->'progress' ? 'total_bytes')
                            OR notification_unsigned_integer_in_range(
                                candidate->'progress'->'total_bytes', 0, 18446744073709551615
                            )
                        )
                        AND (
                            NOT (candidate->'progress' ? 'eta_seconds')
                            OR notification_unsigned_integer_in_range(
                                candidate->'progress'->'eta_seconds', 0, 18446744073709551615
                            )
                        )
                        AND (
                            NOT (candidate->'progress' ? 'seeds')
                            OR notification_unsigned_integer_in_range(
                                candidate->'progress'->'seeds', 0, 18446744073709551615
                            )
                        )
                        AND (
                            NOT (candidate->'progress' ? 'peers')
                            OR notification_unsigned_integer_in_range(
                                candidate->'progress'->'peers', 0, 18446744073709551615
                            )
                        )
                        AND (
                            NOT (candidate->'progress' ? 'source_state')
                            OR (
                                jsonb_typeof(candidate->'progress'->'source_state') = 'string'
                                AND candidate->'progress'->>'source_state' ~ '^[A-Za-z0-9_-]{1,64}$'
                            )
                        )
                        AND (
                            NOT (
                                candidate->'progress' ? 'downloaded_bytes'
                                AND candidate->'progress' ? 'total_bytes'
                            )
                            OR (
                                notification_unsigned_integer_in_range(
                                    candidate->'progress'->'downloaded_bytes',
                                    0,
                                    18446744073709551615
                                )
                                AND notification_unsigned_integer_in_range(
                                    candidate->'progress'->'total_bytes',
                                    0,
                                    18446744073709551615
                                )
                                AND (candidate->'progress'->>'downloaded_bytes')::numeric
                                    <= (candidate->'progress'->>'total_bytes')::numeric
                            )
                        ),
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
                        OR notification_payload_v2_valid_transfer_details(payload)
                        OR notification_source_choice_v1_valid(payload)
                    );
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
                SET payload = jsonb_set(
                    payload,
                    '{progress}',
                    (payload->'progress') - ARRAY[
                        'total_bytes', 'eta_seconds', 'seeds', 'peers', 'source_state'
                    ]
                )
                WHERE notification_payload_v2_valid_transfer_details(payload);

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

                DROP FUNCTION notification_payload_v2_valid_transfer_details(jsonb);
                "#,
            )
            .await?;
        Ok(())
    }
}
