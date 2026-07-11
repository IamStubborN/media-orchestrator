use media_core::{
    CanonicalEpisode, CanonicalMedia, CanonicalSeason, EpisodeProviderMapping, ExternalNamespace,
    IdentityStore, MediaExternalReference, PortError,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::{
    entity::{episode, episode_provider_mapping, media, media_external_ref, season},
    mapping::{
        episode_active_model, episode_mapping_active_model, external_namespace_value,
        external_reference_active_model, media_active_model, season_active_model,
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
}
