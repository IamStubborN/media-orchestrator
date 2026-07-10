use media_core::{ClientId, PortError};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait,
    prelude::Uuid,
};

use crate::repository::map_database_error;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReservationRecord {
    Reserved,
    Replay(StoredResponseRecord),
    Conflict,
    InProgress,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StoredResponseRecord {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StoredResponseError {
    InvalidStatus,
    EmptyContentType,
}

impl std::fmt::Display for StoredResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidStatus => "response status must be between 100 and 599",
            Self::EmptyContentType => "response content type cannot be blank",
        })
    }
}

impl std::error::Error for StoredResponseError {}

impl StoredResponseRecord {
    pub fn new(
        status: u16,
        content_type: String,
        body: Vec<u8>,
    ) -> Result<Self, StoredResponseError> {
        if !(100..=599).contains(&status) {
            return Err(StoredResponseError::InvalidStatus);
        }
        if content_type.trim().is_empty() {
            return Err(StoredResponseError::EmptyContentType);
        }
        Ok(Self {
            status,
            content_type,
            body,
        })
    }

    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Clone)]
pub struct SeaOrmIdempotencyRepository {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmIdempotencyRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmIdempotencyRepository { database: [REDACTED] }")
    }
}

impl SeaOrmIdempotencyRepository {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }

    pub async fn reserve(
        &self,
        client: ClientId,
        key: &str,
        request_hash: [u8; 32],
        expires_at: time::OffsetDateTime,
    ) -> Result<ReservationRecord, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            let inserted = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO idempotency_records \
                     (id, client_id, idempotency_key, request_hash, status, expires_at) \
                     VALUES ($1, $2, $3, $4, 'in_progress', $5) \
                     ON CONFLICT (client_id, idempotency_key) DO NOTHING",
                    [
                        Uuid::new_v4().into(),
                        client.into_uuid().into(),
                        key.to_owned().into(),
                        request_hash.to_vec().into(),
                        expires_at.into(),
                    ],
                ))
                .await?;
            if inserted.rows_affected() == 1 {
                return Ok(ReservationRecord::Reserved);
            }

            let row = transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT request_hash, status, response_status, response_content_type, \
                     response_body, expires_at <= now() AS expired \
                     FROM idempotency_records \
                     WHERE client_id = $1 AND idempotency_key = $2 FOR UPDATE",
                    [client.into_uuid().into(), key.to_owned().into()],
                ))
                .await?
                .ok_or_else(|| sea_orm::DbErr::Custom("reservation disappeared".to_owned()))?;

            if row.try_get::<bool>("", "expired")? {
                transaction
                    .execute_raw(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "UPDATE idempotency_records SET request_hash = $3, status = 'in_progress', \
                         response_status = NULL, response_content_type = NULL, response_body = NULL, \
                         expires_at = $4, created_at = now(), updated_at = now() \
                         WHERE client_id = $1 AND idempotency_key = $2",
                        [
                            client.into_uuid().into(),
                            key.to_owned().into(),
                            request_hash.to_vec().into(),
                            expires_at.into(),
                        ],
                    ))
                    .await?;
                return Ok(ReservationRecord::Reserved);
            }

            if row.try_get::<Vec<u8>>("", "request_hash")? != request_hash {
                return Ok(ReservationRecord::Conflict);
            }
            match row.try_get::<String>("", "status")?.as_str() {
                "in_progress" => Ok(ReservationRecord::InProgress),
                "completed" => {
                    let status = row
                        .try_get::<i16>("", "response_status")?
                        .try_into()
                        .map_err(|_| sea_orm::DbErr::Type("invalid response status".to_owned()))?;
                    let response = StoredResponseRecord::new(
                        status,
                        row.try_get("", "response_content_type")?,
                        row.try_get("", "response_body")?,
                    )
                    .map_err(|_| sea_orm::DbErr::Type("invalid stored response".to_owned()))?;
                    Ok(ReservationRecord::Replay(response))
                }
                _ => Err(sea_orm::DbErr::Type(
                    "invalid idempotency status".to_owned(),
                )),
            }
        }
        .await;
        finish(transaction, result).await
    }

    pub async fn complete(
        &self,
        client: ClientId,
        key: &str,
        request_hash: [u8; 32],
        response: StoredResponseRecord,
    ) -> Result<(), PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            let updated = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE idempotency_records SET status = 'completed', response_status = $4, \
                     response_content_type = $5, response_body = $6, updated_at = now() \
                     WHERE client_id = $1 AND idempotency_key = $2 AND request_hash = $3 \
                     AND status = 'in_progress' AND expires_at > now()",
                    [
                        client.into_uuid().into(),
                        key.to_owned().into(),
                        request_hash.to_vec().into(),
                        i16::try_from(response.status)
                            .map_err(|_| sea_orm::DbErr::Type("invalid status".to_owned()))?
                            .into(),
                        response.content_type.into(),
                        response.body.into(),
                    ],
                ))
                .await?;
            Ok(updated.rows_affected())
        }
        .await;
        finish_semantic_update(transaction, result).await
    }

    pub async fn abort(
        &self,
        client: ClientId,
        key: &str,
        request_hash: [u8; 32],
    ) -> Result<(), PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            let deleted = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM idempotency_records WHERE client_id = $1 \
                     AND idempotency_key = $2 AND request_hash = $3 AND status = 'in_progress'",
                    [
                        client.into_uuid().into(),
                        key.to_owned().into(),
                        request_hash.to_vec().into(),
                    ],
                ))
                .await?;
            Ok(deleted.rows_affected())
        }
        .await;
        finish_semantic_update(transaction, result).await
    }
}

async fn finish_semantic_update(
    transaction: sea_orm::DatabaseTransaction,
    result: Result<u64, sea_orm::DbErr>,
) -> Result<(), PortError> {
    match result {
        Ok(1) => transaction.commit().await.map_err(map_database_error),
        Ok(_) => {
            transaction.rollback().await.map_err(map_database_error)?;
            Err(PortError::Conflict)
        }
        Err(error) => {
            transaction.rollback().await.map_err(map_database_error)?;
            Err(map_database_error(error))
        }
    }
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
