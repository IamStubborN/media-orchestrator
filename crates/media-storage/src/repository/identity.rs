use media_core::{
    CanonicalEpisode, CanonicalEpisodeCoordinates, CanonicalMedia, CanonicalSeason, EpisodeId,
    EpisodeMappingConfirmation, EpisodeProviderMapping, ExternalNamespace, IdentityStore,
    MediaExternalReference, PortError, Provider,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, QueryFilter,
    Statement, TransactionTrait,
};

use crate::{
    entity::{episode, episode_provider_mapping, media, media_external_ref, season},
    mapping::{
        episode_active_model, episode_mapping_active_model, external_namespace_value,
        external_reference_active_model, media_active_model, provider_value, season_active_model,
    },
    repository::{map_database_error, map_mapping_error},
};

#[derive(Clone)]
pub struct SeaOrmIdentityStore {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmIdentityStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmIdentityStore { database: [REDACTED] }")
    }
}

impl SeaOrmIdentityStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl IdentityStore for SeaOrmIdentityStore {
    async fn create_media(&self, value: CanonicalMedia) -> Result<CanonicalMedia, PortError> {
        media::Entity::insert(media_active_model(&value))
            .exec_with_returning(&self.database)
            .await
            .map_err(map_database_error)?
            .try_into()
            .map_err(map_mapping_error)
    }

    async fn add_external_reference(
        &self,
        value: MediaExternalReference,
    ) -> Result<MediaExternalReference, PortError> {
        media_external_ref::Entity::insert(external_reference_active_model(&value))
            .exec_with_returning(&self.database)
            .await
            .map_err(map_database_error)?
            .try_into()
            .map_err(map_mapping_error)
    }

    async fn find_media_by_external_reference(
        &self,
        namespace: ExternalNamespace,
        value: &str,
    ) -> Result<Option<CanonicalMedia>, PortError> {
        let reference = media_external_ref::Entity::find()
            .filter(media_external_ref::Column::Namespace.eq(external_namespace_value(namespace)))
            .filter(media_external_ref::Column::Value.eq(value))
            .one(&self.database)
            .await
            .map_err(map_database_error)?;
        let Some(reference) = reference else {
            return Ok(None);
        };

        media::Entity::find_by_id(reference.media_id)
            .one(&self.database)
            .await
            .map_err(map_database_error)?
            .map(CanonicalMedia::try_from)
            .transpose()
            .map_err(map_mapping_error)
    }

    async fn create_season(&self, value: CanonicalSeason) -> Result<CanonicalSeason, PortError> {
        let active = season_active_model(&value).map_err(map_mapping_error)?;
        season::Entity::insert(active)
            .exec_with_returning(&self.database)
            .await
            .map_err(map_database_error)?
            .try_into()
            .map_err(map_mapping_error)
    }

    async fn create_episode(&self, value: CanonicalEpisode) -> Result<CanonicalEpisode, PortError> {
        let active = episode_active_model(&value).map_err(map_mapping_error)?;
        episode::Entity::insert(active)
            .exec_with_returning(&self.database)
            .await
            .map_err(map_database_error)?
            .try_into()
            .map_err(map_mapping_error)
    }

    async fn save_episode_mapping(
        &self,
        value: EpisodeProviderMapping,
    ) -> Result<EpisodeProviderMapping, PortError> {
        let active = episode_mapping_active_model(&value).map_err(map_mapping_error)?;
        episode_provider_mapping::Entity::insert(active)
            .exec_with_returning(&self.database)
            .await
            .map_err(map_database_error)?
            .try_into()
            .map_err(map_mapping_error)
    }

    async fn find_episode_mapping(
        &self,
        provider: Provider,
        provider_media_ref: &str,
        provider_season: u32,
        provider_episode: u32,
    ) -> Result<Option<CanonicalEpisodeCoordinates>, PortError> {
        find_episode_mapping(
            &self.database,
            provider,
            provider_media_ref,
            provider_season,
            provider_episode,
        )
        .await
    }

