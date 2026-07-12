use media_core::{Job, JobId, JobStore, NewJob, OperationKey, PortError, QueueStatus, UserId};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Statement, TransactionTrait,
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
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO job_tasks (id, job_id, ordinal, state) \
                     VALUES ($1, $2, 0, 'pending')",
                    [uuid::Uuid::new_v4().into(), job.id().into_uuid().into()],
                ))
                .await?;
            insert_outbox(
                &transaction,
                job.id(),
                "job.created",
                operation.as_bytes().to_vec(),
                serde_json::json!({"state": "queued"}),
            )
            .await?;
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

    async fn find_detail_for_owner(
        &self,
        id: JobId,
        owner: UserId,
    ) -> Result<Option<media_core::JobDetail>, PortError> {
        let Some(job) = self.find_for_owner(id, owner).await? else {
            return Ok(None);
        };
        // The running stage, preferring the latest task and stage, answers "how is
        // my movie doing?" with the phase the runner is currently executing. The
        // internal "execution" wrapper spans the whole task and is always running,
        // so it is excluded by name; otherwise it would mask the real phase for
        // every single-task (torrent and movie) job. No running phase (queued,
        // publishing, terminal) yields no stage.
        let current_stage = self
            .database
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT s.name FROM job_stages s \
                 JOIN job_tasks t ON s.task_id = t.id \
                 WHERE t.job_id = $1 AND s.state = 'running' AND s.name <> 'execution' \
                 ORDER BY t.ordinal DESC, s.ordinal DESC LIMIT 1",
                [id.into_uuid().into()],
            ))
            .await
            .map_err(map_database_error)?
            .map(|row| row.try_get::<String>("", "name"))
            .transpose()
            .map_err(map_database_error)?;
        Ok(Some(media_core::JobDetail { job, current_stage }))
    }

    async fn list_for_owner(&self, owner: UserId) -> Result<Vec<Job>, PortError> {
        job::Entity::find()
            .filter(job::Column::OwnerId.eq(owner.into_uuid()))
            .order_by_desc(job::Column::CreatedAt)
            .all(&self.database)
            .await
            .map_err(map_database_error)?
            .into_iter()
            .map(Job::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_mapping_error)
    }

    async fn cancel(
        &self,
        operation: OperationKey,
        id: JobId,
        owner: UserId,
    ) -> Result<Option<Job>, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::CancelJob).await? {
                OperationClaim::Replay(OperationResult::Job(job)) => return Ok(Some(job)),
                OperationClaim::Replay(OperationResult::None) => return Ok(None),
                OperationClaim::Replay(OperationResult::Lease(_)) => {
                    return Err(sea_orm::DbErr::Type(
                        "cancel-job operation has an invalid result".to_owned(),
                    ));
                }
                OperationClaim::Fresh => {}
            }
            let Some(row) = transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT state FROM jobs WHERE id = $1 AND owner_id = $2 FOR UPDATE",
                    [id.into_uuid().into(), owner.into_uuid().into()],
                ))
                .await?
            else {
                operation::complete(
                    &transaction,
                    operation,
                    OperationKind::CancelJob,
                    &OperationResult::None,
                )
                .await?;
                return Ok(None);
            };
            let current = row.try_get::<String>("", "state")?;
            let target = match current.as_str() {
                "queued" => "cancelled",
                "leased" | "running" | "blocked_storage" | "publishing" | "plex_pending"
                => "cancel_requested",
                "needs_action" => "cancelled",
                "cancel_requested" | "cancelled" => current.as_str(),
                "completed" | "partial" | "failed" => {
                    return Err(sea_orm::DbErr::Custom(
                        "terminal job cannot be cancelled".to_owned(),
                    ));
                }
                _ => return Err(sea_orm::DbErr::Type("invalid persisted job state".to_owned())),
            };
            if target != current {
                transaction
                    .execute_raw(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "UPDATE jobs SET state = $2, updated_at = now(), \
                         completed_at = CASE WHEN $2 = 'cancelled' THEN now() ELSE completed_at END \
                         WHERE id = $1",
                        [id.into_uuid().into(), target.into()],
                    ))
                    .await?;
                insert_outbox(
                    &transaction,
                    id,
                    if target == "cancelled" {
                        "job.cancelled"
                    } else {
                        "job.cancel_requested"
                    },
                    operation.as_bytes().to_vec(),
                    serde_json::json!({"state": target}),
                )
                .await?;
            }
            let model = job::Entity::find_by_id(id.into_uuid())
                .one(&transaction)
                .await?
                .ok_or_else(|| sea_orm::DbErr::RecordNotFound("job disappeared".to_owned()))?;
            let job = Job::try_from(model)
                .map_err(|_| sea_orm::DbErr::Type("invalid persisted job".to_owned()))?;
            operation::complete(
                &transaction,
                operation,
                OperationKind::CancelJob,
                &OperationResult::Job(job.clone()),
            )
            .await?;
            Ok(Some(job))
        }
        .await;
        finish(transaction, result).await
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

pub(crate) async fn insert_outbox(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: JobId,
    event_type: &str,
    dedupe_key: Vec<u8>,
    payload: serde_json::Value,
) -> Result<(), sea_orm::DbErr> {
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO outbox_events \
             (id, aggregate_type, aggregate_id, event_type, dedupe_key, payload) \
             VALUES ($1, 'job', $2, $3, $4, $5) ON CONFLICT (dedupe_key) DO NOTHING",
            [
                uuid::Uuid::new_v4().into(),
                job_id.into_uuid().into(),
                event_type.into(),
                dedupe_key.into(),
                payload.into(),
            ],
        ))
        .await?;
    Ok(())
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
