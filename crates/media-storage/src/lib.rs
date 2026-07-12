//! PostgreSQL persistence adapters and explicit migrations.

mod entity;
mod mapping;
mod migration;
mod repository;

pub use migration::Migrator;
pub use repository::{
    MaintenanceReport, ReservationGeneration, ReservationHandle, ReservationHandleError,
    ReservationRecord, SeaOrmClientStore, SeaOrmIdempotencyRepository, SeaOrmIdentityStore,
    SeaOrmJobStore, SeaOrmLeaseStore, SeaOrmMaintenanceStore, SeaOrmNotificationOutbox,
    SeaOrmOperationReceiptRepository, SeaOrmReadiness, SeaOrmSearchRepository, SeaOrmTrackingStore,
    SearchSessionRecord, StoredResponseError, StoredResponseRecord,
};
