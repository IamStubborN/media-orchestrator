use media_core::{Job, JobId, JobLease, LeaseId, OperationKey, PortError, UserId};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction,
    EntityTrait, QueryFilter, Statement,
};

use crate::{
    entity::operation_receipt,
    mapping::{
        job_state_value, needs_action_reason_value, notify_scope_value, parse_job_state,
        parse_needs_action_reason, parse_notify_scope, parse_provider, provider_value,
    },
    repository::map_database_error,
};

#[derive(Clone)]
pub struct SeaOrmOperationReceiptRepository {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmOperationReceiptRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmOperationReceiptRepository { database: [REDACTED] }")
    }
}

impl SeaOrmOperationReceiptRepository {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }

    pub async fn is_completed(&self, key: OperationKey) -> Result<bool, PortError> {
        self.database
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM operation_receipts \
                 WHERE operation_key = $1 AND result_kind <> 'pending') AS completed",
                [key.as_bytes().to_vec().into()],
            ))
            .await
            .map_err(map_database_error)?
            .ok_or(PortError::Infrastructure)?
            .try_get("", "completed")
            .map_err(map_database_error)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum OperationKind {
    CreateJob,
    CancelJob,
    LeaseNext,
    Heartbeat,
    ReportEvent,
}

impl OperationKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CreateJob => "create_job",
            Self::CancelJob => "cancel_job",
            Self::LeaseNext => "lease_next",
            Self::Heartbeat => "heartbeat",
            Self::ReportEvent => "report_event",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum OperationResult {
    Job(Job),
    Lease(JobLease),
    None,
}

pub(crate) enum OperationClaim {
    Fresh,
    Replay(OperationResult),
}

pub(crate) async fn claim(
    transaction: &DatabaseTransaction,
    key: OperationKey,
    kind: OperationKind,
) -> Result<OperationClaim, sea_orm::DbErr> {
    let inserted = transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO operation_receipts \
             (id, operation_key, operation_kind, result_kind) \
             VALUES ($1, $2, $3, 'pending') \
             ON CONFLICT (operation_key) DO NOTHING",
            [
                uuid::Uuid::new_v4().into(),
                key.as_bytes().to_vec().into(),
                kind.as_str().into(),
            ],
        ))
        .await?;
    if inserted.rows_affected() == 1 {
        return Ok(OperationClaim::Fresh);
    }

    let receipt = operation_receipt::Entity::find()
        .filter(operation_receipt::Column::OperationKey.eq(key.as_bytes().to_vec()))
        .one(transaction)
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("operation receipt disappeared".to_owned()))?;
    if receipt.operation_kind != kind.as_str() {
        return Err(sea_orm::DbErr::Type(
            "operation key was reused across operation kinds".to_owned(),
        ));
    }

    let result = decode_result(&receipt.result_kind, receipt.result_snapshot)?;
    if !result_matches_kind(kind, &result) {
        return Err(sea_orm::DbErr::Type(
            "operation receipt has an invalid result kind".to_owned(),
        ));
    }
    Ok(OperationClaim::Replay(result))
}

pub(crate) async fn complete(
    transaction: &DatabaseTransaction,
    key: OperationKey,
    kind: OperationKind,
    result: &OperationResult,
) -> Result<(), sea_orm::DbErr> {
    if !result_matches_kind(kind, result) {
        return Err(sea_orm::DbErr::Type(
            "operation result does not match its kind".to_owned(),
        ));
    }
    let (result_kind, result_snapshot) = encode_result(result)?;
    let updated = transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE operation_receipts SET result_kind = $3, result_snapshot = $4, \
             updated_at = now() WHERE operation_key = $1 AND operation_kind = $2 \
             AND result_kind = 'pending'",
            [
                key.as_bytes().to_vec().into(),
                kind.as_str().into(),
                result_kind.into(),
                result_snapshot.into(),
            ],
        ))
        .await?;
    if updated.rows_affected() != 1 {
        return Err(sea_orm::DbErr::RecordNotUpdated);
    }
    Ok(())
}

const fn result_matches_kind(kind: OperationKind, result: &OperationResult) -> bool {
    matches!(
        (kind, result),
        (OperationKind::CreateJob, OperationResult::Job(_))
            | (
                OperationKind::CancelJob | OperationKind::ReportEvent,
                OperationResult::Job(_) | OperationResult::None
            )
            | (
                OperationKind::LeaseNext | OperationKind::Heartbeat,
                OperationResult::Lease(_) | OperationResult::None
            )
    )
}

fn encode_result(
    result: &OperationResult,
) -> Result<(&'static str, Option<serde_json::Value>), sea_orm::DbErr> {
    match result {
        OperationResult::Job(job) => Ok(("job", Some(job_snapshot(job)))),
        OperationResult::Lease(lease) => Ok(("lease", Some(lease_snapshot(lease)?))),
        OperationResult::None => Ok(("none", None)),
    }
}

