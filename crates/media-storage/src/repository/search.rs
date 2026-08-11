use media_core::{PortError, UserId};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait};

use super::map_database_error;

#[derive(Clone, PartialEq)]
pub struct SearchSessionRecord {
    pub id: uuid::Uuid,
    pub owner: UserId,
    pub payload: serde_json::Value,
    pub expires_at: time::OffsetDateTime,
}

impl std::fmt::Debug for SearchSessionRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SearchSessionRecord")
            .field("id", &self.id)
            .field("owner", &self.owner)
            .field("payload", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct SeaOrmSearchRepository {
    database: DatabaseConnection,
}

impl SeaOrmSearchRepository {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }

    pub async fn insert_session(&self, record: SearchSessionRecord) -> Result<(), PortError> {
        self.database.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO search_sessions (id, owner_id, payload, expires_at) VALUES ($1, $2, $3, $4)",
            [record.id.into(), (*record.owner.as_uuid()).into(), record.payload.into(), record.expires_at.into()],
        )).await.map_err(map_database_error).map(|_| ())
    }

    pub async fn update_session(&self, record: SearchSessionRecord) -> Result<(), PortError> {
        let result = self.database.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE search_sessions SET payload = $3, expires_at = $4, updated_at = now() WHERE id = $1 AND owner_id = $2",
            [record.id.into(), (*record.owner.as_uuid()).into(), record.payload.into(), record.expires_at.into()],
        )).await.map_err(map_database_error)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    pub async fn update_session_for_user(
        &self,
        record: SearchSessionRecord,
        user: UserId,
    ) -> Result<(), PortError> {
        let result = self
            .database
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE search_sessions AS sessions
                    SET payload = $3, expires_at = $4, updated_at = now()
                   FROM tracking_subscriptions AS tracking
                  WHERE sessions.id = $1
                    AND (sessions.owner_id = $2
                         OR (tracking.id::text = substr(sessions.payload #>> '{request,scope,chat_id}', 10)
                             AND tracking.deleted_at IS NULL
                             AND tracking.scope = 'family'
                             AND $2 IN ($5, $6)))",
                [
                    record.id.into(),
                    (*user.as_uuid()).into(),
                    record.payload.into(),
                    record.expires_at.into(),
                    (*media_core::PRIMARY_USER_ID.as_uuid()).into(),
                    (*media_core::SECONDARY_USER_ID.as_uuid()).into(),
                ],
            ))
            .await
            .map_err(map_database_error)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    pub async fn session_for_owner(
        &self,
        id: uuid::Uuid,
        owner: UserId,
    ) -> Result<Option<SearchSessionRecord>, PortError> {
        let row = self.database.query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, owner_id, payload, expires_at FROM search_sessions WHERE id = $1 AND owner_id = $2",
            [id.into(), (*owner.as_uuid()).into()],
        )).await.map_err(map_database_error)?;
        row.map(|row| {
            let owner_id: uuid::Uuid = row.try_get("", "owner_id").map_err(map_database_error)?;
            Ok(SearchSessionRecord {
                id: row.try_get("", "id").map_err(map_database_error)?,
                owner: UserId::from_uuid(owner_id),
                payload: row.try_get("", "payload").map_err(map_database_error)?,
                expires_at: row.try_get("", "expires_at").map_err(map_database_error)?,
            })
        })
        .transpose()
    }

    /// Resolve a tracked-episode session for either its owner or a fixed
    /// family participant. Generic search sessions remain owner-only.
    pub async fn session_for_user(
        &self,
        id: uuid::Uuid,
        user: UserId,
    ) -> Result<Option<SearchSessionRecord>, PortError> {
        let row = self
            .database
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT sessions.id, sessions.owner_id, sessions.payload, sessions.expires_at
                   FROM search_sessions AS sessions
                   LEFT JOIN tracking_subscriptions AS tracking
                     ON tracking.id::text = substr(sessions.payload #>> '{request,scope,chat_id}', 10)
                    AND tracking.deleted_at IS NULL
                  WHERE sessions.id = $1
                    AND (sessions.owner_id = $2
                         OR (tracking.scope = 'family' AND $2 IN ($3, $4)))",
                [
                    id.into(),
                    (*user.as_uuid()).into(),
                    (*media_core::PRIMARY_USER_ID.as_uuid()).into(),
                    (*media_core::SECONDARY_USER_ID.as_uuid()).into(),
                ],
            ))
            .await
            .map_err(map_database_error)?;
        row.map(|row| {
            let owner_id: uuid::Uuid = row.try_get("", "owner_id").map_err(map_database_error)?;
            Ok(SearchSessionRecord {
                id: row.try_get("", "id").map_err(map_database_error)?,
                owner: UserId::from_uuid(owner_id),
                payload: row.try_get("", "payload").map_err(map_database_error)?,
                expires_at: row.try_get("", "expires_at").map_err(map_database_error)?,
            })
        })
        .transpose()
    }

    pub async fn insert_execution(
        &self,
        result_ref: &str,
        payload: serde_json::Value,
    ) -> Result<(), PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            transaction
                .execute_unprepared("LOCK TABLE jobs IN SHARE MODE")
                .await?;
            transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT result_ref FROM search_executions WHERE result_ref = $1 FOR UPDATE",
                    [result_ref.into()],
                ))
                .await?;
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO search_executions (result_ref, payload) VALUES ($1, $2)
             ON CONFLICT (result_ref) DO UPDATE SET payload = EXCLUDED.payload
             WHERE NOT EXISTS (
                 SELECT 1 FROM jobs WHERE jobs.result_ref = search_executions.result_ref
             )",
                    [result_ref.into(), payload.into()],
                ))
                .await
                .map(|_| ())
        }
        .await;
        match result {
            Ok(()) => transaction.commit().await.map_err(map_database_error),
            Err(error) => {
                transaction.rollback().await.map_err(map_database_error)?;
                Err(map_database_error(error))
            }
        }
    }

    pub async fn execution_for(
        &self,
        result_ref: &str,
    ) -> Result<Option<serde_json::Value>, PortError> {
        self.database
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT payload FROM search_executions WHERE result_ref = $1",
                [result_ref.into()],
            ))
            .await
            .map_err(map_database_error)?
            .map(|row| row.try_get("", "payload").map_err(map_database_error))
            .transpose()
    }

    pub async fn update_execution(
        &self,
        result_ref: &str,
        payload: serde_json::Value,
    ) -> Result<(), PortError> {
        let result = self
            .database
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE search_executions SET payload = $2 WHERE result_ref = $1",
                [result_ref.into(), payload.into()],
            ))
            .await
            .map_err(map_database_error)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }
}

impl std::fmt::Debug for SeaOrmSearchRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmSearchRepository { database: [REDACTED] }")
    }
}
