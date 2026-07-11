use media_core::{Job, JobId, JobStore, NewJob, OperationKey, PortError, QueueStatus, UserId};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, QueryFilter,
    Statement, TransactionTrait,
};

use crate::{
    entity::job,
    mapping::job_active_model,
    repository::{
        map_database_error, map_mapping_error,
        operation::{self, OperationClaim, OperationKind, OperationResult},
    },
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
    async fn create(&self, operation: OperationKey, value: NewJob) -> Result<Job, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::CreateJob).await? {
                OperationClaim::Replay(OperationResult::Job(job)) => return Ok(job),
                OperationClaim::Replay(_) => {
                    return Err(sea_orm::DbErr::Type(
                        "create-job operation has an invalid result".to_owned(),
                    ));
                }
                OperationClaim::Fresh => {}
            }

            let model = job::Entity::insert(job_active_model(&value))
                .exec_with_returning(&transaction)
                .await?;
            let job = Job::try_from(model)
                .map_err(|_| sea_orm::DbErr::Type("invalid persisted job".to_owned()))?;
            operation::complete(
                &transaction,
                operation,
                OperationKind::CreateJob,
                &OperationResult::Job(job.clone()),
            )
            .await?;
            Ok(job)
        }
        .await;
        finish(transaction, result).await
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

async fn finish<T>(
    transaction: sea_orm::DatabaseTransaction,
    result: Result<T, sea_orm::DbErr>,
) -> Result<T, PortError> {
    match result {
        Ok(value) => {
            transaction.commit().await.map_err(map_database_error)?;
            Ok(value)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(map_database_error(error))
        }
    }
}
