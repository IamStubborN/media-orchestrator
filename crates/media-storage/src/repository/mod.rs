mod client;
mod idempotency;
mod identity;
mod job;
mod lease;
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
pub use operation::SeaOrmOperationReceiptRepository;
pub use readiness::SeaOrmReadiness;
pub use search::{SeaOrmSearchRepository, SearchSessionRecord};
pub use tracking::{SeaOrmNotificationOutbox, SeaOrmTrackingStore};

use media_core::PortError;
use sea_orm::{DbErr, SqlErr};

fn map_database_error(error: DbErr) -> PortError {
    if matches!(error.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) {
        PortError::Conflict
    } else {
        PortError::Infrastructure
    }
}

fn map_mapping_error(_: crate::mapping::MappingError) -> PortError {
    PortError::Infrastructure
}
