use media_core::{
    Checkpoint, CheckpointValue, ClientId, Job, JobEvent, JobEventKind, JobLease, JobState,
    LeaseId, LeaseStore, OperationKey, PortError, StageFailureOutcome, StageRef,
};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, Statement, TransactionTrait,
    prelude::Uuid,
};

use crate::{
    entity::job,
    mapping::{job_state_value, needs_action_reason_value},
    repository::{
        job::insert_outbox,
        map_database_error,
        operation::{self, OperationClaim, OperationKind, OperationResult},
    },
};

const LEASE_ADVISORY_LOCK: i64 = 0x4d45_4449_414c_5345;

#[derive(Clone)]
pub struct SeaOrmLeaseStore {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmLeaseStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmLeaseStore { database: [REDACTED] }")
    }
}

impl SeaOrmLeaseStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl LeaseStore for SeaOrmLeaseStore {
    async fn lease_next(
        &self,
        operation: OperationKey,
        runner: ClientId,
        ttl: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        let ttl_seconds = valid_ttl_seconds(ttl)?;
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::LeaseNext).await? {
                OperationClaim::Replay(result) => return replayed_lease(result),
                OperationClaim::Fresh => {}
            }
            let lease = lease_next_in_transaction(&transaction, runner, ttl_seconds).await?;
            let stored = lease
                .clone()
                .map_or(OperationResult::None, OperationResult::Lease);
            operation::complete(&transaction, operation, OperationKind::LeaseNext, &stored).await?;
            Ok(lease)
        }
        .await;
        finish(transaction, result).await
    }

    async fn heartbeat(
        &self,
        operation: OperationKey,
        lease: LeaseId,
        runner: ClientId,
        ttl: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        let ttl_seconds = valid_ttl_seconds(ttl)?;
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::Heartbeat).await? {
                OperationClaim::Replay(result) => return replayed_lease(result),
                OperationClaim::Fresh => {}
            }
            let renewed =
                heartbeat_in_transaction(&transaction, lease, runner, ttl_seconds).await?;
            let stored = renewed
                .clone()
                .map_or(OperationResult::None, OperationResult::Lease);
            operation::complete(&transaction, operation, OperationKind::Heartbeat, &stored).await?;
            Ok(renewed)
        }
        .await;
        finish(transaction, result).await
    }

    async fn report_event(
        &self,
        operation: OperationKey,
        lease: LeaseId,
        runner: ClientId,
        event: JobEvent,
    ) -> Result<Option<Job>, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::ReportEvent).await? {
                OperationClaim::Replay(OperationResult::Job(job)) => return Ok(Some(job)),
                OperationClaim::Replay(OperationResult::None) => return Ok(None),
                OperationClaim::Replay(OperationResult::Lease(_)) => {
                    return Err(sea_orm::DbErr::Type(
                        "report-event operation has an invalid result".to_owned(),
                    ));
                }
                OperationClaim::Fresh => {}
            }
            let Some(lease_row) = transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT job_id FROM job_leases WHERE id = $1 AND runner_client_id = $2 \
                     AND expires_at > now() FOR UPDATE",
                    [lease.into_uuid().into(), runner.into_uuid().into()],
                ))
                .await?
            else {
                operation::complete(
                    &transaction,
                    operation,
                    OperationKind::ReportEvent,
                    &OperationResult::None,
                )
                .await?;
                return Ok(None);
            };
            let job_id = lease_row.try_get::<Uuid>("", "job_id")?;
            let current = load_job(&transaction, job_id).await?;
            let (event_type, payload) = event_payload(&event);
            let inserted = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO job_events \
                     (id, job_id, lease_id, runner_client_id, event_type, payload) \
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
                    [
                        event.id().into_uuid().into(),
                        job_id.into(),
                        lease.into_uuid().into(),
                        runner.into_uuid().into(),
                        event_type.into(),
                        payload.clone().into(),
                    ],
                ))
                .await?;
            let updated = if inserted.rows_affected() == 0 {
                current
            } else {
                apply_event(&transaction, lease, current, &event).await?
            };
            if inserted.rows_affected() == 1 {
                insert_outbox(
                    &transaction,
                    updated.id(),
                    event_type,
                    event.id().into_uuid().as_bytes().to_vec(),
                    payload,
                )
                .await?;
            }
            operation::complete(
                &transaction,
                operation,
                OperationKind::ReportEvent,
                &OperationResult::Job(updated.clone()),
            )
            .await?;
            Ok(Some(updated))
        }
        .await;
        finish(transaction, result).await
    }
}

