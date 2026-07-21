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
                CREATE FUNCTION notification_payload_v2_valid_episode_number(candidate jsonb)
                RETURNS boolean
                LANGUAGE SQL
                IMMUTABLE
                AS $function$
                    SELECT COALESCE(
                        jsonb_typeof(candidate->'progress') = 'object'
                        AND candidate->'progress' ? 'current_episode'
                        AND notification_unsigned_integer_in_range(
                            candidate->'progress'->'current_episode', 1, 4294967295
                        )
                        AND notification_payload_v2_valid(
                            jsonb_set(
                                candidate,
                                '{progress}',
                                (candidate->'progress') - 'current_episode'
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
                SET payload = payload #- '{progress,current_episode}'
                WHERE notification_payload_v2_valid_episode_number(payload)
                  AND NOT notification_payload_v2_valid(payload);

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
                    );

                DROP FUNCTION notification_payload_v2_valid_episode_number(jsonb);
                "#,
            )
            .await?;
        Ok(())
    }
}
