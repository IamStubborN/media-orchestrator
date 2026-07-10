use media_core::{Job, JobId, JobStore, NewJob, PortError, QueueStatus, UserId};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, QueryFilter,
    Statement,
};

use crate::{
    entity::job,
    mapping::job_active_model,
    repository::{map_database_error, map_mapping_error},
};

#[derive(Clone)]
pub struct SeaOrmJobStore {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmJobStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmJobStore { database: [REDACTED] }")
    }
}

impl SeaOrmJobStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl JobStore for SeaOrmJobStore {
    async fn create(&self, value: NewJob) -> Result<Job, PortError> {
        job::Entity::insert(job_active_model(&value))
            .exec_with_returning(&self.database)
            .await
            .map_err(map_database_error)?
            .try_into()
            .map_err(map_mapping_error)
    }

    async fn find_for_owner(&self, id: JobId, owner: UserId) -> Result<Option<Job>, PortError> {
        job::Entity::find()
            .filter(job::Column::Id.eq(id.into_uuid()))
            .filter(job::Column::OwnerId.eq(owner.into_uuid()))
            .one(&self.database)
            .await
            .map_err(map_database_error)?
            .map(Job::try_from)
            .transpose()
            .map_err(map_mapping_error)
    }

    async fn queue_status(&self) -> Result<QueueStatus, PortError> {
        let row = self
            .database
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*) FILTER (WHERE state = 'queued')::bigint AS queued, \
                 EXISTS (SELECT 1 FROM job_leases WHERE expires_at > now()) AS active \
                 FROM jobs",
            ))
            .await
            .map_err(map_database_error)?
            .ok_or(PortError::Infrastructure)?;
        let queued = row
            .try_get::<i64>("", "queued")
            .ok()
            .and_then(|count| u64::try_from(count).ok())
            .ok_or(PortError::Infrastructure)?;
        let active = row
            .try_get::<bool>("", "active")
            .map_err(|_| PortError::Infrastructure)?;
        Ok(QueueStatus { queued, active })
    }
}