fn replayed_lease(result: OperationResult) -> Result<Option<JobLease>, sea_orm::DbErr> {
    match result {
        OperationResult::Lease(lease) => Ok(Some(lease)),
        OperationResult::None => Ok(None),
        OperationResult::Job(_) => Err(sea_orm::DbErr::Type(
            "lease operation has an invalid result".to_owned(),
        )),
    }
}

async fn apply_event(
    transaction: &sea_orm::DatabaseTransaction,
    lease: LeaseId,
    current: Job,
    event: &JobEvent,
) -> Result<Job, sea_orm::DbErr> {
    match event.kind() {
        JobEventKind::Started => {
            transition_job(transaction, &current, JobState::Running, None).await?;
        }
        JobEventKind::StageStarted(stage) => {
            require_running(&current)?;
            start_stage(transaction, current.id(), stage).await?;
        }
        JobEventKind::StageCheckpoint { stage, checkpoint } => {
            require_running(&current)?;
            update_stage_checkpoint(transaction, current.id(), stage, checkpoint, false).await?;
        }
        JobEventKind::StageCompleted { stage, checkpoint } => {
            require_running(&current)?;
            update_stage_checkpoint(transaction, current.id(), stage, checkpoint, true).await?;
        }
        JobEventKind::StageFailed {
            stage,
            retryable,
            error_code,
        } => {
            require_running(&current)?;
            let terminal =
                fail_stage(transaction, current.id(), stage, *retryable, error_code).await?;
            if terminal {
                transition_job(transaction, &current, JobState::Failed, None).await?;
                release_lease(transaction, lease).await?;
            }
        }
        JobEventKind::JobTransition {
            state,
            needs_action_reason,
        } => {
            transition_job(transaction, &current, *state, *needs_action_reason).await?;
            if matches!(
                state,
                JobState::NeedsAction
                    | JobState::Partial
                    | JobState::Completed
                    | JobState::Failed
                    | JobState::Cancelled
            ) {
                finish_tasks(transaction, current.id(), *state).await?;
                release_lease(transaction, lease).await?;
            }
        }
    }
    load_job(transaction, current.id().into_uuid()).await
}

fn require_running(job: &Job) -> Result<(), sea_orm::DbErr> {
    if job.state() == JobState::Running {
        Ok(())
    } else {
        Err(sea_orm::DbErr::Custom(
            "stage event requires a running job".to_owned(),
        ))
    }
}

async fn transition_job(
    transaction: &sea_orm::DatabaseTransaction,
    current: &Job,
    target: JobState,
    reason: Option<media_core::NeedsActionReason>,
) -> Result<(), sea_orm::DbErr> {
    current
        .state()
        .transition(target)
        .map_err(|_| sea_orm::DbErr::Custom("invalid job transition".to_owned()))?;
    let updated = transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE jobs SET state = $2, needs_action_reason = $3, updated_at = now(), \
             started_at = CASE WHEN $2 = 'running' THEN COALESCE(started_at, now()) ELSE started_at END, \
             completed_at = CASE WHEN $2 IN ('partial', 'completed', 'failed', 'cancelled') \
             THEN now() ELSE completed_at END WHERE id = $1 AND state = $4",
            [
                current.id().into_uuid().into(),
                job_state_value(target).into(),
                reason.map(needs_action_reason_value).into(),
                job_state_value(current.state()).into(),
            ],
        ))
        .await?;
    if updated.rows_affected() != 1 {
        return Err(sea_orm::DbErr::RecordNotUpdated);
    }
    Ok(())
}

async fn ensure_task(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    ordinal: u32,
) -> Result<Uuid, sea_orm::DbErr> {
    let ordinal = i32::try_from(ordinal)
        .map_err(|_| sea_orm::DbErr::Type("task ordinal is out of range".to_owned()))?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO job_tasks (id, job_id, ordinal, state, started_at) \
             VALUES ($1, $2, $3, 'running', now()) ON CONFLICT (job_id, ordinal) \
             DO UPDATE SET state = 'running', started_at = COALESCE(job_tasks.started_at, now()), \
             updated_at = now() WHERE job_tasks.state IN ('pending', 'running') RETURNING id",
            [
                Uuid::new_v4().into(),
                job_id.into_uuid().into(),
                ordinal.into(),
            ],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("task is not resumable".to_owned()))?;
    row.try_get("", "id")
}

