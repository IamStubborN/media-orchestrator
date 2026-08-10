use media_core::{
    PRIMARY_USER_ID, EpisodeSnapshot, FutureEpisodeRecord, JobId, MediaNotification,
    MediaNotificationAction, MediaNotificationAudio, MediaNotificationDeliveryKind,
    MediaNotificationEpisode, MediaNotificationIssue, MediaNotificationKind,
    MediaNotificationLibrary, MediaNotificationMedia, MediaNotificationNextStep,
    MediaNotificationOrigin, MediaNotificationProcessing, MediaNotificationProcessingMode,
    MediaNotificationProgress, MediaNotificationPublication, MediaNotificationResult,
    MediaNotificationStage, MediaNotificationState, MediaNotificationSubtitles,
    MediaNotificationVideo, NewTrackingSubscription, NotificationDelivery, NotificationEventType,
    NotificationId, NotificationOutboxPort, NotificationRecipient, OperationKey, PortError,
    Provider, ReleaseIdentity, ReleaseSource, SourceChoiceAction, SourceChoiceNotification,
    TrackingCheckStatus, TrackingClaimToken, TrackingDownload, TrackingDownloadPatch, TrackingId,
    TrackingScheduleStore, TrackingScope, TrackingStore, TrackingSubscription, UserId,
    SECONDARY_USER_ID, episode_choice_set_id, is_valid_tracking_poster_url,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait};

use crate::repository::map_tracking_database_error;

#[derive(Clone)]
pub struct SeaOrmTrackingStore {
    database: DatabaseConnection,
}

impl SeaOrmTrackingStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }

    async fn ensure_active_claim(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
    ) -> Result<(), PortError> {
        let active = self
            .database
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT 1 AS active FROM tracking_subscriptions \
                 WHERE id = $1 AND deleted_at IS NULL AND check_claim_token = $2 \
                   AND check_claim_until > now()",
                [id.into_uuid().into(), claim_token.into_uuid().into()],
            ))
            .await
            .map_err(map_tracking_database_error)?;
        if active.is_some() {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    pub async fn record_future_episode(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
        next_check_at: time::OffsetDateTime,
        actions: Vec<SourceChoiceAction>,
        poster_url: Option<String>,
    ) -> Result<bool, PortError> {
        self.record_future_episode_with_counts(FutureEpisodeRecord {
            id,
            claim_token,
            episode,
            next_check_at,
            actions,
            poster_url,
            rezka_count: 0,
            prowlarr_count: 0,
        })
        .await
    }

    pub async fn record_future_episode_with_counts(
        &self,
        record: FutureEpisodeRecord,
    ) -> Result<bool, PortError> {
        let FutureEpisodeRecord {
            id,
            claim_token,
            episode,
            next_check_at,
            actions,
            poster_url,
            rezka_count,
            prowlarr_count,
        } = record;
        let transaction = self
            .database
            .begin()
            .await
            .map_err(map_tracking_database_error)?;
        let result = async {
            let row = transaction.query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT owner_id, title, scope, download_provider_media_ref FROM tracking_subscriptions WHERE id = $1 AND deleted_at IS NULL AND check_claim_token = $2 AND check_claim_until > now() FOR UPDATE",
                [id.into_uuid().into(), claim_token.into_uuid().into()],
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
            let updated = transaction.execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE tracking_subscriptions SET known_episodes = known_episodes || $2::jsonb, next_check_at = $3, poster_url = COALESCE(poster_url, $4), updated_at = now() WHERE id = $1 AND check_claim_token = $5 AND check_claim_until > now()",
                [
                    id.into_uuid().into(),
                    serde_json::json!([{"season": episode.season(), "episode": episode.episode()}]).into(),
                    next_check_at.into(),
                    poster_url.clone().into(),
                    claim_token.into_uuid().into(),
                ],
            )).await?;
            if updated.rows_affected() != 1 {
                return Err(sea_orm::DbErr::Custom("tracking claim expired".to_owned()));
            }
            transaction.execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM tracking_availability_candidates WHERE tracking_id = $1 AND season = $2 AND episode = $3",
                [
                    id.into_uuid().into(),
                    i32::try_from(episode.season()).map_err(|_| sea_orm::DbErr::Type("season is out of range".to_owned()))?.into(),
                    i32::try_from(episode.episode()).map_err(|_| sea_orm::DbErr::Type("episode is out of range".to_owned()))?.into(),
                ],
            )).await?;
            let owner = UserId::from_uuid(row.try_get("", "owner_id")?);
            let title: String = row.try_get("", "title")?;
            let scope: String = row.try_get("", "scope")?;
            let auto_download: Option<String> = row.try_get("", "download_provider_media_ref")?;
            if auto_download.is_some() {
                return Ok(true);
            }
            let card_key = format!(
                "tracking:{id}:{}:{}",
                episode.season(),
                episode.episode()
            );
            let source_choice = SourceChoiceNotification::new(
                card_key,
                id,
                title,
                episode.season(),
                episode.episode(),
                actions,
            )
            .map_err(|error| sea_orm::DbErr::Type(error.to_string()))?
            .with_poster_url(poster_url)
            .with_choice_set(
                episode_choice_set_id(id, episode.season(), episode.episode()),
                (time::OffsetDateTime::now_utc() + time::Duration::hours(24))
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|error| sea_orm::DbErr::Type(error.to_string()))?,
                rezka_count,
                prowlarr_count,
            );
            let payload = source_choice_payload(&source_choice);
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
                        payload.clone().into(),
                    ],
                )).await?;
            }
            Ok(true)
        }.await;
        finish(transaction, result).await
    }
}

