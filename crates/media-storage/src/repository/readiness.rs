use media_core::{PortError, ReadinessPort};
use sea_orm::DatabaseConnection;
use sea_orm_migration::MigratorTrait;

use crate::{Migrator, repository::map_database_error};

#[derive(Clone)]
pub struct SeaOrmReadiness {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmReadiness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmReadiness { database: [REDACTED] }")
    }
}

impl SeaOrmReadiness {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl ReadinessPort for SeaOrmReadiness {
    async fn is_ready(&self) -> Result<bool, PortError> {
        self.database.ping().await.map_err(map_database_error)?;
        Migrator::get_pending_migrations(&self.database)
            .await
            .map(|pending| pending.is_empty())
            .map_err(map_database_error)
    }
}