    async fn confirm_episode_mapping(
        &self,
        confirmation: EpisodeMappingConfirmation,
    ) -> Result<CanonicalEpisodeCoordinates, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let lock_key = format!(
            "{}:{}:{}:{}",
            provider_value(confirmation.provider()),
            confirmation.provider_media_ref(),
            confirmation.provider_season(),
            confirmation.provider_episode(),
        );
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                [lock_key.into()],
            ))
            .await
            .map_err(map_database_error)?;

        let namespace = match confirmation.provider() {
            Provider::Rezka => "rezka",
            Provider::Prowlarr => "prowlarr_result",
        };
        let media_id = transaction
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT media_id FROM media_external_refs WHERE namespace = $1 AND value = $2",
                [namespace.into(), confirmation.provider_media_ref().into()],
            ))
            .await
            .map_err(map_database_error)?
            .map(|row| row.try_get::<uuid::Uuid>("", "media_id"))
            .transpose()
            .map_err(map_database_error)?
            .unwrap_or_else(uuid::Uuid::new_v4);

        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO media (id, kind, title, release_year, series_ordering) \
                 VALUES ($1, 'series', $2, $3, 'tvdb_aired') \
                 ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title, \
                 release_year = COALESCE(EXCLUDED.release_year, media.release_year), updated_at = now()",
                [
                    media_id.into(),
                    confirmation.title().into(),
                    confirmation.release_year().into(),
                ],
            ))
            .await
            .map_err(map_database_error)?;
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO media_external_refs (id, media_id, namespace, value, source) \
                 VALUES ($1, $2, $3, $4, 'confirmed_by_user') \
                 ON CONFLICT (namespace, value) DO UPDATE SET source = 'confirmed_by_user', updated_at = now()",
                [
                    uuid::Uuid::new_v4().into(),
                    media_id.into(),
                    namespace.into(),
                    confirmation.provider_media_ref().into(),
                ],
            ))
            .await
            .map_err(map_database_error)?;

        let season_id = uuid::Uuid::new_v4();
        let season_row = transaction
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO seasons (id, media_id, season_number) VALUES ($1, $2, $3) \
                 ON CONFLICT (media_id, season_number) DO UPDATE SET updated_at = now() \
                 RETURNING id",
                [
                    season_id.into(),
                    media_id.into(),
                    i64::from(confirmation.canonical_season()).into(),
                ],
            ))
            .await
            .map_err(map_database_error)?
            .ok_or(PortError::Infrastructure)?;
        let season_id = season_row
            .try_get::<uuid::Uuid>("", "id")
            .map_err(map_database_error)?;

        let episode_id = uuid::Uuid::new_v4();
        let episode_row = transaction
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO episodes (id, season_id, episode_number) VALUES ($1, $2, $3) \
                 ON CONFLICT (season_id, episode_number) DO UPDATE SET updated_at = now() \
                 RETURNING id",
                [
                    episode_id.into(),
                    season_id.into(),
                    i64::from(confirmation.canonical_episode()).into(),
                ],
            ))
            .await
            .map_err(map_database_error)?
            .ok_or(PortError::Infrastructure)?;
        let episode_id = episode_row
            .try_get::<uuid::Uuid>("", "id")
            .map_err(map_database_error)?;

        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO episode_provider_mappings \
                 (id, episode_id, provider, provider_media_ref, provider_season_number, \
                  provider_episode_number, source) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'confirmed_by_user') \
                 ON CONFLICT (provider, provider_media_ref, provider_season_number, \
                              provider_episode_number) \
                 DO UPDATE SET episode_id = EXCLUDED.episode_id, source = 'confirmed_by_user', \
                               updated_at = now()",
                [
                    uuid::Uuid::new_v4().into(),
                    episode_id.into(),
                    provider_value(confirmation.provider()).into(),
                    confirmation.provider_media_ref().into(),
                    i64::from(confirmation.provider_season()).into(),
                    i64::from(confirmation.provider_episode()).into(),
                ],
            ))
            .await
            .map_err(map_database_error)?;
        transaction.commit().await.map_err(map_database_error)?;

        Ok(CanonicalEpisodeCoordinates::new(
            EpisodeId::from_uuid(episode_id),
            confirmation.canonical_season(),
            confirmation.canonical_episode(),
            confirmation.title().to_owned(),
        ))
    }
}

async fn find_episode_mapping(
    database: &impl ConnectionTrait,
    provider: Provider,
    provider_media_ref: &str,
    provider_season: u32,
    provider_episode: u32,
) -> Result<Option<CanonicalEpisodeCoordinates>, PortError> {
    let row = database
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT e.id AS episode_id, s.season_number, e.episode_number, m.title AS media_title \
             FROM episode_provider_mappings mapping \
             JOIN episodes e ON e.id = mapping.episode_id \
             JOIN seasons s ON s.id = e.season_id \
             JOIN media m ON m.id = s.media_id \
             WHERE mapping.provider = $1 AND mapping.provider_media_ref = $2 \
               AND mapping.provider_season_number = $3 \
               AND mapping.provider_episode_number = $4",
            [
                provider_value(provider).into(),
                provider_media_ref.into(),
                i64::from(provider_season).into(),
                i64::from(provider_episode).into(),
            ],
        ))
        .await
        .map_err(map_database_error)?;
    row.map(|row| {
        let episode_id = row
            .try_get::<uuid::Uuid>("", "episode_id")
            .map_err(map_database_error)?;
        let season = row
            .try_get::<i32>("", "season_number")
            .map_err(map_database_error)?;
        let episode = row
            .try_get::<i32>("", "episode_number")
            .map_err(map_database_error)?;
        let media_title = row
            .try_get::<String>("", "media_title")
            .map_err(map_database_error)?;
        Ok(CanonicalEpisodeCoordinates::new(
            EpisodeId::from_uuid(episode_id),
            u32::try_from(season).map_err(|_| PortError::Infrastructure)?,
            u32::try_from(episode).map_err(|_| PortError::Infrastructure)?,
            media_title,
        ))
    })
    .transpose()
}