fn source_choice_payload(notification: &SourceChoiceNotification) -> serde_json::Value {
    let actions = notification
        .actions()
        .iter()
        .map(|action| match action {
            SourceChoiceAction::All => "all",
            SourceChoiceAction::Rezka => "rezka",
            SourceChoiceAction::Prowlarr => "prowlarr",
        })
        .collect::<Vec<_>>();
    let mut payload = serde_json::json!({
        "event_type": "media.source-choice",
        "schema_version": 1,
        "card_key": notification.card_key(),
        "tracking_id": notification.tracking_id().to_string(),
        "title": notification.title(),
        "season": notification.season(),
        "episode": notification.episode(),
        "actions": actions,
    });
    if let Some(poster_url) = notification.poster_url() {
        payload["poster_url"] = serde_json::Value::String(poster_url.to_owned());
    }
    if let Some(choice_set_id) = notification.choice_set_id() {
        payload["choice_set_id"] = serde_json::Value::String(choice_set_id.to_owned());
    }
    if let Some(expires_at) = notification.choice_set_expires_at() {
        payload["choice_set_expires_at"] = serde_json::Value::String(expires_at.to_owned());
    }
    if let Some(count) = notification.rezka_count() {
        payload["rezka_count"] = serde_json::Value::Number(count.into());
    }
    if let Some(count) = notification.prowlarr_count() {
        payload["prowlarr_count"] = serde_json::Value::Number(count.into());
    }
    payload
}

#[async_trait::async_trait]
impl TrackingStore for SeaOrmTrackingStore {
    async fn add(
        &self,
        operation: OperationKey,
        value: NewTrackingSubscription,
    ) -> Result<TrackingSubscription, PortError> {
        let known = episode_json(value.known_episodes());
        let release_source_id = value
            .release_identity()
            .map(|identity| i64::try_from(identity.source_id()).map_err(|_| PortError::Conflict))
            .transpose()?;
        let row = self.database.query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO tracking_subscriptions (id, owner_id, provider, title, translation, known_episodes, scope, poster_url, release_source, release_source_id, download_provider_media_ref, download_translation_id, download_season, created_operation_key) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT (created_operation_key) DO UPDATE SET created_operation_key = EXCLUDED.created_operation_key RETURNING id, owner_id, provider, title, translation, known_episodes, scope, poster_url, release_source, release_source_id, download_provider_media_ref, download_translation_id, download_season, last_checked_at, next_check_at, check_status",
            [
                value.id().into_uuid().into(), value.owner_id().into_uuid().into(), provider_value(value.provider()).into(),
                value.title().into(), value.translation().into(), known.into(), scope_value(value.scope()).into(),
                value.poster_url().map(str::to_owned).into(),
                value.release_identity().map(|identity| identity.source().as_str().to_owned()).into(),
                release_source_id.into(),
                value.download().map(|download| download.provider_media_ref().to_owned()).into(),
                value.download().and_then(|download| i64::try_from(download.translation_id()).ok()).into(),
                value.download().and_then(|download| i32::try_from(download.season()).ok()).into(),
                operation.as_bytes().to_vec().into(),
            ],
        )).await.map_err(map_tracking_database_error)?.ok_or(PortError::Infrastructure)?;
        tracking_from_row(&row)
    }

    async fn list_visible(&self, user: UserId) -> Result<Vec<TrackingSubscription>, PortError> {
        self.database.query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, owner_id, provider, title, translation, known_episodes, scope, poster_url, release_source, release_source_id, download_provider_media_ref, download_translation_id, download_season, last_checked_at, next_check_at, check_status FROM tracking_subscriptions WHERE deleted_at IS NULL AND (owner_id = $1 OR (scope = 'family' AND $1 IN ($2, $3))) ORDER BY created_at, id",
            [user.into_uuid().into(), PRIMARY_USER_ID.into_uuid().into(), SECONDARY_USER_ID.into_uuid().into()],
        )).await.map_err(map_tracking_database_error)?.iter().map(tracking_from_row).collect()
    }

    async fn patch_download_visible(
        &self,
        id: TrackingId,
        user: UserId,
        patch: TrackingDownloadPatch,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let download = patch.download();
        let translation_id =
            i64::try_from(download.translation_id()).map_err(|_| PortError::Conflict)?;
        let season = i32::try_from(download.season()).map_err(|_| PortError::Conflict)?;
        let replacement_claim_token = TrackingClaimToken::new();
        let row = self.database.query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE tracking_subscriptions SET translation = $3, download_provider_media_ref = $4, download_translation_id = $5, download_season = $6, next_check_at = CASE WHEN check_claim_until > now() THEN next_check_at ELSE now() END, check_requested_at = CASE WHEN check_claim_until > now() THEN now() ELSE NULL END, check_claim_token = CASE WHEN check_claim_until > now() THEN $9 ELSE NULL END, check_claim_until = CASE WHEN check_claim_until > now() THEN check_claim_until ELSE NULL END, updated_at = now() WHERE id = $1 AND deleted_at IS NULL AND provider = 'rezka' AND (owner_id = $2 OR (scope = 'family' AND $2 IN ($7, $8))) RETURNING id, owner_id, provider, title, translation, known_episodes, scope, poster_url, release_source, release_source_id, download_provider_media_ref, download_translation_id, download_season, last_checked_at, next_check_at, check_status",
            [
                id.into_uuid().into(), user.into_uuid().into(), patch.translation().into(),
                download.provider_media_ref().into(), translation_id.into(), season.into(),
                PRIMARY_USER_ID.into_uuid().into(), SECONDARY_USER_ID.into_uuid().into(),
                replacement_claim_token.into_uuid().into(),
            ],
        )).await.map_err(map_tracking_database_error)?;
        row.as_ref().map(tracking_from_row).transpose()
    }

    async fn set_baseline_visible(
        &self,
        id: TrackingId,
        user: UserId,
        baseline: EpisodeSnapshot,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let season = i32::try_from(baseline.season()).map_err(|_| PortError::Conflict)?;
        let episode = i32::try_from(baseline.episode()).map_err(|_| PortError::Conflict)?;
        let replacement_claim_token = TrackingClaimToken::new();
        let transaction = self
            .database
            .begin()
            .await
            .map_err(map_tracking_database_error)?;
        let result = async {
            let row = transaction.query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE tracking_subscriptions
                 SET known_episodes =
                       COALESCE((
                           SELECT jsonb_agg(item.value ORDER BY (item.value->>'season')::int, (item.value->>'episode')::int)
                           FROM jsonb_array_elements(known_episodes) AS item(value)
                           WHERE (item.value->>'season')::int <> $3
                       ), '[]'::jsonb)
                       ||
                       COALESCE((
                           SELECT jsonb_agg(jsonb_build_object('season', $3, 'episode', value))
                           FROM generate_series(1, $4) AS value
                       ), '[]'::jsonb),
                     next_check_at = CASE WHEN check_claim_until > now() THEN next_check_at ELSE now() END,
                     check_requested_at = CASE WHEN check_claim_until > now() THEN now() ELSE NULL END,
                     check_claim_token = CASE WHEN check_claim_until > now() THEN $7 ELSE NULL END,
                     check_claim_until = CASE WHEN check_claim_until > now() THEN check_claim_until ELSE NULL END,
                     updated_at = now()
                 WHERE id = $1 AND deleted_at IS NULL
                   AND (owner_id = $2 OR (scope = 'family' AND $2 IN ($5, $6)))
                 RETURNING id, owner_id, provider, title, translation, known_episodes, scope, poster_url,
                           release_source, release_source_id,
                           download_provider_media_ref, download_translation_id, download_season,
                           last_checked_at, next_check_at, check_status",
                [
                    id.into_uuid().into(),
                    user.into_uuid().into(),
                    season.into(),
                    episode.into(),
                    PRIMARY_USER_ID.into_uuid().into(),
                    SECONDARY_USER_ID.into_uuid().into(),
                    replacement_claim_token.into_uuid().into(),
                ],
            )).await?;
            let Some(row) = row else {
                return Ok(None);
            };
            transaction.execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM tracking_availability_candidates
                 WHERE tracking_id = $1 AND season = $2 AND episode <= $3",
                [id.into_uuid().into(), season.into(), episode.into()],
            )).await?;
            transaction.execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM notification_outbox
                 WHERE aggregate_type = 'tracking' AND aggregate_id = $1
                   AND delivered_at IS NULL AND dead_at IS NULL
                   AND COALESCE((payload->>'season')::int, -1) = $2
                   AND COALESCE((payload->>'episode')::int, -1) <= $3",
                [id.into_uuid().into(), season.into(), episode.into()],
            )).await?;
            tracking_from_row(&row)
                .map(Some)
                .map_err(|_| sea_orm::DbErr::Type("invalid tracking row".to_owned()))
        }.await;
        finish(transaction, result).await
    }

    async fn request_check_visible(
        &self,
        id: TrackingId,
        user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let row = self
            .database
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE tracking_subscriptions
                 SET next_check_at = CASE WHEN check_claim_until > now() THEN next_check_at ELSE now() END,
                     check_requested_at = CASE WHEN check_claim_until > now() THEN now() ELSE NULL END,
                     check_claim_token = CASE WHEN check_claim_until > now() THEN check_claim_token ELSE NULL END,
                     check_claim_until = CASE WHEN check_claim_until > now() THEN check_claim_until ELSE NULL END,
                     updated_at = now()
             WHERE id = $1 AND deleted_at IS NULL
               AND (owner_id = $2 OR (scope = 'family' AND $2 IN ($3, $4)))
             RETURNING id, owner_id, provider, title, translation, known_episodes, scope, poster_url,
                       release_source, release_source_id,
                       download_provider_media_ref, download_translation_id, download_season,
                       last_checked_at, next_check_at, check_status",
                [
                    id.into_uuid().into(),
                    user.into_uuid().into(),
                    PRIMARY_USER_ID.into_uuid().into(),
                    SECONDARY_USER_ID.into_uuid().into(),
                ],
            ))
            .await
            .map_err(map_tracking_database_error)?;
        row.as_ref().map(tracking_from_row).transpose()
    }

    async fn remove_visible(
        &self,
        operation: OperationKey,
        id: TrackingId,
        user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let row = self.database.query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE tracking_subscriptions SET deleted_at = COALESCE(deleted_at, now()), remove_operation_key = COALESCE(remove_operation_key, $3), updated_at = now() WHERE id = $1 AND (remove_operation_key = $3 OR (deleted_at IS NULL AND (owner_id = $2 OR (scope = 'family' AND $2 IN ($4, $5))))) RETURNING id, owner_id, provider, title, translation, known_episodes, scope, poster_url, release_source, release_source_id, download_provider_media_ref, download_translation_id, download_season, last_checked_at, next_check_at, check_status",
            [id.into_uuid().into(), user.into_uuid().into(), operation.as_bytes().to_vec().into(), PRIMARY_USER_ID.into_uuid().into(), SECONDARY_USER_ID.into_uuid().into()],
        )).await.map_err(map_tracking_database_error)?;
        row.as_ref().map(tracking_from_row).transpose()
    }
}