async fn find_task(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    ordinal: u32,
) -> Result<Uuid, sea_orm::DbErr> {
    let ordinal = i32::try_from(ordinal)
        .map_err(|_| sea_orm::DbErr::Type("task ordinal is out of range".to_owned()))?;
    transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM job_tasks WHERE job_id = $1 AND ordinal = $2",
            [job_id.into_uuid().into(), ordinal.into()],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("job task not found".to_owned()))?
        .try_get("", "id")
}

async fn start_stage(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    stage: &StageRef,
) -> Result<(), sea_orm::DbErr> {
    let task_id = ensure_task(transaction, job_id, stage.task_ordinal()).await?;
    let ordinal = i32::try_from(stage.ordinal())
        .map_err(|_| sea_orm::DbErr::Type("stage ordinal is out of range".to_owned()))?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO job_stages \
             (id, task_id, name, ordinal, state, attempt_count, started_at) \
             VALUES ($1, $2, $3, $4, 'running', 1, now()) \
             ON CONFLICT (task_id, name) DO UPDATE SET state = 'running', \
             attempt_count = job_stages.attempt_count + 1, \
             started_at = COALESCE(job_stages.started_at, now()), updated_at = now() \
             WHERE job_stages.state = 'pending' AND job_stages.attempt_count < 3 \
             AND job_stages.ordinal = EXCLUDED.ordinal RETURNING attempt_count",
            [
                Uuid::new_v4().into(),
                task_id.into(),
                stage.name().into(),
                ordinal.into(),
            ],
        ))
        .await?;
    if row.is_none() {
        return Err(sea_orm::DbErr::Custom(
            "stage cannot be started or retried".to_owned(),
        ));
    }
    Ok(())
}

async fn update_stage_checkpoint(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    stage: &StageRef,
    checkpoint: &Checkpoint,
    complete: bool,
) -> Result<(), sea_orm::DbErr> {
    let task_id = find_task(transaction, job_id, stage.task_ordinal()).await?;
    let ordinal = i32::try_from(stage.ordinal())
        .map_err(|_| sea_orm::DbErr::Type("stage ordinal is out of range".to_owned()))?;
    let state = if complete { "completed" } else { "running" };
    let completed_at = if complete { "now()" } else { "completed_at" };
    let sql = format!(
        "UPDATE job_stages SET checkpoint = checkpoint || $4, state = '{state}', \
         completed_at = {completed_at}, updated_at = now() WHERE task_id = $1 \
         AND name = $2 AND ordinal = $3 AND state = 'running'"
    );
    let updated = transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [
                task_id.into(),
                stage.name().into(),
                ordinal.into(),
                checkpoint_json(checkpoint).into(),
            ],
        ))
        .await?;
    if updated.rows_affected() != 1 {
        return Err(sea_orm::DbErr::RecordNotUpdated);
    }
    Ok(())
}

async fn fail_stage(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    stage: &StageRef,
    retryable: bool,
    error_code: &str,
) -> Result<bool, sea_orm::DbErr> {
    let task_id = find_task(transaction, job_id, stage.task_ordinal()).await?;
    let ordinal = i32::try_from(stage.ordinal())
        .map_err(|_| sea_orm::DbErr::Type("stage ordinal is out of range".to_owned()))?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT attempt_count FROM job_stages WHERE task_id = $1 AND name = $2 \
             AND ordinal = $3 AND state = 'running' FOR UPDATE",
            [task_id.into(), stage.name().into(), ordinal.into()],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("running stage not found".to_owned()))?;
    let attempt = u32::try_from(row.try_get::<i32>("", "attempt_count")?)
        .map_err(|_| sea_orm::DbErr::Type("invalid stage attempt count".to_owned()))?;
    let terminal =
        StageFailureOutcome::for_attempt(attempt, retryable) == StageFailureOutcome::Failed;
    let state = if terminal { "failed" } else { "pending" };
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job_stages SET state = $4, error_snapshot = $5, updated_at = now(), \
             completed_at = CASE WHEN $4 = 'failed' THEN now() ELSE NULL END \
             WHERE task_id = $1 AND name = $2 AND ordinal = $3 AND state = 'running'",
            [
                task_id.into(),
                stage.name().into(),
                ordinal.into(),
                state.into(),
                serde_json::json!({"code": error_code, "retryable": retryable}).into(),
            ],
        ))
        .await?;
    if terminal {
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE job_tasks SET state = 'failed', completed_at = now(), updated_at = now(), \
                 error_snapshot = $2 WHERE id = $1",
                [
                    task_id.into(),
                    serde_json::json!({"code": error_code, "retryable": retryable}).into(),
                ],
            ))
            .await?;
    }
    Ok(terminal)
}

