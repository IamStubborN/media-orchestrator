use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, BootstrapClient, ClientRole, ClientStore,
    CredentialDigest, LIFECYCLE_CLIENT_ID, PortError, RUNNER_CLIENT_ID, SECONDARY_CLIENT_ID,
    SECONDARY_USER_ID,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, sea_query::OnConflict};

use crate::{
    entity::api_client,
    mapping::client_active_model,
    repository::{map_database_error, map_mapping_error},
};

#[derive(Clone)]
pub struct SeaOrmClientStore {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmClientStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmClientStore { database: [REDACTED] }")
    }
}

impl SeaOrmClientStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl ClientStore for SeaOrmClientStore {
    async fn find_by_digest(&self, digest: CredentialDigest) -> Result<Option<Actor>, PortError> {
        api_client::Entity::find()
            .filter(api_client::Column::CredentialDigest.eq(digest.as_bytes().to_vec()))
            .filter(api_client::Column::Enabled.eq(true))
            .one(&self.database)
            .await
            .map_err(map_database_error)?
            .map(Actor::try_from)
            .transpose()
            .map_err(map_mapping_error)
    }

    async fn upsert_client(&self, client: BootstrapClient) -> Result<(), PortError> {
        if !is_fixed_client(&client) {
            return Err(PortError::Conflict);
        }

        api_client::Entity::insert(client_active_model(&client))
            .on_conflict(
                OnConflict::column(api_client::Column::Id)
                    .update_columns([
                        api_client::Column::Name,
                        api_client::Column::CredentialDigest,
                        api_client::Column::Enabled,
                        api_client::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.database)
            .await
            .map_err(map_database_error)?;
        Ok(())
    }
}

fn is_fixed_client(client: &BootstrapClient) -> bool {
    matches!(
        (client.client_id(), client.role(), client.user_id()),
        (PRIMARY_CLIENT_ID, ClientRole::Hermes, Some(PRIMARY_USER_ID))
            | (
                SECONDARY_CLIENT_ID,
                ClientRole::Hermes,
                Some(SECONDARY_USER_ID)
            )
            | (RUNNER_CLIENT_ID, ClientRole::Runner, None)
            | (LIFECYCLE_CLIENT_ID, ClientRole::Lifecycle, None)
    )
}
