use media_core::{
    PRIMARY_USER_ID, EpisodeSnapshot, NewTrackingSubscription, NotificationDelivery,
    NotificationEventType, NotificationId, NotificationOutboxPort, NotificationRecipient,
    OperationKey, PortError, Provider, TrackingId, TrackingScheduleStore, TrackingScope,
    TrackingStore, TrackingSubscription, UserId, SECONDARY_USER_ID,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait};

use crate::repository::map_database_error;

#[derive(Clone)]
pub struct SeaOrmTrackingStore {
    database: DatabaseConnection,
}

impl SeaOrmTrackingStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }

    pub async fn record_future_episode(
        &self,
        id: TrackingId,
        episode: EpisodeSnapshot,
        next_check_at: time::OffsetDateTime,
    ) -> Result<bool, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            let row = transaction.query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT owner_id, title, scope FROM tracking_subscriptions WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
                [id.into_uuid().into()],
            )).await?.ok_or_else(|| sea_orm::DbErr::RecordNotFound("tracking subscription not found".to_owned()))?;
            let discovery_id = uuid::Uuid::new_v4();
            let inserted = transaction.execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO tracking_discoveries (id, tracking_id, season, episode) VALUES ($1, $2, $3, $4) ON CONFLICT (tracking_id, season, episode) DO NOTHING",
                [
                    discovery_id.into(),
                    id.into_uuid().into(),
                    i32::try_from(episode.season()).map_err(|_| sea_orm::DbErr::Type("season is out of range".to_owned()))?.into(),
                    i32::try_from(episode.episode()).map_err(|_| sea_orm::DbErr::Type("episode is out of range".to_owned()))?.into(),
                ],
            )).await?;
            if inserted.rows_affected() == 0 { return Ok(false); }
            transaction.execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE tracking_subscriptions SET known_episodes = known_episodes || $2::jsonb, next_check_at = $3, updated_at = now() WHERE id = $1",
                [
                    id.into_uuid().into(),
                    serde_json::json!([{"season": episode.season(), "episode": episode.episode()}]).into(),
                    next_check_at.into(),
                ],
            )).await?;
            let owner = UserId::from_uuid(row.try_get("", "owner_id")?);
            let title: String = row.try_get("", "title")?;
            let scope: String = row.try_get("", "scope")?;
            let message = format!(
                "📺 **Новая серия доступна**\n\n🎬 {title}\n🔔 S{:02}E{:02}\n\n➡️ **Дальше:** выберите источник — Rezka или Prowlarr",
                episode.season(), episode.episode()
            );
            let recipients = notification_recipients(owner, &scope)?;
            for recipient in recipients {
                transaction.execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO notification_outbox (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload) VALUES ($1, 'tracking', $2, 'future-episode-found', $3, $4, $5) ON CONFLICT (source_dedupe_key, recipient) DO NOTHING",
                    [
                        uuid::Uuid::new_v4().into(),
                        id.into_uuid().into(),
                        recipient.into(),
                        discovery_id.as_bytes().to_vec().into(),
                        serde_json::json!({"message": message}).into(),
                    ],
                )).await?;
            }
            Ok(true)
        }.await;
        finish(transaction, result).await
    }
}

