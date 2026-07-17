use media_core::{
    Job, JobId, JobStore, NewJob, OperationKey, PortError, QueueStatus, RunnerLifecycleState,
    TransferKind, TransferProgress, UserId,
};
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

const JOB_NOT_RETRYABLE: &str =
    "only blocked-storage, partial, failed, or needs-action jobs can be retried";

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
        let running_stage = self
            .database
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT s.name, s.checkpoint, s.updated_at FROM job_stages s \
                 JOIN job_tasks t ON s.task_id = t.id \
                 WHERE t.job_id = $1 AND s.state = 'running' AND s.name <> 'execution' \
                 ORDER BY t.ordinal DESC, s.ordinal DESC LIMIT 1",
                [id.into_uuid().into()],
            ))
            .await
            .map_err(map_database_error)?;
        let (current_stage, progress) = match running_stage {
            Some(row) => {
                let name = row
                    .try_get::<String>("", "name")
                    .map_err(map_database_error)?;
                let checkpoint = row
                    .try_get::<serde_json::Value>("", "checkpoint")
                    .map_err(map_database_error)?;
                let updated_at = row
                    .try_get::<time::OffsetDateTime>("", "updated_at")
                    .map_err(map_database_error)?;
                let progress = parse_transfer_progress(&name, &checkpoint, updated_at);
                (Some(name), progress)
            }
            None => (None, None),
        };
        Ok(Some(media_core::JobDetail {
            job,
            current_stage,
            progress,
        }))
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

    async fn retry(
        &self,
        operation: OperationKey,
        id: JobId,
        owner: UserId,
    ) -> Result<Option<Job>, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::RetryJob).await? {
                OperationClaim::Replay(OperationResult::Job(job)) => return Ok(Some(job)),
                OperationClaim::Replay(OperationResult::None) => return Ok(None),
                OperationClaim::Replay(OperationResult::Lease(_)) => {
                    return Err(sea_orm::DbErr::Type(
                        "retry-job operation has an invalid result".to_owned(),
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
                    OperationKind::RetryJob,
                    &OperationResult::None,
                )
                .await?;
                return Ok(None);
            };
            let state = row.try_get::<String>("", "state")?;
            if !matches!(
                state.as_str(),
                "blocked_storage" | "partial" | "failed" | "needs_action"
            ) {
                return Err(sea_orm::DbErr::Custom(JOB_NOT_RETRYABLE.to_owned()));
            }
            if matches!(state.as_str(), "blocked_storage" | "needs_action") {
                // A blocked outcome completes the runner's wrapper stages before
                // the job transition is reported, while a manual-action outcome
                // can leave its active task running. Reset the whole task ledger
                // so the next lease re-enters the pipeline with the new input.
                transaction
                    .execute_raw(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "UPDATE job_stages SET state = 'pending', attempt_count = 0, \
                         checkpoint = '{}'::jsonb, error_snapshot = NULL, started_at = NULL, \
                         completed_at = NULL, updated_at = now() WHERE task_id IN \
                         (SELECT id FROM job_tasks WHERE job_id = $1)",
                        [id.into_uuid().into()],
                    ))
                    .await?;
                transaction
                    .execute_raw(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "UPDATE job_tasks SET state = 'pending', attempt_count = 0, \
                         checkpoint = '{}'::jsonb, error_snapshot = NULL, started_at = NULL, \
                         completed_at = NULL, updated_at = now() WHERE job_id = $1",
                        [id.into_uuid().into()],
                    ))
                    .await?;
            }
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE job_stages SET state = 'pending', attempt_count = 0, \
                     error_snapshot = NULL, started_at = NULL, completed_at = NULL, updated_at = now() \
                     WHERE task_id IN (SELECT id FROM job_tasks WHERE job_id = $1) \
                     AND state = 'failed'",
                    [id.into_uuid().into()],
                ))
                .await?;
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE job_tasks SET state = 'pending', attempt_count = 0, \
                     error_snapshot = NULL, started_at = NULL, completed_at = NULL, updated_at = now() \
                     WHERE job_id = $1 AND state = 'failed'",
                    [id.into_uuid().into()],
                ))
                .await?;
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE jobs SET state = 'queued', needs_action_reason = NULL, \
                     error_snapshot = NULL, attempt_count = 0, started_at = NULL, \
                     completed_at = NULL, updated_at = now() WHERE id = $1",
                    [id.into_uuid().into()],
                ))
                .await?;
            insert_outbox(
                &transaction,
                id,
                "job.retried",
                operation.as_bytes().to_vec(),
                serde_json::json!({"state": "queued"}),
            )
            .await?;
            let model = job::Entity::find_by_id(id.into_uuid())
                .one(&transaction)
                .await?
                .ok_or_else(|| sea_orm::DbErr::RecordNotFound("job disappeared".to_owned()))?;
            let job = Job::try_from(model)
                .map_err(|_| sea_orm::DbErr::Type("invalid persisted job".to_owned()))?;
            operation::complete(
                &transaction,
                operation,
                OperationKind::RetryJob,
                &OperationResult::Job(job.clone()),
            )
            .await?;
            Ok(Some(job))
        }
        .await;
        match result {
            Ok(value) => {
                transaction.commit().await.map_err(map_database_error)?;
                Ok(value)
            }
            Err(sea_orm::DbErr::Custom(message)) if message == JOB_NOT_RETRYABLE => {
                transaction.rollback().await.map_err(map_database_error)?;
                Err(PortError::Conflict)
            }
            Err(error) => {
                transaction.rollback().await.map_err(map_database_error)?;
                Err(map_database_error(error))
            }
        }
    }

    async fn queue_status(&self) -> Result<QueueStatus, PortError> {
        let row = self
            .database
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT (SELECT COUNT(*) FROM jobs WHERE state = 'queued')::bigint AS queued, \
                 EXISTS (SELECT 1 FROM job_leases WHERE expires_at > now()) AS active, \
                 lifecycle.state AS runner_state, lifecycle.reason AS blocked_reason \
                 FROM runner_lifecycle lifecycle WHERE lifecycle.singleton = true",
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
        let runner_state = match row
            .try_get::<String>("", "runner_state")
            .map_err(|_| PortError::Infrastructure)?
            .as_str()
        {
            "ready" => RunnerLifecycleState::Ready,
            "rotating" => RunnerLifecycleState::Rotating,
            "blocked" => RunnerLifecycleState::Blocked,
            _ => return Err(PortError::Infrastructure),
        };
        let blocked_reason = row
            .try_get::<Option<String>>("", "blocked_reason")
            .map_err(|_| PortError::Infrastructure)?;
        Ok(QueueStatus {
            queued,
            active,
            runner_state,
            blocked_reason,
        })
    }
}

