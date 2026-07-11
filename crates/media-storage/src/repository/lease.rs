use media_core::{ClientId, Job, JobLease, JobState, LeaseId, LeaseStore, OperationKey, PortError};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, Statement, TransactionTrait,
    prelude::Uuid,
};

use crate::{
    entity::job,
    repository::{
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
                "UPDATE jobs SET state = CASE \
                 WHEN state = 'cancel_requested' THEN 'cancelled' ELSE 'queued' END, \
                 completed_at = CASE WHEN state = 'cancel_requested' THEN now() \
                 ELSE completed_at END, updated_at = now() WHERE id = $1 \
                 AND state IN ('leased', 'running', 'publishing', 'cancel_requested')",
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
            "SELECT id FROM jobs WHERE state = 'queued' \
             ORDER BY created_at, id FOR UPDATE SKIP LOCKED LIMIT 1",
        ))
        .await?
    else {
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
    let transitioned = transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE jobs SET state = 'leased', attempt_count = attempt_count + 1, \
             updated_at = now() WHERE id = $1 AND state = 'queued'",
            [job_id.into()],
        ))
        .await?;
    if transitioned.rows_affected() != 1 {
        return Err(sea_orm::DbErr::RecordNotUpdated);
    }
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