#[async_trait::async_trait]
impl TrackingStore for SeaOrmTrackingStore {
    async fn add(
        &self,
        operation: OperationKey,
        value: NewTrackingSubscription,
    ) -> Result<TrackingSubscription, PortError> {
        let known = episode_json(value.known_episodes());
        let row = self.database.query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO tracking_subscriptions (id, owner_id, provider, title, translation, known_episodes, scope, created_operation_key) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (created_operation_key) DO UPDATE SET created_operation_key = EXCLUDED.created_operation_key RETURNING id, owner_id, provider, title, translation, known_episodes, scope",
            [
                value.id().into_uuid().into(), value.owner_id().into_uuid().into(), provider_value(value.provider()).into(),
                value.title().into(), value.translation().into(), known.into(), scope_value(value.scope()).into(), operation.as_bytes().to_vec().into(),
            ],
        )).await.map_err(map_database_error)?.ok_or(PortError::Infrastructure)?;
        tracking_from_row(&row)
    }

    async fn list_visible(&self, user: UserId) -> Result<Vec<TrackingSubscription>, PortError> {
        self.database.query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, owner_id, provider, title, translation, known_episodes, scope FROM tracking_subscriptions WHERE deleted_at IS NULL AND (owner_id = $1 OR (scope = 'family' AND $1 IN ($2, $3))) ORDER BY created_at, id",
            [user.into_uuid().into(), PRIMARY_USER_ID.into_uuid().into(), SECONDARY_USER_ID.into_uuid().into()],
        )).await.map_err(map_database_error)?.iter().map(tracking_from_row).collect()
    }

    async fn remove_visible(
        &self,
        operation: OperationKey,
        id: TrackingId,
        user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let row = self.database.query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE tracking_subscriptions SET deleted_at = COALESCE(deleted_at, now()), remove_operation_key = COALESCE(remove_operation_key, $3), updated_at = now() WHERE id = $1 AND (remove_operation_key = $3 OR (deleted_at IS NULL AND (owner_id = $2 OR (scope = 'family' AND $2 IN ($4, $5))))) RETURNING id, owner_id, provider, title, translation, known_episodes, scope",
            [id.into_uuid().into(), user.into_uuid().into(), operation.as_bytes().to_vec().into(), PRIMARY_USER_ID.into_uuid().into(), SECONDARY_USER_ID.into_uuid().into()],
        )).await.map_err(map_database_error)?;
        row.as_ref().map(tracking_from_row).transpose()
    }
}

#[async_trait::async_trait]
impl TrackingScheduleStore for SeaOrmTrackingStore {
    async fn list_due(
        &self,
        now: time::OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        if limit == 0 || limit > 100 {
            return Err(PortError::Conflict);
        }
        self.database
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, owner_id, provider, title, translation, known_episodes, scope \
                 FROM tracking_subscriptions WHERE deleted_at IS NULL AND next_check_at <= $1 \
                 ORDER BY next_check_at, created_at LIMIT $2",
                [now.into(), i64::from(limit).into()],
            ))
            .await
            .map_err(map_database_error)?
            .iter()
            .map(tracking_from_row)
            .collect()
    }

    async fn record_future_episode(
        &self,
        id: TrackingId,
        episode: EpisodeSnapshot,
        next_check_at: time::OffsetDateTime,
    ) -> Result<bool, PortError> {
        SeaOrmTrackingStore::record_future_episode(self, id, episode, next_check_at).await
    }

    async fn defer_check(
        &self,
        id: TrackingId,
        next_check_at: time::OffsetDateTime,
    ) -> Result<(), PortError> {
        let changed = self
            .database
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE tracking_subscriptions SET next_check_at = $2, updated_at = now() \
                 WHERE id = $1 AND deleted_at IS NULL",
                [id.into_uuid().into(), next_check_at.into()],
            ))
            .await
            .map_err(map_database_error)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }
}

#[derive(Clone)]
pub struct SeaOrmNotificationOutbox {
    database: DatabaseConnection,
}

