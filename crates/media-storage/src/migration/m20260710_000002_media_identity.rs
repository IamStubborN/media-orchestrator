use super::finish_transaction;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::TransactionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = async {
            let db = &transaction;

            db.execute_unprepared(
                r#"
            CREATE TABLE media (
                id uuid PRIMARY KEY,
                kind text NOT NULL,
                title text NOT NULL,
                release_year integer,
                series_ordering text,
                metadata_snapshot jsonb NOT NULL DEFAULT '{}'::jsonb,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT media_kind_check CHECK (kind IN ('movie', 'series')),
                CONSTRAINT media_title_not_blank CHECK (btrim(title) <> ''),
                CONSTRAINT media_release_year_check
                    CHECK (release_year IS NULL OR release_year BETWEEN 1878 AND 9999),
                CONSTRAINT media_kind_ordering_check CHECK (
                    (kind = 'movie' AND series_ordering IS NULL)
                    OR (
                        kind = 'series'
                        AND series_ordering IS NOT NULL
                        AND series_ordering IN (
                            'tmdb_aired', 'tvdb_aired', 'tvdb_dvd', 'tvdb_absolute'
                        )
                    )
                ),
                CONSTRAINT media_metadata_snapshot_object_check
                    CHECK (jsonb_typeof(metadata_snapshot) = 'object')
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            CREATE TABLE media_external_refs (
                id uuid PRIMARY KEY,
                media_id uuid NOT NULL REFERENCES media(id) ON DELETE CASCADE,
                namespace text NOT NULL,
                value text NOT NULL,
                source text NOT NULL,
                provider_snapshot jsonb,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT media_external_refs_namespace_check CHECK (
                    namespace IN (
                        'tmdb', 'tvdb', 'imdb', 'anilist', 'rezka', 'plex',
                        'prowlarr_result'
                    )
                ),
                CONSTRAINT media_external_refs_value_not_blank CHECK (btrim(value) <> ''),
                CONSTRAINT media_external_refs_source_check
                    CHECK (source IN ('discovered', 'confirmed_by_user')),
                CONSTRAINT media_external_refs_snapshot_object_check CHECK (
                    provider_snapshot IS NULL OR jsonb_typeof(provider_snapshot) = 'object'
                ),
                CONSTRAINT media_external_refs_namespace_value_key UNIQUE (namespace, value)
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            CREATE TABLE seasons (
                id uuid PRIMARY KEY,
                media_id uuid NOT NULL REFERENCES media(id) ON DELETE CASCADE,
                season_number integer NOT NULL,
                title text,
                metadata_snapshot jsonb NOT NULL DEFAULT '{}'::jsonb,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT seasons_number_check CHECK (season_number >= 0),
                CONSTRAINT seasons_title_not_blank
                    CHECK (title IS NULL OR btrim(title) <> ''),
                CONSTRAINT seasons_metadata_snapshot_object_check
                    CHECK (jsonb_typeof(metadata_snapshot) = 'object'),
                CONSTRAINT seasons_media_number_key UNIQUE (media_id, season_number)
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            CREATE TABLE episodes (
                id uuid PRIMARY KEY,
                season_id uuid NOT NULL REFERENCES seasons(id) ON DELETE CASCADE,
                episode_number integer NOT NULL,
                absolute_number integer,
                title text,
                metadata_snapshot jsonb NOT NULL DEFAULT '{}'::jsonb,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT episodes_number_check CHECK (episode_number >= 0),
                CONSTRAINT episodes_absolute_number_check
                    CHECK (absolute_number IS NULL OR absolute_number >= 0),
                CONSTRAINT episodes_title_not_blank
                    CHECK (title IS NULL OR btrim(title) <> ''),
                CONSTRAINT episodes_metadata_snapshot_object_check
                    CHECK (jsonb_typeof(metadata_snapshot) = 'object'),
                CONSTRAINT episodes_season_number_key UNIQUE (season_id, episode_number)
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                r#"
            CREATE TABLE episode_provider_mappings (
                id uuid PRIMARY KEY,
                episode_id uuid NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
                provider text NOT NULL,
                provider_media_ref text NOT NULL,
                provider_season_number integer NOT NULL,
                provider_episode_number integer NOT NULL,
                source text NOT NULL,
                provider_snapshot jsonb,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT episode_provider_mappings_provider_check
                    CHECK (provider IN ('rezka', 'prowlarr')),
                CONSTRAINT episode_provider_mappings_media_ref_not_blank
                    CHECK (btrim(provider_media_ref) <> ''),
                CONSTRAINT episode_provider_mappings_season_number_check
                    CHECK (provider_season_number >= 0),
                CONSTRAINT episode_provider_mappings_episode_number_check
                    CHECK (provider_episode_number >= 0),
                CONSTRAINT episode_provider_mappings_source_check
                    CHECK (source IN ('discovered', 'confirmed_by_user')),
                CONSTRAINT episode_provider_mappings_snapshot_object_check CHECK (
                    provider_snapshot IS NULL OR jsonb_typeof(provider_snapshot) = 'object'
                ),
                CONSTRAINT episode_provider_mappings_provider_coordinate_key UNIQUE (
                    provider, provider_media_ref, provider_season_number,
                    provider_episode_number
                )
            )
            "#,
            )
            .await?;

            db.execute_unprepared(
                "CREATE INDEX media_external_refs_media_id_idx ON media_external_refs (media_id)",
            )
            .await?;
            db.execute_unprepared(
                "CREATE INDEX episode_provider_mappings_episode_id_idx \
             ON episode_provider_mappings (episode_id)",
            )
            .await?;

            Ok::<(), DbErr>(())
        }
        .await;

        finish_transaction(transaction, result).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let transaction = manager.get_connection().begin().await?;
        let result = async {
            let db = &transaction;
            db.execute_unprepared("DROP INDEX episode_provider_mappings_episode_id_idx")
                .await?;
            db.execute_unprepared("DROP INDEX media_external_refs_media_id_idx")
                .await?;
            db.execute_unprepared("DROP TABLE episode_provider_mappings")
                .await?;
            db.execute_unprepared("DROP TABLE episodes").await?;
            db.execute_unprepared("DROP TABLE seasons").await?;
            db.execute_unprepared("DROP TABLE media_external_refs")
                .await?;
            db.execute_unprepared("DROP TABLE media").await?;
            Ok::<(), DbErr>(())
        }
        .await;

        finish_transaction(transaction, result).await
    }
}