async fn finish_tasks(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    state: JobState,
) -> Result<(), sea_orm::DbErr> {
    let target = match state {
        JobState::Completed => "completed",
        JobState::Cancelled => "cancelled",
        JobState::Failed => "failed",
        JobState::Partial | JobState::NeedsAction => return Ok(()),
        _ => return Ok(()),
    };
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job_tasks SET state = $2, completed_at = now(), updated_at = now() \
             WHERE job_id = $1 AND state IN ('pending', 'running')",
            [job_id.into_uuid().into(), target.into()],
        ))
        .await?;
    Ok(())
}

async fn release_lease(
    transaction: &sea_orm::DatabaseTransaction,
    lease: LeaseId,
) -> Result<(), sea_orm::DbErr> {
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM job_leases WHERE id = $1",
            [lease.into_uuid().into()],
        ))
        .await?;
    Ok(())
}

fn checkpoint_json(checkpoint: &Checkpoint) -> serde_json::Value {
    serde_json::Value::Object(
        checkpoint
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    CheckpointValue::String(value) => serde_json::Value::String(value.clone()),
                    CheckpointValue::Unsigned(value) => (*value).into(),
                    CheckpointValue::Bool(value) => (*value).into(),
                };
                (key.clone(), value)
            })
            .collect(),
    )
}

fn event_payload(event: &JobEvent) -> (&'static str, serde_json::Value) {
    match event.kind() {
        JobEventKind::Started => ("job.started", serde_json::json!({})),
        JobEventKind::StageStarted(stage) => ("stage.started", stage_payload(stage)),
        JobEventKind::StageCheckpoint { stage, checkpoint } => (
            "stage.checkpointed",
            with_checkpoint(stage_payload(stage), checkpoint),
        ),
        JobEventKind::StageCompleted { stage, checkpoint } => (
            "stage.completed",
            with_checkpoint(stage_payload(stage), checkpoint),
        ),
        JobEventKind::StageFailed {
            stage,
            retryable,
            error_code,
        } => {
            let mut payload = stage_payload(stage);
            let object = payload.as_object_mut().expect("stage payload is an object");
            object.insert("retryable".to_owned(), (*retryable).into());
            object.insert(
                "error_code".to_owned(),
                serde_json::Value::String(error_code.clone()),
            );
            ("stage.failed", payload)
        }
        JobEventKind::JobTransition {
            state,
            needs_action_reason,
        } => (
            "job.transitioned",
            serde_json::json!({
                "state": job_state_value(*state),
                "needs_action_reason": needs_action_reason.map(needs_action_reason_value),
            }),
        ),
    }
}

fn stage_payload(stage: &StageRef) -> serde_json::Value {
    serde_json::json!({
        "task_ordinal": stage.task_ordinal(),
        "stage_name": stage.name(),
        "stage_ordinal": stage.ordinal(),
    })
}

fn with_checkpoint(mut payload: serde_json::Value, checkpoint: &Checkpoint) -> serde_json::Value {
    payload
        .as_object_mut()
        .expect("stage payload is an object")
        .insert("checkpoint".to_owned(), checkpoint_json(checkpoint));
    payload
}

