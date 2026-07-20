use media_core::{
    PortError, RunnerLifecycle, RunnerLifecycleState, RunnerLifecycleStore, RunnerLifecycleUpdate,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

use crate::repository::map_database_error;

#[derive(Clone)]
pub struct SeaOrmRunnerLifecycleStore {
    database: DatabaseConnection,
}

impl SeaOrmRunnerLifecycleStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl RunnerLifecycleStore for SeaOrmRunnerLifecycleStore {
    async fn get(&self) -> Result<RunnerLifecycle, PortError> {
        let row = self.database.query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT state, reason, previous_ip, current_ip, updated_at FROM runner_lifecycle WHERE singleton = true".to_owned(),
        )).await.map_err(map_database_error)?.ok_or(PortError::Infrastructure)?;
        map_row(&row)
    }

    async fn update(&self, update: RunnerLifecycleUpdate) -> Result<RunnerLifecycle, PortError> {
        let row = self.database.query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE runner_lifecycle SET state = $1, reason = $2, previous_ip = $3, current_ip = $4, \
             sticky_job_id = CASE WHEN state = 'ready' AND $1 = 'ready' \
             AND current_ip IS NOT DISTINCT FROM $4 THEN sticky_job_id ELSE NULL END, \
             sticky_attempt_count = CASE WHEN state = 'ready' AND $1 = 'ready' \
             AND current_ip IS NOT DISTINCT FROM $4 THEN sticky_attempt_count ELSE 0 END, \
             updated_at = now() WHERE singleton = true \
             RETURNING state, reason, previous_ip, current_ip, updated_at",
            [update.state.as_wire().into(), update.reason.into(), update.previous_ip.into(), update.current_ip.into()],
        )).await.map_err(map_database_error)?.ok_or(PortError::Infrastructure)?;
        map_row(&row)
    }
}

fn map_row(row: &sea_orm::QueryResult) -> Result<RunnerLifecycle, PortError> {
    let state = match row
        .try_get::<String>("", "state")
        .map_err(map_database_error)?
        .as_str()
    {
        "ready" => RunnerLifecycleState::Ready,
        "rotating" => RunnerLifecycleState::Rotating,
        "blocked" => RunnerLifecycleState::Blocked,
        _ => return Err(PortError::Infrastructure),
    };
    Ok(RunnerLifecycle {
        state,
        reason: row.try_get("", "reason").map_err(map_database_error)?,
        previous_ip: row.try_get("", "previous_ip").map_err(map_database_error)?,
        current_ip: row.try_get("", "current_ip").map_err(map_database_error)?,
        updated_at: row.try_get("", "updated_at").map_err(map_database_error)?,
    })
}
