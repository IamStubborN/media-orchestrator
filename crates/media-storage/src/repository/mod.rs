mod client;
mod idempotency;
mod identity;
mod job;
mod lease;
mod lifecycle;
mod maintenance;
mod metrics;
mod operation;
mod readiness;
mod search;
mod tracking;

pub use client::SeaOrmClientStore;
pub use idempotency::{
    ReservationGeneration, ReservationHandle, ReservationHandleError, ReservationRecord,
    SeaOrmIdempotencyRepository, StoredResponseError, StoredResponseRecord,
};
pub use identity::SeaOrmIdentityStore;
pub use job::SeaOrmJobStore;
pub use lease::SeaOrmLeaseStore;
pub use lifecycle::SeaOrmRunnerLifecycleStore;
pub use maintenance::{MaintenanceReport, SeaOrmMaintenanceStore};
pub use metrics::SeaOrmMetricsSource;
pub use operation::SeaOrmOperationReceiptRepository;
pub use readiness::SeaOrmReadiness;
pub use search::{SeaOrmSearchRepository, SearchSessionRecord};
pub use tracking::{SeaOrmNotificationOutbox, SeaOrmTrackingStore};

use media_core::PortError;
use sea_orm::{DbErr, RuntimeErr, SqlErr};

fn notification_delivery_fence_key(
    recipient: &str,
    aggregate_type: &str,
    aggregate_id: uuid::Uuid,
) -> String {
    format!("notification:{recipient}:{aggregate_type}:{aggregate_id}")
}

fn map_database_error(error: DbErr) -> PortError {
    if matches!(error.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) {
        PortError::Conflict
    } else {
        PortError::Infrastructure
    }
}

fn map_tracking_database_error(error: DbErr) -> PortError {
    let diagnostic = safe_database_diagnostic(&error);
    tracing::warn!(
        error_kind = diagnostic.kind,
        sqlstate = diagnostic.sqlstate.as_deref().unwrap_or("unknown"),
        constraint = diagnostic.constraint.as_deref().unwrap_or("unknown"),
        table = diagnostic.table.as_deref().unwrap_or("unknown"),
        "tracking database operation failed"
    );
    map_database_error(error)
}

struct SafeDatabaseDiagnostic {
    kind: &'static str,
    sqlstate: Option<String>,
    constraint: Option<String>,
    table: Option<String>,
}

fn safe_database_diagnostic(error: &DbErr) -> SafeDatabaseDiagnostic {
    let (kind, runtime) = match error {
        DbErr::ConnectionAcquire(_) => ("connection_acquire", None),
        DbErr::Conn(runtime) => ("connection", Some(runtime)),
        DbErr::Exec(runtime) => ("execution", Some(runtime)),
        DbErr::Query(runtime) => ("query", Some(runtime)),
        DbErr::RecordNotFound(_) => ("record_not_found", None),
        DbErr::RecordNotInserted => ("record_not_inserted", None),
        DbErr::RecordNotUpdated => ("record_not_updated", None),
        DbErr::Type(_) | DbErr::TryIntoErr { .. } | DbErr::ConvertFromU64(_) => {
            ("type_conversion", None)
        }
        _ => ("database", None),
    };
    let database_error = runtime.and_then(|runtime| match runtime {
        RuntimeErr::SqlxError(error) => error.as_database_error(),
        _ => None,
    });
    SafeDatabaseDiagnostic {
        kind,
        sqlstate: database_error
            .and_then(|error| error.code())
            .map(|code| code.into_owned()),
        constraint: database_error
            .and_then(|error| error.constraint())
            .map(str::to_owned),
        table: database_error
            .and_then(|error| error.table())
            .map(str::to_owned),
    }
}

fn map_mapping_error(_: crate::mapping::MappingError) -> PortError {
    PortError::Infrastructure
}
