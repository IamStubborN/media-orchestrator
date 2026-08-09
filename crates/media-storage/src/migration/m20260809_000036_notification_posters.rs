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
                CREATE FUNCTION notification_payload_v2_valid_poster(candidate jsonb)
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    WITH normalized AS (
                        SELECT jsonb_set(
                            candidate,
                            '{media}',
                            (candidate->'media') - 'poster_url'
                        ) AS value
                    )
                    SELECT COALESCE(
                        jsonb_typeof(candidate->'media') = 'object'
                        AND candidate->'media' ? 'poster_url'
                        AND jsonb_typeof(candidate->'media'->'poster_url') = 'string'
                        AND length(candidate->'media'->>'poster_url') BETWEEN 1 AND 2048
                        AND candidate->'media'->>'poster_url'
                            ~ '^https://[^/@[:space:]]+([/?#][^[:space:]]*)?$'
                        AND (
                            notification_payload_v2_valid((SELECT value FROM normalized))
                            OR notification_payload_v2_valid_episode_number((SELECT value FROM normalized))
                            OR notification_payload_v2_valid_specials((SELECT value FROM normalized))
                            OR notification_payload_v2_valid_detailed((SELECT value FROM normalized))
                            OR notification_payload_v2_valid_transfer_details((SELECT value FROM normalized))
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
                        OR notification_payload_v2_valid_poster(payload)
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
                    '{media}',
                    (payload->'media') - 'poster_url'
                )
                WHERE jsonb_typeof(payload->'media') = 'object'
                  AND payload->'media' ? 'poster_url';

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

                DROP FUNCTION notification_payload_v2_valid_poster(jsonb);
                "#,
            )
            .await?;
        Ok(())
    }
}