#[async_trait::async_trait]
impl TrackingScheduleStore for SeaOrmTrackingStore {
    async fn claim_due(
        &self,
        now: time::OffsetDateTime,
        claim_token: TrackingClaimToken,
        claim_until: time::OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError> {
        if limit == 0 || limit > 100 || claim_until <= now {
            return Err(PortError::Conflict);
        }
        self.database
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "WITH due AS ( \
                     SELECT id FROM tracking_subscriptions \
                     WHERE deleted_at IS NULL \
                       AND (next_check_at <= $1 OR check_requested_at IS NOT NULL) \
                       AND (check_claim_until IS NULL OR check_claim_until <= $1) \
                     ORDER BY next_check_at, created_at \
                     FOR UPDATE SKIP LOCKED LIMIT $2 \
                 ) \
                 UPDATE tracking_subscriptions AS tracking \
                 SET check_claim_token = $3, check_claim_until = $4, \
                     next_check_at = CASE WHEN check_requested_at IS NULL THEN next_check_at \
                                          ELSE LEAST(next_check_at, check_requested_at) END, \
                     check_requested_at = NULL, updated_at = now() \
                 FROM due WHERE tracking.id = due.id \
                 RETURNING tracking.id, tracking.owner_id, tracking.provider, tracking.title, \
                           tracking.translation, tracking.known_episodes, tracking.scope, \
                           tracking.poster_url, tracking.release_source, tracking.release_source_id, \
                           tracking.download_provider_media_ref, tracking.download_translation_id, \
                           tracking.download_season, tracking.last_checked_at, tracking.next_check_at, \
                           tracking.check_status",
                [
                    now.into(),
                    i64::from(limit).into(),
                    claim_token.into_uuid().into(),
                    claim_until.into(),
                ],
            ))
            .await
            .map_err(map_tracking_database_error)?
            .iter()
            .map(tracking_from_row)
            .collect()
    }

    async fn set_release_metadata_if_missing(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        release_identity: ReleaseIdentity,
        poster_url: String,
    ) -> Result<(), PortError> {
        if !is_valid_tracking_poster_url(&poster_url) {
            return Err(PortError::Conflict);
        }
        let source_id =
            i64::try_from(release_identity.source_id()).map_err(|_| PortError::Conflict)?;
        let row = self
            .database
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "WITH active_claim AS ( \
                     SELECT id FROM tracking_subscriptions \
                     WHERE id = $1 AND deleted_at IS NULL \
                       AND check_claim_token = $5 AND check_claim_until > now() \
                 ), updated AS ( \
                     UPDATE tracking_subscriptions AS tracking \
                     SET release_source = COALESCE(release_source, $2), \
                         release_source_id = COALESCE(release_source_id, $3), \
                         poster_url = $4, updated_at = now() \
                     FROM active_claim \
                     WHERE tracking.id = active_claim.id AND tracking.poster_url IS NULL \
                       AND ((tracking.release_source = $2 AND tracking.release_source_id = $3) \
                            OR (tracking.release_source IS NULL AND tracking.release_source_id IS NULL)) \
                     RETURNING tracking.id \
                 ) SELECT EXISTS (SELECT 1 FROM active_claim) AS active",
                [
                    id.into_uuid().into(),
                    release_identity.source().as_str().to_owned().into(),
                    source_id.into(),
                    poster_url.into(),
                    claim_token.into_uuid().into(),
                ],
            ))
            .await
            .map_err(map_tracking_database_error)?
            .ok_or(PortError::Conflict)?;
        if row
            .try_get::<bool>("", "active")
            .map_err(map_tracking_database_error)?
        {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    async fn record_future_episode(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
        next_check_at: time::OffsetDateTime,
        actions: Vec<SourceChoiceAction>,
        poster_url: Option<String>,
    ) -> Result<bool, PortError> {
        SeaOrmTrackingStore::record_future_episode(
            self,
            id,
            claim_token,
            episode,
            next_check_at,
            actions,
            poster_url,
        )
        .await
    }

    async fn record_future_episode_with_counts(
        &self,
        record: FutureEpisodeRecord,
    ) -> Result<bool, PortError> {
        SeaOrmTrackingStore::record_future_episode_with_counts(self, record).await
    }

    async fn pending_episodes(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
    ) -> Result<Vec<EpisodeSnapshot>, PortError> {
        self.ensure_active_claim(id, claim_token).await?;
        self.database
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT season, episode FROM tracking_availability_candidates \
                 WHERE tracking_id = $1
                   AND EXISTS (SELECT 1 FROM tracking_subscriptions
                               WHERE id = $1 AND check_claim_token = $2
                                 AND check_claim_until > now())
                 ORDER BY season, episode",
                [id.into_uuid().into(), claim_token.into_uuid().into()],
            ))
            .await
            .map_err(map_tracking_database_error)?
            .iter()
            .map(|row| {
                let season = row
                    .try_get::<i32>("", "season")
                    .map_err(map_tracking_database_error)?;
                let episode = row
                    .try_get::<i32>("", "episode")
                    .map_err(map_tracking_database_error)?;
                EpisodeSnapshot::new(
                    u32::try_from(season).map_err(|_| PortError::Conflict)?,
                    u32::try_from(episode).map_err(|_| PortError::Conflict)?,
                )
                .map_err(|_| PortError::Conflict)
            })
            .collect()
    }

    async fn record_pending_episode(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        let changed = self
            .database
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO tracking_availability_candidates (tracking_id, season, episode) \
                 SELECT $1, $2, $3 FROM tracking_subscriptions \
                 WHERE id = $1 AND check_claim_token = $4 \
                   AND check_claim_until > now() \
                 ON CONFLICT (tracking_id, season, episode) DO UPDATE \
                   SET last_checked_at = now()",
                [
                    id.into_uuid().into(),
                    i32::try_from(episode.season())
                        .map_err(|_| PortError::Conflict)?
                        .into(),
                    i32::try_from(episode.episode())
                        .map_err(|_| PortError::Conflict)?
                        .into(),
                    claim_token.into_uuid().into(),
                ],
            ))
            .await
            .map_err(map_tracking_database_error)?;
        if changed.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    async fn reserve_episode_download(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        let reserved = self
            .database
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO tracking_download_reservations \
                     (tracking_id, season, episode, provider_media_ref, translation_id, download_season) \
                 SELECT id, $3, $4, download_provider_media_ref, download_translation_id, download_season \
                 FROM tracking_subscriptions \
                 WHERE id = $1 AND deleted_at IS NULL AND check_claim_token = $2 \
                   AND check_claim_until > now() AND download_provider_media_ref IS NOT NULL \
                 ON CONFLICT (tracking_id, season, episode) DO UPDATE \
                   SET provider_media_ref = EXCLUDED.provider_media_ref, \
                       translation_id = EXCLUDED.translation_id, \
                       download_season = EXCLUDED.download_season",
                [
                    id.into_uuid().into(),
                    claim_token.into_uuid().into(),
                    i32::try_from(episode.season())
                        .map_err(|_| PortError::Conflict)?
                        .into(),
                    i32::try_from(episode.episode())
                        .map_err(|_| PortError::Conflict)?
                        .into(),
                ],
            ))
            .await
            .map_err(map_tracking_database_error)?;
        if reserved.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    async fn release_episode_download(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        let released = self
            .database
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM tracking_download_reservations AS reservation \
                 USING tracking_subscriptions AS tracking \
                 WHERE reservation.tracking_id = $1 AND reservation.season = $3 \
                   AND reservation.episode = $4 AND tracking.id = reservation.tracking_id \
                   AND tracking.deleted_at IS NULL AND tracking.check_claim_token = $2 \
                   AND tracking.check_claim_until > now() \
                   AND reservation.provider_media_ref = tracking.download_provider_media_ref \
                   AND reservation.translation_id = tracking.download_translation_id \
                   AND reservation.download_season = tracking.download_season",
                [
                    id.into_uuid().into(),
                    claim_token.into_uuid().into(),
                    i32::try_from(episode.season())
                        .map_err(|_| PortError::Conflict)?
                        .into(),
                    i32::try_from(episode.episode())
                        .map_err(|_| PortError::Conflict)?
                        .into(),
                ],
            ))
            .await
            .map_err(map_tracking_database_error)?;
        if released.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PortError::Conflict)
        }
    }

    async fn finish_check(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        next_check_at: time::OffsetDateTime,
        status: TrackingCheckStatus,
    ) -> Result<(), PortError> {
        let changed = self
            .database
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE tracking_subscriptions
                 SET next_check_at = CASE WHEN check_requested_at IS NULL THEN $3 ELSE now() END,
                     last_checked_at = now(), check_status = $4,
                     check_claim_token = NULL, check_claim_until = NULL,
                     check_requested_at = NULL, updated_at = now() \
                 WHERE id = $1 AND deleted_at IS NULL AND check_claim_token = $2 \
                   AND check_claim_until > now()",
                [
                    id.into_uuid().into(),
                    claim_token.into_uuid().into(),
                    next_check_at.into(),
                    tracking_check_status_value(status).into(),
                ],
            ))
            .await
            .map_err(map_tracking_database_error)?;
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
            "WITH pending AS (SELECT id FROM notification_outbox WHERE delivered_at IS NULL AND dead_at IS NULL AND next_attempt_at <= $1 AND (lease_expires_at IS NULL OR lease_expires_at <= $1) ORDER BY next_attempt_at, created_at FOR UPDATE SKIP LOCKED LIMIT $2) UPDATE notification_outbox n SET lease_owner = $3, lease_expires_at = $1 + make_interval(secs => $4) FROM pending WHERE n.id = pending.id RETURNING n.id, n.aggregate_type, n.aggregate_id, n.recipient, n.event_type, n.payload, n.generation, n.attempt_count",
            [now.into(), i64::from(limit).into(), worker.into_uuid().into(), ttl_seconds.into()],
        )).await.map_err(map_tracking_database_error)?.iter().map(delivery_from_row).collect()
    }

    pub async fn mark_delivered(
        &self,
        id: NotificationId,
        worker: NotificationId,
        generation: u64,
    ) -> Result<(), PortError> {
        let generation = i64::try_from(generation).map_err(|_| PortError::Conflict)?;
        let changed = self
            .database
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE notification_outbox SET \
             delivered_at = CASE WHEN generation = $3 THEN now() ELSE NULL END, \
             next_attempt_at = CASE WHEN generation = $3 THEN next_attempt_at ELSE now() END, \
             lease_owner = NULL, lease_expires_at = NULL, \
             last_error_code = CASE WHEN generation = $3 THEN NULL ELSE last_error_code END \
             WHERE id = $1 AND lease_owner = $2 AND delivered_at IS NULL",
                [
                    id.into_uuid().into(),
                    worker.into_uuid().into(),
                    generation.into(),
                ],
            ))
            .await
            .map_err(map_tracking_database_error)?;
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
        generation: u64,
        error_code: &str,
    ) -> Result<(), PortError> {
        if error_code.trim().is_empty() || error_code.contains("://") {
            return Err(PortError::Conflict);
        }
        let generation = i64::try_from(generation).map_err(|_| PortError::Conflict)?;
        let changed = self.database.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE notification_outbox SET \
             attempt_count = CASE WHEN generation = $4 THEN attempt_count + 1 ELSE attempt_count END, \
             next_attempt_at = CASE WHEN generation = $4 \
               THEN $3 + make_interval(secs => LEAST(3600, 30 * power(2, LEAST(attempt_count, 7))::bigint)) \
               ELSE now() END, \
             lease_owner = NULL, lease_expires_at = NULL, \
             last_error_code = CASE WHEN generation = $4 THEN $5 ELSE last_error_code END \
             WHERE id = $1 AND lease_owner = $2 AND delivered_at IS NULL AND dead_at IS NULL",
            [id.into_uuid().into(), worker.into_uuid().into(), now.into(), generation.into(), error_code.into()],
        )).await.map_err(map_tracking_database_error)?;
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
        generation: u64,
        error_code: &str,
    ) -> Result<(), PortError> {
        if error_code.trim().is_empty() || error_code.contains("://") {
            return Err(PortError::Conflict);
        }
        let generation = i64::try_from(generation).map_err(|_| PortError::Conflict)?;
        let changed = self.database.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE notification_outbox SET \
             attempt_count = CASE WHEN generation = $4 THEN attempt_count + 1 ELSE attempt_count END, \
             dead_at = CASE WHEN generation = $4 THEN $3 ELSE NULL END, \
             next_attempt_at = CASE WHEN generation = $4 THEN next_attempt_at ELSE now() END, \
             lease_owner = NULL, lease_expires_at = NULL, \
             last_error_code = CASE WHEN generation = $4 THEN $5 ELSE last_error_code END \
             WHERE id = $1 AND lease_owner = $2 AND delivered_at IS NULL AND dead_at IS NULL",
            [id.into_uuid().into(), worker.into_uuid().into(), now.into(), generation.into(), error_code.into()],
        )).await.map_err(map_tracking_database_error)?;
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
        generation: u64,
    ) -> Result<(), PortError> {
        SeaOrmNotificationOutbox::mark_delivered(self, id, worker, generation).await
    }

    async fn mark_failed(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        generation: u64,
        error_code: &str,
    ) -> Result<(), PortError> {
        SeaOrmNotificationOutbox::mark_failed(self, id, worker, now, generation, error_code).await
    }

    async fn mark_dead(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        generation: u64,
        error_code: &str,
    ) -> Result<(), PortError> {
        SeaOrmNotificationOutbox::mark_dead(self, id, worker, now, generation, error_code).await
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
    let provider_media_ref: Option<String> = row
        .try_get("", "download_provider_media_ref")
        .map_err(|_| PortError::Infrastructure)?;
    let translation_id: Option<i64> = row
        .try_get("", "download_translation_id")
        .map_err(|_| PortError::Infrastructure)?;
    let season: Option<i32> = row
        .try_get("", "download_season")
        .map_err(|_| PortError::Infrastructure)?;
    let download = match (provider_media_ref, translation_id, season) {
        (Some(provider_media_ref), Some(translation_id), Some(season)) => Some(
            TrackingDownload::new(
                provider_media_ref,
                u64::try_from(translation_id).map_err(|_| PortError::Infrastructure)?,
                u32::try_from(season).map_err(|_| PortError::Infrastructure)?,
            )
            .map_err(|_| PortError::Infrastructure)?,
        ),
        (None, None, None) => None,
        _ => return Err(PortError::Infrastructure),
    };
    let release_source: Option<String> = row
        .try_get("", "release_source")
        .map_err(|_| PortError::Infrastructure)?;
    let release_source_id: Option<i64> = row
        .try_get("", "release_source_id")
        .map_err(|_| PortError::Infrastructure)?;
    let release_identity = match (release_source.as_deref(), release_source_id) {
        (Some("tvmaze"), Some(source_id)) => Some(
            ReleaseIdentity::new(
                ReleaseSource::Tvmaze,
                u64::try_from(source_id).map_err(|_| PortError::Infrastructure)?,
            )
            .map_err(|_| PortError::Infrastructure)?,
        ),
        (None, None) => None,
        _ => return Err(PortError::Infrastructure),
    };
    let last_checked_at = row
        .try_get("", "last_checked_at")
        .map_err(|_| PortError::Infrastructure)?;
    let next_check_at = row
        .try_get("", "next_check_at")
        .map_err(|_| PortError::Infrastructure)?;
    let check_status = parse_tracking_check_status(
        &row.try_get::<String>("", "check_status")
            .map_err(|_| PortError::Infrastructure)?,
    )?;
    TrackingSubscription::rehydrate_with_check_identity_and_poster(
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
        release_identity,
        download,
        row.try_get("", "poster_url")
            .map_err(|_| PortError::Infrastructure)?,
        last_checked_at,
        next_check_at,
        check_status,
    )
    .map_err(|_| PortError::Infrastructure)
}

