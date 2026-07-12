use media_core::PortError;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait};
use time::{Duration, OffsetDateTime};

use super::map_database_error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaintenanceReport {
    pub search_sessions_deleted: u64,
    pub search_executions_deleted: u64,
    pub jobs_deleted: u64,
    pub notifications_deleted: u64,
    pub outbox_events_deleted: u64,
    pub idempotency_records_deleted: u64,
    pub operation_receipts_deleted: u64,
}

#[derive(Clone)]
pub struct SeaOrmMaintenanceStore {
    database: DatabaseConnection,
}

impl SeaOrmMaintenanceStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }

    pub async fn run(&self, now: OffsetDateTime) -> Result<MaintenanceReport, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            let search_cutoff = now - Duration::hours(24);
            let job_cutoff = now - Duration::days(90);

            let sessions = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM search_sessions \
                     WHERE expires_at <= $1 OR created_at <= $2",
                    [now.into(), search_cutoff.into()],
                ))
                .await?;

            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM job_leases AS lease \
                     USING jobs AS job \
                     WHERE lease.job_id = job.id \
                       AND lease.expires_at <= $1 \
                       AND job.state IN ('completed', 'partial', 'failed', 'cancelled') \
                       AND job.completed_at <= $2",
                    [now.into(), job_cutoff.into()],
                ))
                .await?;

            let notifications = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM notification_outbox AS notification \
                     WHERE (notification.lease_expires_at IS NULL \
                            OR notification.lease_expires_at <= $2) \
                       AND (notification.delivered_at <= $1 \
                        OR notification.dead_at <= $1 \
                        OR (notification.aggregate_type = 'job' AND EXISTS ( \
                            SELECT 1 FROM jobs AS job \
                            WHERE job.id = notification.aggregate_id \
                              AND job.state IN ('completed', 'partial', 'failed', 'cancelled') \
                              AND job.completed_at <= $1 \
                              AND NOT EXISTS ( \
                                  SELECT 1 FROM job_leases AS lease \
                                  WHERE lease.job_id = job.id AND lease.expires_at > $2 \
                              ) \
                        )))",
                    [job_cutoff.into(), now.into()],
                ))
                .await?;

            let outbox_events = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM outbox_events AS event \
                     WHERE event.aggregate_type = 'job' AND EXISTS ( \
                         SELECT 1 FROM jobs AS job \
                         WHERE job.id = event.aggregate_id \
                           AND job.state IN ('completed', 'partial', 'failed', 'cancelled') \
                           AND job.completed_at <= $1 \
                           AND NOT EXISTS ( \
                               SELECT 1 FROM job_leases AS lease \
                               WHERE lease.job_id = job.id AND lease.expires_at > $2 \
                           ) \
                     )",
                    [job_cutoff.into(), now.into()],
                ))
                .await?;

            let jobs = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM jobs AS job \
                     WHERE job.state IN ('completed', 'partial', 'failed', 'cancelled') \
                       AND job.completed_at <= $1 \
                       AND NOT EXISTS ( \
                           SELECT 1 FROM job_leases AS lease \
                           WHERE lease.job_id = job.id AND lease.expires_at > $2 \
                       ) \
                       AND NOT EXISTS ( \
                           SELECT 1 FROM notification_outbox AS notification \
                           WHERE notification.aggregate_type = 'job' \
                             AND notification.aggregate_id = job.id \
                             AND notification.lease_expires_at > $2 \
                       )",
                    [job_cutoff.into(), now.into()],
                ))
                .await?;

            let executions = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM search_executions AS execution \
                     WHERE execution.created_at <= $1 \
                       AND NOT EXISTS ( \
                           SELECT 1 FROM jobs AS job \
                           WHERE job.result_ref = execution.result_ref \
                       )",
                    [search_cutoff.into()],
                ))
                .await?;

            let idempotency_records = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM idempotency_records WHERE expires_at <= $1",
                    [now.into()],
                ))
                .await?;

            // Operation receipts back idempotent replay of runner/API operations,
            // so — like the job, notification, and outbox purges — a receipt is
            // only removed once it is settled and its operation is no longer
            // live: a receipt tied to a job (its snapshot carries the job id, at
            // the top level for a job result or under `job` for a lease result)
            // is kept while that job is still non-terminal, only recently
            // completed, or actively leased. Receipts with no job reference
            // ('none' results) or referencing an already-purged job fall through
            // to age-only pruning. Never purge a still-'pending' receipt.
            let operation_receipts = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM operation_receipts AS receipt \
                     WHERE receipt.created_at <= $1 \
                       AND receipt.result_kind <> 'pending' \
                       AND NOT EXISTS ( \
                           SELECT 1 FROM jobs AS job \
                           WHERE job.id = COALESCE( \
                                   (receipt.result_snapshot->>'id')::uuid, \
                                   (receipt.result_snapshot->'job'->>'id')::uuid) \
                             AND ( \
                                 job.state NOT IN \
                                     ('completed', 'partial', 'failed', 'cancelled') \
                                 OR job.completed_at > $1 \
                                 OR EXISTS ( \
                                     SELECT 1 FROM job_leases AS lease \
                                     WHERE lease.job_id = job.id AND lease.expires_at > $2 \
                                 ) \
                             ) \
                       )",
                    [job_cutoff.into(), now.into()],
                ))
                .await?;

            Ok::<_, sea_orm::DbErr>(MaintenanceReport {
                search_sessions_deleted: sessions.rows_affected(),
                search_executions_deleted: executions.rows_affected(),
                jobs_deleted: jobs.rows_affected(),
                notifications_deleted: notifications.rows_affected(),
                outbox_events_deleted: outbox_events.rows_affected(),
                idempotency_records_deleted: idempotency_records.rows_affected(),
                operation_receipts_deleted: operation_receipts.rows_affected(),
            })
        }
        .await;

        match result {
            Ok(report) => {
                transaction.commit().await.map_err(map_database_error)?;
                Ok(report)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(map_database_error(error))
            }
        }
    }
}

impl std::fmt::Debug for SeaOrmMaintenanceStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmMaintenanceStore { database: [REDACTED] }")
    }
}