impl SeaOrmNotificationOutbox {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }

    pub async fn lease_pending(
        &self,
        worker: NotificationId,
        now: time::OffsetDateTime,
        ttl: time::Duration,
        limit: u32,
    ) -> Result<Vec<NotificationDelivery>, PortError> {
        let ttl_seconds = ttl.whole_seconds();
        if !(1..=300).contains(&ttl_seconds) || limit == 0 || limit > 100 {
            return Err(PortError::Conflict);
        }
        self.database.query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "WITH pending AS (SELECT id FROM notification_outbox WHERE delivered_at IS NULL AND dead_at IS NULL AND next_attempt_at <= $1 AND (lease_expires_at IS NULL OR lease_expires_at <= $1) ORDER BY next_attempt_at, created_at FOR UPDATE SKIP LOCKED LIMIT $2) UPDATE notification_outbox n SET lease_owner = $3, lease_expires_at = $1 + make_interval(secs => $4) FROM pending WHERE n.id = pending.id RETURNING n.id, n.recipient, n.event_type, n.payload, n.attempt_count",
            [now.into(), i64::from(limit).into(), worker.into_uuid().into(), ttl_seconds.into()],
        )).await.map_err(map_database_error)?.iter().map(delivery_from_row).collect()
    }

    pub async fn mark_delivered(
        &self,
        id: NotificationId,
        worker: NotificationId,
    ) -> Result<(), PortError> {
        let changed = self.database.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE notification_outbox SET delivered_at = now(), lease_owner = NULL, lease_expires_at = NULL, last_error_code = NULL WHERE id = $1 AND lease_owner = $2 AND delivered_at IS NULL",
            [id.into_uuid().into(), worker.into_uuid().into()],
        )).await.map_err(map_database_error)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    pub async fn mark_failed(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        error_code: &str,
    ) -> Result<(), PortError> {
        if error_code.trim().is_empty() || error_code.contains("://") {
            return Err(PortError::Conflict);
        }
        let changed = self.database.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE notification_outbox SET attempt_count = attempt_count + 1, next_attempt_at = $3 + make_interval(secs => LEAST(3600, 30 * power(2, LEAST(attempt_count, 7))::bigint)), lease_owner = NULL, lease_expires_at = NULL, last_error_code = $4 WHERE id = $1 AND lease_owner = $2 AND delivered_at IS NULL AND dead_at IS NULL",
            [id.into_uuid().into(), worker.into_uuid().into(), now.into(), error_code.into()],
        )).await.map_err(map_database_error)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    pub async fn mark_dead(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        error_code: &str,
    ) -> Result<(), PortError> {
        if error_code.trim().is_empty() || error_code.contains("://") {
            return Err(PortError::Conflict);
        }
        let changed = self.database.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE notification_outbox SET attempt_count = attempt_count + 1, dead_at = $3, lease_owner = NULL, lease_expires_at = NULL, last_error_code = $4 WHERE id = $1 AND lease_owner = $2 AND delivered_at IS NULL AND dead_at IS NULL",
            [id.into_uuid().into(), worker.into_uuid().into(), now.into(), error_code.into()],
        )).await.map_err(map_database_error)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }
}

#[async_trait::async_trait]
impl NotificationOutboxPort for SeaOrmNotificationOutbox {
    async fn lease_pending(
        &self,
        worker: NotificationId,
        now: time::OffsetDateTime,
        ttl: time::Duration,
        limit: u32,
    ) -> Result<Vec<NotificationDelivery>, PortError> {
        SeaOrmNotificationOutbox::lease_pending(self, worker, now, ttl, limit).await
    }

    async fn mark_delivered(
        &self,
        id: NotificationId,
        worker: NotificationId,
    ) -> Result<(), PortError> {
        SeaOrmNotificationOutbox::mark_delivered(self, id, worker).await
    }

    async fn mark_failed(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        error_code: &str,
    ) -> Result<(), PortError> {
        SeaOrmNotificationOutbox::mark_failed(self, id, worker, now, error_code).await
    }

    async fn mark_dead(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        error_code: &str,
    ) -> Result<(), PortError> {
        SeaOrmNotificationOutbox::mark_dead(self, id, worker, now, error_code).await
    }
}