const fn tracking_check_status_value(status: TrackingCheckStatus) -> &'static str {
    match status {
        TrackingCheckStatus::Never => "never",
        TrackingCheckStatus::NoNewEpisode => "no_new_episode",
        TrackingCheckStatus::AwaitingSource => "awaiting_source",
        TrackingCheckStatus::EpisodeFound => "episode_found",
        TrackingCheckStatus::DownloadQueued => "download_queued",
        TrackingCheckStatus::ReleaseError => "release_error",
        TrackingCheckStatus::SourceError => "source_error",
    }
}

fn parse_tracking_check_status(value: &str) -> Result<TrackingCheckStatus, PortError> {
    match value {
        "never" => Ok(TrackingCheckStatus::Never),
        "no_new_episode" => Ok(TrackingCheckStatus::NoNewEpisode),
        "awaiting_source" => Ok(TrackingCheckStatus::AwaitingSource),
        "episode_found" => Ok(TrackingCheckStatus::EpisodeFound),
        "download_queued" => Ok(TrackingCheckStatus::DownloadQueued),
        "release_error" => Ok(TrackingCheckStatus::ReleaseError),
        "source_error" => Ok(TrackingCheckStatus::SourceError),
        _ => Err(PortError::Infrastructure),
    }
}

fn delivery_from_row(row: &sea_orm::QueryResult) -> Result<NotificationDelivery, PortError> {
    let payload: serde_json::Value = row
        .try_get("", "payload")
        .map_err(|_| PortError::Infrastructure)?;
    let event_type = parse_event(
        &row.try_get::<String>("", "event_type")
            .map_err(|_| PortError::Infrastructure)?,
    )?;
    let id = NotificationId::from_uuid(
        row.try_get("", "id")
            .map_err(|_| PortError::Infrastructure)?,
    );
    let recipient = match row
        .try_get::<String>("", "recipient")
        .map_err(|_| PortError::Infrastructure)?
        .as_str()
    {
        "primary" => NotificationRecipient::Primary,
        "secondary" => NotificationRecipient::Secondary,
        _ => return Err(PortError::Infrastructure),
    };
    let generation = u64::try_from(
        row.try_get::<i64>("", "generation")
            .map_err(|_| PortError::Infrastructure)?,
    )
    .map_err(|_| PortError::Infrastructure)?;
    let attempts = u32::try_from(
        row.try_get::<i32>("", "attempt_count")
            .map_err(|_| PortError::Infrastructure)?,
    )
    .map_err(|_| PortError::Infrastructure)?;
    if payload.get("schema_version") == Some(&serde_json::json!(2)) {
        return NotificationDelivery::rehydrate_media(
            id,
            recipient,
            event_type,
            media_notification_from_payload(&payload)?,
            generation,
            attempts,
        )
        .map_err(|_| PortError::Infrastructure);
    }
    if payload.get("schema_version") == Some(&serde_json::json!(1))
        && payload
            .get("event_type")
            .and_then(serde_json::Value::as_str)
            == Some("media.source-choice")
    {
        return NotificationDelivery::rehydrate_source_choice(
            id,
            recipient,
            event_type,
            source_choice_from_payload(&payload)?,
            generation,
            attempts,
        )
        .map_err(|_| PortError::Infrastructure);
    }
    let aggregate_type = row
        .try_get::<String>("", "aggregate_type")
        .map_err(|_| PortError::Infrastructure)?;
    NotificationDelivery::rehydrate(
        id,
        recipient,
        event_type,
        match aggregate_type.as_str() {
            "job" => Some(format!(
                "media-job:{}",
                row.try_get::<uuid::Uuid>("", "aggregate_id")
                    .map_err(|_| PortError::Infrastructure)?
            )),
            "tracking" => None,
            _ => return Err(PortError::Infrastructure),
        },
        payload
            .get("message")
            .and_then(serde_json::Value::as_str)
            .ok_or(PortError::Infrastructure)?
            .to_owned(),
        generation,
        attempts,
    )
    .map_err(|_| PortError::Infrastructure)
}

