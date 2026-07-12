use media_core::{MetricsSnapshot, MetricsSource, PortError};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

use crate::{
    mapping::parse_job_state,
    repository::{map_database_error, map_mapping_error},
};

/// Gathers aggregate monitoring counts with plain aggregate queries.
#[derive(Clone)]
pub struct SeaOrmMetricsSource {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmMetricsSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmMetricsSource { database: [REDACTED] }")
    }
}

impl SeaOrmMetricsSource {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl MetricsSource for SeaOrmMetricsSource {
    async fn snapshot(&self) -> Result<MetricsSnapshot, PortError> {
        let rows = self
            .database
            .query_all_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT state, COUNT(*)::bigint AS count FROM jobs GROUP BY state",
            ))
            .await
            .map_err(map_database_error)?;
        let mut jobs_by_state = Vec::with_capacity(rows.len());
        for row in rows {
            let raw = row
                .try_get::<String>("", "state")
                .map_err(map_database_error)?;
            let count = row
                .try_get::<i64>("", "count")
                .ok()
                .and_then(|value| u64::try_from(value).ok())
                .ok_or(PortError::Infrastructure)?;
            let state = parse_job_state(&raw).map_err(map_mapping_error)?;
            jobs_by_state.push((state, count));
        }

        let outbox = self
            .database
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT \
                 COUNT(*) FILTER (WHERE delivered_at IS NULL AND dead_at IS NULL)::bigint \
                   AS pending, \
                 COUNT(*) FILTER (WHERE dead_at IS NOT NULL)::bigint AS dead \
                 FROM notification_outbox",
            ))
            .await
            .map_err(map_database_error)?
            .ok_or(PortError::Infrastructure)?;
        let notifications_pending = read_count(&outbox, "pending")?;
        let notifications_dead = read_count(&outbox, "dead")?;

        Ok(MetricsSnapshot {
            jobs_by_state,
            notifications_pending,
            notifications_dead,
        })
    }
}

fn read_count(row: &sea_orm::QueryResult, column: &str) -> Result<u64, PortError> {
    row.try_get::<i64>("", column)
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(PortError::Infrastructure)
}