async fn lease_next_in_transaction(
    transaction: &sea_orm::DatabaseTransaction,
    runner: ClientId,
    ttl_seconds: i64,
) -> Result<Option<JobLease>, sea_orm::DbErr> {
    transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [LEASE_ADVISORY_LOCK.into()],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("advisory lock query failed".to_owned()))?;

    if let Some(existing) = transaction
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT job_id, expires_at > now() AS active \
             FROM job_leases WHERE slot = 1 FOR UPDATE",
        ))
        .await?
    {
        if existing.try_get::<bool>("", "active")? {
            return Ok(None);
        }
        let expired_job = existing.try_get::<Uuid>("", "job_id")?;
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE job_stages SET state = 'pending', updated_at = now() \
                 WHERE state = 'running' AND task_id IN \
                 (SELECT id FROM job_tasks WHERE job_id = $1)",
                [expired_job.into()],
            ))
            .await?;
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE job_tasks SET state = 'pending', updated_at = now() \
                 WHERE job_id = $1 AND state = 'running'",
                [expired_job.into()],
            ))
            .await?;
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE jobs SET state = CASE \
                 WHEN state = 'cancel_requested' THEN 'cancelled' ELSE 'queued' END, \
                 completed_at = CASE WHEN state = 'cancel_requested' THEN now() \
                 ELSE completed_at END, updated_at = now() WHERE id = $1 \
                 AND state IN ('leased', 'running', 'blocked_storage', 'publishing', \
                 'plex_pending', 'cancel_requested')",
                [expired_job.into()],
            ))
            .await?;
        transaction
            .execute_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "DELETE FROM job_leases WHERE slot = 1",
            ))
            .await?;
    }

    let Some(candidate) = transaction
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "UPDATE jobs SET state = 'leased', attempt_count = attempt_count + 1, \
             updated_at = now() WHERE id = (SELECT id FROM jobs WHERE state = 'queued' \
             ORDER BY created_at, id FOR UPDATE SKIP LOCKED LIMIT 1) AND state = 'queued' \
             RETURNING id",
        ))
        .await?
    else {
        let queued = transaction
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM jobs WHERE state = 'queued') AS queued",
            ))
            .await?
            .ok_or_else(|| sea_orm::DbErr::Custom("queued job check failed".to_owned()))?;
        if queued.try_get::<bool>("", "queued")? {
            return Err(sea_orm::DbErr::RecordNotUpdated);
        }
        return Ok(None);
    };
    let job_id = candidate.try_get::<Uuid>("", "id")?;
    let lease_id = Uuid::new_v4();
    let inserted = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO job_leases \
             (id, slot, job_id, runner_client_id, expires_at) \
             VALUES ($1, 1, $2, $3, now() + ($4::double precision * interval '1 second')) \
             ON CONFLICT DO NOTHING RETURNING expires_at",
            [
                lease_id.into(),
                job_id.into(),
                runner.into_uuid().into(),
                ttl_seconds.into(),
            ],
        ))
        .await?;
    let Some(inserted) = inserted else {
        let active = transaction
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM job_leases \
                 WHERE slot = 1 AND expires_at > now()) AS active",
            ))
            .await?
            .ok_or_else(|| sea_orm::DbErr::Custom("lease race check failed".to_owned()))?;
        if active.try_get::<bool>("", "active")? {
            return Ok(None);
        }
        return Err(sea_orm::DbErr::Custom(
            "lease slot conflicted without an active lease".to_owned(),
        ));
    };
    let expires_at = inserted.try_get("", "expires_at")?;
    let job = load_job(transaction, job_id).await?;
    debug_assert_eq!(job.state(), JobState::Leased);
    Ok(Some(JobLease::new(
        LeaseId::from_uuid(lease_id),
        job,
        runner,
        expires_at,
    )))
}

async fn heartbeat_in_transaction(
    transaction: &sea_orm::DatabaseTransaction,
    lease: LeaseId,
    runner: ClientId,
    ttl_seconds: i64,
) -> Result<Option<JobLease>, sea_orm::DbErr> {
    let Some(updated) = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job_leases SET \
             expires_at = now() + ($3::double precision * interval '1 second'), \
             updated_at = now() WHERE id = $1 AND runner_client_id = $2 \
             AND expires_at > now() RETURNING job_id, expires_at",
            [
                lease.into_uuid().into(),
                runner.into_uuid().into(),
                ttl_seconds.into(),
            ],
        ))
        .await?
    else {
        return Ok(None);
    };
    let job = load_job(transaction, updated.try_get("", "job_id")?).await?;
    Ok(Some(JobLease::new(
        lease,
        job,
        runner,
        updated.try_get("", "expires_at")?,
    )))
}

fn valid_ttl_seconds(ttl: time::Duration) -> Result<i64, PortError> {
    if ttl < time::Duration::seconds(30) || ttl > time::Duration::seconds(300) {
        Err(PortError::Conflict)
    } else {
        Ok(ttl.whole_seconds())
    }
}

async fn load_job(
    transaction: &sea_orm::DatabaseTransaction,
    id: Uuid,
) -> Result<Job, sea_orm::DbErr> {
    job::Entity::find_by_id(id)
        .one(transaction)
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("leased job disappeared".to_owned()))?
        .try_into()
        .map_err(|error| sea_orm::DbErr::Type(format!("invalid persisted job: {error:?}")))
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