fn decode_result(
    result_kind: &str,
    snapshot: Option<serde_json::Value>,
) -> Result<OperationResult, sea_orm::DbErr> {
    match (result_kind, snapshot) {
        ("job", Some(snapshot)) => job_from_snapshot(&snapshot).map(OperationResult::Job),
        ("lease", Some(snapshot)) => lease_from_snapshot(&snapshot).map(OperationResult::Lease),
        ("none", None) => Ok(OperationResult::None),
        ("pending", None) => Err(sea_orm::DbErr::Type(
            "operation receipt is incomplete".to_owned(),
        )),
        _ => Err(sea_orm::DbErr::Type(
            "operation receipt snapshot is invalid".to_owned(),
        )),
    }
}

fn job_snapshot(job: &Job) -> serde_json::Value {
    serde_json::json!({
        "id": job.id().to_string(),
        "owner_id": job.owner_id().to_string(),
        "provider": provider_value(job.provider()),
        "result_ref": job.result_ref(),
        "state": job_state_value(job.state()),
        "needs_action_reason": job.needs_action_reason().map(needs_action_reason_value),
        "notify_scope": notify_scope_value(job.notify_scope()),
    })
}

fn lease_snapshot(lease: &JobLease) -> Result<serde_json::Value, sea_orm::DbErr> {
    let expires_at_unix_nanos = i64::try_from(lease.expires_at().unix_timestamp_nanos())
        .map_err(|_| sea_orm::DbErr::Type("lease timestamp is out of range".to_owned()))?;
    Ok(serde_json::json!({
        "lease_id": lease.lease_id().to_string(),
        "runner_client_id": lease.runner_client_id().to_string(),
        "expires_at_unix_nanos": expires_at_unix_nanos,
        "job": job_snapshot(lease.job()),
    }))
}

fn job_from_snapshot(snapshot: &serde_json::Value) -> Result<Job, sea_orm::DbErr> {
    let object = object(snapshot)?;
    let needs_action_reason = match object.get("needs_action_reason") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => {
            Some(parse_needs_action_reason(value).map_err(|_| invalid_snapshot())?)
        }
        _ => return Err(invalid_snapshot()),
    };
    Job::rehydrate(
        string(object, "id")?
            .parse::<JobId>()
            .map_err(|_| invalid_snapshot())?,
        string(object, "owner_id")?
            .parse::<UserId>()
            .map_err(|_| invalid_snapshot())?,
        parse_provider(string(object, "provider")?).map_err(|_| invalid_snapshot())?,
        string(object, "result_ref")?.to_owned(),
        parse_job_state(string(object, "state")?).map_err(|_| invalid_snapshot())?,
        needs_action_reason,
        parse_notify_scope(string(object, "notify_scope")?).map_err(|_| invalid_snapshot())?,
    )
    .map_err(|_| invalid_snapshot())
}

fn lease_from_snapshot(snapshot: &serde_json::Value) -> Result<JobLease, sea_orm::DbErr> {
    let object = object(snapshot)?;
    let expires_at = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(integer(
        object,
        "expires_at_unix_nanos",
    )?))
    .map_err(|_| invalid_snapshot())?;
    Ok(JobLease::new(
        string(object, "lease_id")?
            .parse::<LeaseId>()
            .map_err(|_| invalid_snapshot())?,
        job_from_snapshot(object.get("job").ok_or_else(invalid_snapshot)?)?,
        string(object, "runner_client_id")?
            .parse()
            .map_err(|_| invalid_snapshot())?,
        expires_at,
    ))
}

fn object(
    value: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, sea_orm::DbErr> {
    value.as_object().ok_or_else(invalid_snapshot)
}

fn string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, sea_orm::DbErr> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(invalid_snapshot)
}

fn integer(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<i64, sea_orm::DbErr> {
    object
        .get(field)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(invalid_snapshot)
}

fn invalid_snapshot() -> sea_orm::DbErr {
    sea_orm::DbErr::Type("operation receipt snapshot is invalid".to_owned())
}

#[cfg(test)]
mod tests {
    use media_core::{
        PRIMARY_USER_ID, Job, JobId, JobLease, JobState, LeaseId, NotifyScope, Provider,
        RUNNER_CLIENT_ID,
    };

    use super::{job_from_snapshot, job_snapshot, lease_from_snapshot, lease_snapshot};

    fn job() -> Job {
        Job::rehydrate(
            JobId::new(),
            PRIMARY_USER_ID,
            Provider::Rezka,
            "receipt-test".to_owned(),
            JobState::Leased,
            None,
            NotifyScope::Initiator,
        )
        .unwrap()
    }

    #[test]
    fn corrupted_snapshots_fail_closed_when_required_fields_are_missing() {
        let job = job();
        let mut job_snapshot = job_snapshot(&job);
        job_snapshot
            .as_object_mut()
            .unwrap()
            .remove("needs_action_reason");
        assert!(job_from_snapshot(&job_snapshot).is_err());

        let lease = JobLease::new(
            LeaseId::new(),
            job,
            RUNNER_CLIENT_ID,
            time::OffsetDateTime::UNIX_EPOCH,
        );
        let mut lease_snapshot = lease_snapshot(&lease).unwrap();
        lease_snapshot
            .as_object_mut()
            .unwrap()
            .remove("expires_at_unix_nanos");
        assert!(lease_from_snapshot(&lease_snapshot).is_err());
    }
}