fn parse_transfer_progress(
    stage: &str,
    checkpoint: &serde_json::Value,
    updated_at: time::OffsetDateTime,
) -> Option<TransferProgress> {
    if !matches!(stage, "download" | "torrent_monitor") {
        return None;
    }
    let checkpoint = checkpoint.as_object()?;
    let kind = match checkpoint.get("kind")?.as_str()? {
        "direct" => TransferKind::Direct,
        "hls" => TransferKind::Hls,
        "torrent" => TransferKind::Torrent,
        _ => return None,
    };
    let state = optional_state(checkpoint.get("state"))?;
    let progress_percent = optional_unsigned(checkpoint.get("progress_percent"))?
        .map(u8::try_from)
        .transpose()
        .ok()?
        .filter(|value| *value <= 100);
    if checkpoint.contains_key("progress_percent") && progress_percent.is_none() {
        return None;
    }
    let downloaded_bytes = optional_unsigned(checkpoint.get("downloaded_bytes"))?;
    let total_bytes = optional_unsigned(checkpoint.get("total_bytes"))?;
    if downloaded_bytes
        .zip(total_bytes)
        .is_some_and(|(downloaded, total)| downloaded > total)
    {
        return None;
    }
    Some(TransferProgress {
        kind,
        state,
        progress_percent,
        downloaded_bytes,
        total_bytes,
        download_speed_bps: optional_unsigned(checkpoint.get("download_speed_bps"))?,
        eta_seconds: optional_unsigned(checkpoint.get("eta_seconds"))?,
        seeds: optional_unsigned(checkpoint.get("seeds"))?,
        peers: optional_unsigned(checkpoint.get("peers"))?,
        updated_at,
    })
}

fn optional_unsigned(value: Option<&serde_json::Value>) -> Option<Option<u64>> {
    match value {
        Some(value) => value.as_u64().map(Some),
        None => Some(None),
    }
}

fn optional_state(value: Option<&serde_json::Value>) -> Option<Option<String>> {
    let Some(value) = value else {
        return Some(None);
    };
    let value = value.as_str()?;
    if value.is_empty()
        || value.len() > 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    Some(Some(value.to_owned()))
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
