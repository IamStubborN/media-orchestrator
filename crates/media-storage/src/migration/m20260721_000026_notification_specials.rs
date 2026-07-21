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
                CREATE FUNCTION notification_payload_v2_valid_specials(candidate jsonb)
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    WITH media_normalized AS (
                        SELECT CASE
                            WHEN candidate #> '{media,season}' = '0'::jsonb
                            THEN jsonb_set(candidate, '{media,season}', '1'::jsonb)
                            ELSE candidate
                        END AS value
                    ),
                    normalized AS (
                        SELECT CASE
                            WHEN jsonb_typeof(value #> '{progress,missing_episodes}') = 'array'
                            THEN jsonb_set(
                                value,
                                '{progress,missing_episodes}',
                                COALESCE(
                                    (
                                        SELECT jsonb_agg(
                                            CASE
                                                WHEN item #> '{season}' = '0'::jsonb
                                                THEN jsonb_set(item, '{season}', '1'::jsonb)
                                                ELSE item
                                            END
                                        )
                                        FROM jsonb_array_elements(
                                            value #> '{progress,missing_episodes}'
                                        ) AS missing(item)
                                    ),
                                    '[]'::jsonb
                                )
                            )
                            ELSE value
                        END AS value
                        FROM media_normalized
                    )
                    SELECT COALESCE(
                        (
                            candidate #> '{media,season}' = '0'::jsonb
                            OR EXISTS (
                                SELECT 1
                                FROM jsonb_array_elements(
                                    CASE
                                        WHEN jsonb_typeof(
                                            candidate #> '{progress,missing_episodes}'
                                        ) = 'array'
                                        THEN candidate #> '{progress,missing_episodes}'
                                        ELSE '[]'::jsonb
                                    END
                                ) AS missing(item)
                                WHERE item #> '{season}' = '0'::jsonb
                            )
                        )
                        AND (
                            notification_payload_v2_valid(normalized.value)
                            OR notification_payload_v2_valid_episode_number(normalized.value)
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
                SET payload = payload #- '{media,season}'
                WHERE notification_payload_v2_valid_specials(payload)
                  AND payload #> '{media,season}' = '0'::jsonb;

                UPDATE notification_outbox
                SET payload = jsonb_set(
                    payload,
                    '{progress,missing_episodes}',
                    COALESCE(
                        (
                            SELECT jsonb_agg(item)
                            FROM jsonb_array_elements(
                                payload #> '{progress,missing_episodes}'
                            ) AS missing(item)
                            WHERE item #> '{season}' <> '0'::jsonb
                        ),
                        '[]'::jsonb
                    )
                )
                WHERE notification_payload_v2_valid_specials(payload)
                  AND jsonb_typeof(payload #> '{progress,missing_episodes}') = 'array';

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
                    );

                DROP FUNCTION notification_payload_v2_valid_specials(jsonb);
                "#,
            )
            .await?;
        Ok(())
    }
}