fn tracking_from_row(row: &sea_orm::QueryResult) -> Result<TrackingSubscription, PortError> {
    let known: serde_json::Value = row
        .try_get("", "known_episodes")
        .map_err(|_| PortError::Infrastructure)?;
    let episodes = known
        .as_array()
        .ok_or(PortError::Infrastructure)?
        .iter()
        .map(|item| {
            let season = item
                .get("season")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or(PortError::Infrastructure)?;
            let episode = item
                .get("episode")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or(PortError::Infrastructure)?;
            EpisodeSnapshot::new(season, episode).map_err(|_| PortError::Infrastructure)
        })
        .collect::<Result<Vec<_>, _>>()?;
    TrackingSubscription::rehydrate(
        TrackingId::from_uuid(
            row.try_get("", "id")
                .map_err(|_| PortError::Infrastructure)?,
        ),
        UserId::from_uuid(
            row.try_get("", "owner_id")
                .map_err(|_| PortError::Infrastructure)?,
        ),
        parse_provider(
            &row.try_get::<String>("", "provider")
                .map_err(|_| PortError::Infrastructure)?,
        )?,
        row.try_get("", "title")
            .map_err(|_| PortError::Infrastructure)?,
        row.try_get("", "translation")
            .map_err(|_| PortError::Infrastructure)?,
        episodes,
        parse_scope(
            &row.try_get::<String>("", "scope")
                .map_err(|_| PortError::Infrastructure)?,
        )?,
    )
    .map_err(|_| PortError::Infrastructure)
}

fn delivery_from_row(row: &sea_orm::QueryResult) -> Result<NotificationDelivery, PortError> {
    let payload: serde_json::Value = row
        .try_get("", "payload")
        .map_err(|_| PortError::Infrastructure)?;
    NotificationDelivery::rehydrate(
        NotificationId::from_uuid(
            row.try_get("", "id")
                .map_err(|_| PortError::Infrastructure)?,
        ),
        match row
            .try_get::<String>("", "recipient")
            .map_err(|_| PortError::Infrastructure)?
            .as_str()
        {
            "primary" => NotificationRecipient::Primary,
            "secondary" => NotificationRecipient::Secondary,
            _ => return Err(PortError::Infrastructure),
        },
        parse_event(
            &row.try_get::<String>("", "event_type")
                .map_err(|_| PortError::Infrastructure)?,
        )?,
        payload
            .get("message")
            .and_then(serde_json::Value::as_str)
            .ok_or(PortError::Infrastructure)?
            .to_owned(),
        u32::try_from(
            row.try_get::<i32>("", "attempt_count")
                .map_err(|_| PortError::Infrastructure)?,
        )
        .map_err(|_| PortError::Infrastructure)?,
    )
    .map_err(|_| PortError::Infrastructure)
}

fn episode_json(values: &[EpisodeSnapshot]) -> serde_json::Value {
    serde_json::Value::Array(
        values
            .iter()
            .map(|value| serde_json::json!({"season": value.season(), "episode": value.episode()}))
            .collect(),
    )
}
const fn provider_value(value: Provider) -> &'static str {
    match value {
        Provider::Rezka => "rezka",
        Provider::Prowlarr => "prowlarr",
    }
}
const fn scope_value(value: TrackingScope) -> &'static str {
    match value {
        TrackingScope::Personal => "personal",
        TrackingScope::Family => "family",
    }
}
fn parse_provider(value: &str) -> Result<Provider, PortError> {
    match value {
        "rezka" => Ok(Provider::Rezka),
        "prowlarr" => Ok(Provider::Prowlarr),
        _ => Err(PortError::Infrastructure),
    }
}
fn parse_scope(value: &str) -> Result<TrackingScope, PortError> {
    match value {
        "personal" => Ok(TrackingScope::Personal),
        "family" => Ok(TrackingScope::Family),
        _ => Err(PortError::Infrastructure),
    }
}
fn parse_event(value: &str) -> Result<NotificationEventType, PortError> {
    NotificationEventType::from_wire(value).ok_or(PortError::Infrastructure)
}
fn notification_recipients(
    owner: UserId,
    scope: &str,
) -> Result<Vec<&'static str>, sea_orm::DbErr> {
    match scope {
        "personal" if owner == PRIMARY_USER_ID => Ok(vec!["primary"]),
        "personal" if owner == SECONDARY_USER_ID => Ok(vec!["secondary"]),
        "family" => Ok(vec!["primary", "secondary"]),
        _ => Err(sea_orm::DbErr::Type(
            "invalid tracking notification route".to_owned(),
        )),
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