fn source_choice_from_payload(
    payload: &serde_json::Value,
) -> Result<SourceChoiceNotification, PortError> {
    let string = |name: &str| {
        payload
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or(PortError::Infrastructure)
    };
    let tracking_id = string("tracking_id")?
        .parse::<uuid::Uuid>()
        .map(TrackingId::from_uuid)
        .map_err(|_| PortError::Infrastructure)?;
    let season = payload
        .get("season")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(PortError::Infrastructure)?;
    let episode = payload
        .get("episode")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(PortError::Infrastructure)?;
    let actions = payload
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .ok_or(PortError::Infrastructure)?
        .iter()
        .map(|value| match value.as_str() {
            Some("all") => Ok(SourceChoiceAction::All),
            Some("rezka") => Ok(SourceChoiceAction::Rezka),
            Some("prowlarr") => Ok(SourceChoiceAction::Prowlarr),
            _ => Err(PortError::Infrastructure),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let choice_set = match (
        payload
            .get("choice_set_id")
            .and_then(serde_json::Value::as_str),
        payload
            .get("choice_set_expires_at")
            .and_then(serde_json::Value::as_str),
        payload
            .get("rezka_count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        payload
            .get("prowlarr_count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
    ) {
        (None, None, None, None) => None,
        (Some(id), Some(expires_at), Some(rezka), Some(prowlarr)) => {
            Some((id.to_owned(), expires_at.to_owned(), rezka, prowlarr))
        }
        _ => return Err(PortError::Infrastructure),
    };
    let notification = SourceChoiceNotification::new(
        string("card_key")?.to_owned(),
        tracking_id,
        string("title")?.to_owned(),
        season,
        episode,
        actions,
    )
    .map_err(|_| PortError::Infrastructure)?
    .with_poster_url(
        payload
            .get("poster_url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    );
    Ok(match choice_set {
        Some((id, expires_at, rezka, prowlarr)) => {
            notification.with_choice_set(id, expires_at, rezka, prowlarr)
        }
        None => notification,
    })
}

fn media_notification_from_payload(
    payload: &serde_json::Value,
) -> Result<MediaNotification, PortError> {
    let value = |name: &str| {
        payload
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or(PortError::Infrastructure)
    };
    let media = payload
        .get("media")
        .and_then(serde_json::Value::as_object)
        .ok_or(PortError::Infrastructure)?;
    let media_value = |name: &str| {
        media
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or(PortError::Infrastructure)
    };
    let job_id = media_value("job_id")?
        .parse::<uuid::Uuid>()
        .map_err(|_| PortError::Infrastructure)?;
    let kind = match media_value("kind")? {
        "movie" => MediaNotificationKind::Movie,
        "series" => MediaNotificationKind::Series,
        _ => return Err(PortError::Infrastructure),
    };
    let origin = match media.get("origin").and_then(serde_json::Value::as_str) {
        None => None,
        Some("tracked-episode") => Some(MediaNotificationOrigin::TrackedEpisode),
        _ => return Err(PortError::Infrastructure),
    };
    let poster_url = media
        .get("poster_url")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let mut media = MediaNotificationMedia::new(
        JobId::from_uuid(job_id),
        media_value("title")?.to_owned(),
        kind,
        media_value("provider")?.to_owned(),
        media
            .get("season")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        media
            .get("translation")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    )
    .map_err(|_| PortError::Infrastructure)?;
    if let Some(origin) = origin {
        media = media.with_origin(origin);
    }
    media = media.with_poster_url(poster_url);
    let progress = payload
        .get("progress")
        .map(|progress| {
            let object = progress.as_object().ok_or(PortError::Infrastructure)?;
            let number = |name: &str| {
                object
                    .get(name)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
            };
            let missing = object
                .get("missing_episodes")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .map(|episode| {
                    MediaNotificationEpisode::new(
                        u32::try_from(
                            episode
                                .get("season")
                                .and_then(serde_json::Value::as_u64)
                                .ok_or(PortError::Infrastructure)?,
                        )
                        .map_err(|_| PortError::Infrastructure)?,
                        u32::try_from(
                            episode
                                .get("episode")
                                .and_then(serde_json::Value::as_u64)
                                .ok_or(PortError::Infrastructure)?,
                        )
                        .map_err(|_| PortError::Infrastructure)?,
                    )
                    .map_err(|_| PortError::Infrastructure)
                })
                .collect::<Result<Vec<_>, _>>()?;
            MediaNotificationProgress::new(
                number("completed_episodes"),
                number("total_episodes"),
                number("current_episode"),
                missing,
                object
                    .get("downloaded_bytes")
                    .and_then(serde_json::Value::as_u64),
                object
                    .get("download_speed_bps")
                    .and_then(serde_json::Value::as_u64),
                object
                    .get("percentage")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u8::try_from(value).ok()),
            )
            .and_then(|progress| {
                progress.with_transfer_details(
                    object
                        .get("total_bytes")
                        .and_then(serde_json::Value::as_u64),
                    object
                        .get("eta_seconds")
                        .and_then(serde_json::Value::as_u64),
                    object.get("seeds").and_then(serde_json::Value::as_u64),
                    object.get("peers").and_then(serde_json::Value::as_u64),
                    object
                        .get("source_state")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned),
                )
            })
            .and_then(|progress| {
                progress.with_recovery(
                    number("connection_attempt"),
                    number("connection_attempt_limit"),
                    object
                        .get("vpn_rotation_pending")
                        .and_then(serde_json::Value::as_bool),
                )
            })
            .and_then(|progress| {
                progress.with_storage(
                    object
                        .get("storage_available_bytes")
                        .and_then(serde_json::Value::as_u64),
                    object
                        .get("storage_required_bytes")
                        .and_then(serde_json::Value::as_u64),
                )
            })
            .map_err(|_| PortError::Infrastructure)
        })
        .transpose()?;
    let issue = payload
        .get("issue")
        .map(|issue| {
            MediaNotificationIssue::new(
                issue
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(PortError::Infrastructure)?
                    .to_owned(),
                issue
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(PortError::Infrastructure)?
                    .to_owned(),
            )
            .map_err(|_| PortError::Infrastructure)
        })
        .transpose()?;
    let delivery_kind = match value("delivery_kind")? {
        "card" => MediaNotificationDeliveryKind::Card,
        "final-push" => MediaNotificationDeliveryKind::FinalPush,
        _ => return Err(PortError::Infrastructure),
    };
    let state = match value("state")? {
        "queued" => MediaNotificationState::Queued,
        "downloading" => MediaNotificationState::Downloading,
        "processing" => MediaNotificationState::Processing,
        "publishing" => MediaNotificationState::Publishing,
        "completed" => MediaNotificationState::Completed,
        "partial" => MediaNotificationState::Partial,
        "failed" => MediaNotificationState::Failed,
        "cancelled" => MediaNotificationState::Cancelled,
        "needs-action" => MediaNotificationState::NeedsAction,
        _ => return Err(PortError::Infrastructure),
    };
    let stage = match payload.get("stage").and_then(serde_json::Value::as_str) {
        None => None,
        Some("download") => Some(MediaNotificationStage::Download),
        Some("process") => Some(MediaNotificationStage::Process),
        Some("publish") => Some(MediaNotificationStage::Publish),
        _ => return Err(PortError::Infrastructure),
    };
    let next_step = match payload.get("next_step").and_then(serde_json::Value::as_str) {
        None => None,
        Some("download") => Some(MediaNotificationNextStep::Download),
        Some("process") => Some(MediaNotificationNextStep::Process),
        Some("publish") => Some(MediaNotificationNextStep::Publish),
        Some("none") => Some(MediaNotificationNextStep::None),
        _ => return Err(PortError::Infrastructure),
    };
    let result = payload
        .get("result")
        .map(media_result_from_payload)
        .transpose()?;
    let actions = payload
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|action| match action.as_str() {
            Some("cancel") => Ok(MediaNotificationAction::Cancel),
            Some("details") => Ok(MediaNotificationAction::Details),
            Some("retry") => Ok(MediaNotificationAction::Retry),
            Some("retry-missing") => Ok(MediaNotificationAction::RetryMissing),
            Some("resume-storage") => Ok(MediaNotificationAction::ResumeStorage),
            Some("search-alternative") => Ok(MediaNotificationAction::SearchAlternative),
            _ => Err(PortError::Infrastructure),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let notification = MediaNotification::new(
        delivery_kind,
        value("card_key")?.to_owned(),
        payload
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .ok_or(PortError::Infrastructure)?,
        payload
            .get("lifecycle_cycle")
            .and_then(serde_json::Value::as_u64)
            .ok_or(PortError::Infrastructure)?,
        payload
            .get("terminal")
            .and_then(serde_json::Value::as_bool)
            .ok_or(PortError::Infrastructure)?,
        state,
        media,
        progress,
        stage,
        next_step,
        issue,
        actions,
    )
    .map_err(|_| PortError::Infrastructure)?;
    match result {
        Some(result) => notification
            .with_result(result)
            .map_err(|_| PortError::Infrastructure),
        None => Ok(notification),
    }
}

fn media_result_from_payload(
    result: &serde_json::Value,
) -> Result<MediaNotificationResult, PortError> {
    let object = result.as_object().ok_or(PortError::Infrastructure)?;
    let video = object
        .get("video")
        .map(|video| {
            MediaNotificationVideo::new(
                required_string(video, "codec")?,
                optional_string(video, "profile")?,
                required_u32(video, "width")?,
                required_u32(video, "height")?,
            )
            .map_err(|_| PortError::Infrastructure)
        })
        .transpose()?;
    let audio = object
        .get("audio")
        .map(|audio| {
            MediaNotificationAudio::new(
                optional_string(audio, "language")?,
                required_string(audio, "codec")?,
                optional_u32(audio, "channels")?,
                optional_string(audio, "channel_layout")?,
                optional_string(audio, "title")?,
            )
            .map_err(|_| PortError::Infrastructure)
        })
        .transpose()?;
    let subtitles = object
        .get("subtitles")
        .map(|subtitles| {
            Ok(MediaNotificationSubtitles::new(
                required_u32(subtitles, "downloaded")?,
                required_u32(subtitles, "missing")?,
            ))
        })
        .transpose()?;
    let processing = object
        .get("processing")
        .map(|processing| {
            let mode = match required_str(processing, "mode")? {
                "vaapi-upscale" => MediaNotificationProcessingMode::VaapiUpscale,
                "original" => MediaNotificationProcessingMode::Original,
                _ => return Err(PortError::Infrastructure),
            };
            Ok(MediaNotificationProcessing::new(
                mode,
                optional_u64(processing, "elapsed_seconds")?,
            ))
        })
        .transpose()?;
    let publication = object
        .get("publication")
        .map(|publication| {
            let library = match required_str(publication, "library")? {
                "movies" => MediaNotificationLibrary::Movies,
                "tv-shows" => MediaNotificationLibrary::TvShows,
                _ => return Err(PortError::Infrastructure),
            };
            MediaNotificationPublication::new(
                library,
                required_string(publication, "title")?,
                optional_u32(publication, "season")?,
                optional_u32(publication, "episode")?,
            )
            .map_err(|_| PortError::Infrastructure)
        })
        .transpose()?;
    Ok(MediaNotificationResult::new(
        video,
        audio,
        subtitles,
        optional_u64(result, "file_size_bytes")?,
        optional_u64(result, "duration_seconds")?,
        processing,
        publication,
    ))
}

fn required_str<'a>(value: &'a serde_json::Value, name: &str) -> Result<&'a str, PortError> {
    value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or(PortError::Infrastructure)
}

fn required_string(value: &serde_json::Value, name: &str) -> Result<String, PortError> {
    required_str(value, name).map(ToOwned::to_owned)
}

fn optional_string(value: &serde_json::Value, name: &str) -> Result<Option<String>, PortError> {
    value
        .get(name)
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or(PortError::Infrastructure)
        })
        .transpose()
}

fn required_u32(value: &serde_json::Value, name: &str) -> Result<u32, PortError> {
    value
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(PortError::Infrastructure)
}

fn optional_u32(value: &serde_json::Value, name: &str) -> Result<Option<u32>, PortError> {
    value
        .get(name)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or(PortError::Infrastructure)
        })
        .transpose()
}

fn optional_u64(value: &serde_json::Value, name: &str) -> Result<Option<u64>, PortError> {
    value
        .get(name)
        .map(|value| value.as_u64().ok_or(PortError::Infrastructure))
        .transpose()
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
            transaction
                .commit()
                .await
                .map_err(map_tracking_database_error)?;
            Ok(value)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(map_tracking_database_error(error))
        }
    }
}
