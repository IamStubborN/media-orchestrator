mod client;
mod identity;
mod readiness;

pub use client::SeaOrmClientStore;
pub use identity::SeaOrmIdentityStore;
pub use readiness::SeaOrmReadiness;

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
