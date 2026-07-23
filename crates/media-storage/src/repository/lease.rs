use media_core::{
    PRIMARY_USER_ID, Checkpoint, CheckpointValue, ClientId, Job, JobEvent, JobEventId, JobEventKind,
    JobLease, JobState, LeaseId, LeaseStore, MAX_STICKY_VPN_ATTEMPTS, MediaNotificationAudio,
    MediaNotificationLibrary, MediaNotificationProcessing, MediaNotificationProcessingMode,
    MediaNotificationPublication, MediaNotificationResult, MediaNotificationSubtitles,
    MediaNotificationVideo, NotifyScope, OperationKey, PortError, Provider, StageFailureOutcome,
    StageRef, SECONDARY_USER_ID, max_stage_attempts,
};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, Statement, TransactionTrait,
    prelude::Uuid,
};

use crate::{
    entity::job,
    mapping::{job_state_value, needs_action_reason_value},
    repository::{
        job::insert_outbox,
        map_database_error,
        operation::{self, OperationClaim, OperationKind, OperationResult},
    },
};

const LEASE_ADVISORY_LOCK: i64 = 0x4d45_4449_414c_5345;
enum LeaseDecision {
    Available(Option<JobLease>),
    RotationRequired,
}

#[derive(Clone)]
pub struct SeaOrmLeaseStore {
    database: DatabaseConnection,
}

impl std::fmt::Debug for SeaOrmLeaseStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeaOrmLeaseStore { database: [REDACTED] }")
    }
}

impl SeaOrmLeaseStore {
    #[must_use]
    pub fn new(database: DatabaseConnection) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl LeaseStore for SeaOrmLeaseStore {
    async fn lease_next(
        &self,
        operation: OperationKey,
        runner: ClientId,
        ttl: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        let ttl_seconds = valid_ttl_seconds(ttl)?;
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::LeaseNext).await? {
                OperationClaim::Replay(result) => {
                    return replayed_lease(result).map(LeaseDecision::Available);
                }
                OperationClaim::Fresh => {}
            }
            let decision = lease_next_in_transaction(&transaction, runner, ttl_seconds).await?;
            let stored = match &decision {
                LeaseDecision::Available(lease) => lease
                    .clone()
                    .map_or(OperationResult::None, OperationResult::Lease),
                LeaseDecision::RotationRequired => OperationResult::None,
            };
            operation::complete(&transaction, operation, OperationKind::LeaseNext, &stored).await?;
            Ok(decision)
        }
        .await;
        match finish(transaction, result).await? {
            LeaseDecision::Available(lease) => Ok(lease),
            LeaseDecision::RotationRequired => Err(PortError::Conflict),
        }
    }

    async fn heartbeat(
        &self,
        operation: OperationKey,
        lease: LeaseId,
        runner: ClientId,
        ttl: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        let ttl_seconds = valid_ttl_seconds(ttl)?;
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::Heartbeat).await? {
                OperationClaim::Replay(result) => return replayed_lease(result),
                OperationClaim::Fresh => {}
            }
            let renewed =
                heartbeat_in_transaction(&transaction, lease, runner, ttl_seconds).await?;
            let stored = renewed
                .clone()
                .map_or(OperationResult::None, OperationResult::Lease);
            operation::complete(&transaction, operation, OperationKind::Heartbeat, &stored).await?;
            Ok(renewed)
        }
        .await;
        finish(transaction, result).await
    }

    async fn report_event(
        &self,
        operation: OperationKey,
        lease: LeaseId,
        runner: ClientId,
        event: JobEvent,
    ) -> Result<Option<Job>, PortError> {
        let transaction = self.database.begin().await.map_err(map_database_error)?;
        let result = async {
            match operation::claim(&transaction, operation, OperationKind::ReportEvent).await? {
                OperationClaim::Replay(OperationResult::Job(job)) => return Ok(Some(job)),
                OperationClaim::Replay(OperationResult::None) => return Ok(None),
                OperationClaim::Replay(OperationResult::Lease(_)) => {
                    return Err(sea_orm::DbErr::Type(
                        "report-event operation has an invalid result".to_owned(),
                    ));
                }
                OperationClaim::Fresh => {}
            }
            let Some(lease_row) = transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT job_id FROM job_leases WHERE id = $1 AND runner_client_id = $2 \
                     AND expires_at > now() FOR UPDATE",
                    [lease.into_uuid().into(), runner.into_uuid().into()],
                ))
                .await?
            else {
                operation::complete(
                    &transaction,
                    operation,
                    OperationKind::ReportEvent,
                    &OperationResult::None,
                )
                .await?;
                return Ok(None);
            };
            let job_id = lease_row.try_get::<Uuid>("", "job_id")?;
            let current = load_job(&transaction, job_id).await?;
            let (event_type, payload) = event_payload(&event);
            let inserted = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO job_events \
                     (id, job_id, lease_id, runner_client_id, event_type, payload) \
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
                    [
                        event.id().into_uuid().into(),
                        job_id.into(),
                        lease.into_uuid().into(),
                        runner.into_uuid().into(),
                        event_type.into(),
                        payload.clone().into(),
                    ],
                ))
                .await?;
            let updated = if inserted.rows_affected() == 0 {
                current
            } else {
                apply_event(&transaction, lease, current, &event).await?
            };
            if inserted.rows_affected() == 1 {
                insert_outbox(
                    &transaction,
                    updated.id(),
                    event_type,
                    event.id().into_uuid().as_bytes().to_vec(),
                    payload,
                )
                .await?;
                insert_notification_outbox(&transaction, &updated, &event).await?;
            }
            operation::complete(
                &transaction,
                operation,
                OperationKind::ReportEvent,
                &OperationResult::Job(updated.clone()),
            )
            .await?;
            Ok(Some(updated))
        }
        .await;
        finish(transaction, result).await
    }
}

pub(super) async fn insert_notification_outbox(
    transaction: &sea_orm::DatabaseTransaction,
    job: &Job,
    event: &JobEvent,
) -> Result<(), sea_orm::DbErr> {
    let Some(projected) = project_notification(transaction, job, event).await? else {
        return Ok(());
    };
    let initiator = match job.owner_id() {
        owner if owner == PRIMARY_USER_ID => "primary",
        owner if owner == SECONDARY_USER_ID => "secondary",
        _ => {
            return Err(sea_orm::DbErr::Type(
                "job owner has no notification route".to_owned(),
            ));
        }
    };
    let scope_recipients: Vec<&str> = match job.notify_scope() {
        NotifyScope::Family => vec!["primary", "secondary"],
        NotifyScope::Initiator => vec![initiator],
    };
    let recipients: &[&str] = if projected.terminal {
        &scope_recipients
    } else {
        std::slice::from_ref(&initiator)
    };
    for recipient in recipients {
        insert_projected_notification(transaction, job, recipient, &projected).await?;
    }
    Ok(())
}

struct ProjectedNotification {
    event_type: &'static str,
    state: &'static str,
    terminal: bool,
    lifecycle_cycle: u64,
    media: serde_json::Value,
    progress: Option<serde_json::Value>,
    stage: Option<&'static str>,
    next_step: Option<&'static str>,
    issue: Option<serde_json::Value>,
    result: Option<MediaNotificationResult>,
    actions: Vec<&'static str>,
}

#[derive(Clone)]
struct TaskArtifactProjection {
    season: Option<u32>,
    episode: Option<u32>,
    state: String,
    video: Option<MediaNotificationVideo>,
    audio: Option<MediaNotificationAudio>,
    subtitles: Option<MediaNotificationSubtitles>,
    file_size_bytes: Option<u64>,
    duration_seconds: Option<u64>,
    processing: Option<MediaNotificationProcessing>,
}

impl TaskArtifactProjection {
    fn has_measured_artifact(&self) -> bool {
        self.video.is_some()
            || self.audio.is_some()
            || self.subtitles.is_some()
            || self.file_size_bytes.is_some()
            || self.duration_seconds.is_some()
            || self.processing.is_some()
    }
}

async fn project_notification(
    transaction: &sea_orm::DatabaseTransaction,
    job: &Job,
    event: &JobEvent,
) -> Result<Option<ProjectedNotification>, sea_orm::DbErr> {
    if job.result_ref().starts_with("selection:session-refresh:") {
        return Ok(None);
    }
    let (event_type, state, terminal, stage, next_step, issue, actions) = match event.kind() {
        JobEventKind::Started => (
            "started",
            "queued",
            false,
            None,
            Some("download"),
            None,
            vec!["cancel", "details"],
        ),
        JobEventKind::StageStarted(stage) | JobEventKind::StageCheckpoint { stage, .. }
            if matches!(stage.name(), "download" | "torrent_monitor") =>
        {
            (
                "download-progress",
                "downloading",
                false,
                Some("download"),
                Some("process"),
                None,
                vec!["cancel", "details"],
            )
        }
        JobEventKind::StageStarted(stage) | JobEventKind::StageCheckpoint { stage, .. }
            if matches!(stage.name(), "encode" | "encoding" | "transcode") =>
        {
            (
                "transcoding-started",
                "processing",
                false,
                Some("process"),
                Some("publish"),
                None,
                vec!["cancel", "details"],
            )
        }
        JobEventKind::StageCompleted { stage, .. }
            if matches!(stage.name(), "download" | "torrent_monitor") =>
        {
            (
                "downloaded",
                "processing",
                false,
                Some("process"),
                Some("publish"),
                None,
                vec!["cancel", "details"],
            )
        }
        JobEventKind::StageCompleted { stage, .. }
            if matches!(stage.name(), "encode" | "encoding" | "transcode") =>
        {
            (
                "encoding-complete",
                "publishing",
                false,
                Some("publish"),
                Some("publish"),
                None,
                vec!["cancel", "details"],
            )
        }
        JobEventKind::JobTransition {
            state: JobState::Publishing,
            ..
        } => (
            "downloaded",
            "publishing",
            false,
            Some("publish"),
            Some("publish"),
            None,
            vec!["cancel", "details"],
        ),
        JobEventKind::JobTransition {
            state: JobState::PlexPending,
            ..
        } => (
            "plex-added",
            "publishing",
            false,
            Some("publish"),
            Some("publish"),
            Some(serde_json::json!({
                "code":"plex_publish_recovering",
                "message":"Plex publication is being recovered"
            })),
            vec!["retry", "details"],
        ),
        JobEventKind::JobTransition {
            state: JobState::Completed,
            ..
        } => (
            "completed",
            "completed",
            true,
            None,
            Some("none"),
            None,
            vec!["details"],
        ),
        JobEventKind::JobTransition {
            state: JobState::Partial,
            ..
        } => (
            "partial",
            "partial",
            true,
            None,
            Some("none"),
            None,
            vec!["details"],
        ),
        JobEventKind::JobTransition {
            state: JobState::BlockedStorage,
            ..
        } => (
            "blocked-storage",
            "needs-action",
            true,
            None,
            Some("download"),
            Some(serde_json::json!({"code":"storage_blocked","message":"storage is required"})),
            vec!["resume-storage", "details"],
        ),
        JobEventKind::JobTransition {
            state: JobState::NeedsAction,
            ..
        } => (
            "choice-needed",
            "needs-action",
            true,
            None,
            Some("none"),
            Some(
                serde_json::json!({"code":"needs_action","message":"media selection needs attention"}),
            ),
            vec!["details"],
        ),
        JobEventKind::StageFailed {
            retryable: true, ..
        } if job.state() == JobState::Queued => (
            "download-progress",
            "downloading",
            false,
            Some("download"),
            Some("download"),
            Some(serde_json::json!({
                "code":"source_recovering",
                "message":"source transfer is being recovered"
            })),
            vec!["cancel", "details"],
        ),
        JobEventKind::StageFailed { .. } if job.state() == JobState::Failed => (
            "failed",
            "failed",
            true,
            None,
            Some("none"),
            Some(serde_json::json!({"code":"media_failed","message":"media processing failed"})),
            vec!["retry", "search-alternative", "details"],
        ),
        JobEventKind::JobTransition {
            state: JobState::Failed,
            ..
        } => (
            "failed",
            "failed",
            true,
            None,
            Some("none"),
            Some(serde_json::json!({"code":"media_failed","message":"media processing failed"})),
            vec!["retry", "search-alternative", "details"],
        ),
        JobEventKind::JobTransition {
            state: JobState::Cancelled,
            ..
        } => (
            "cancelled",
            "cancelled",
            true,
            None,
            Some("none"),
            None,
            vec!["details"],
        ),
        _ => return Ok(None),
    };
    let payload = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT payload FROM search_executions WHERE result_ref = $1",
            [job.result_ref().into()],
        ))
        .await?
        .map(|row| row.try_get::<serde_json::Value>("", "payload"))
        .transpose()?
        .unwrap_or_default();
    let task_rows = transaction
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT ordinal, state FROM job_tasks WHERE job_id = $1 ORDER BY ordinal",
            [job.id().into_uuid().into()],
        ))
        .await?;
    let provider_selected = payload
        .get("episodes")
        .and_then(serde_json::Value::as_array)
        .map(|episodes| {
            episodes
                .iter()
                .filter_map(|episode| {
                    Some((
                        u32::try_from(episode.get("season")?.as_u64()?).ok()?,
                        u32::try_from(episode.get("episode")?.as_u64()?).ok()?,
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let episode_mappings = payload
        .get("episode_mappings")
        .and_then(serde_json::Value::as_array);
    let selected = provider_selected
        .iter()
        .map(|coordinates| {
            episode_mappings
                .and_then(|mappings| {
                    mappings.iter().find_map(|mapping| {
                        let provider = mapping.get("provider")?;
                        if json_episode_coordinates(provider)? != *coordinates {
                            return None;
                        }
                        let canonical = mapping.get("canonical")?;
                        json_episode_coordinates(canonical)
                    })
                })
                .unwrap_or(*coordinates)
        })
        .collect::<Vec<_>>();
    let canonical_titles = provider_selected
        .iter()
        .filter_map(|coordinates| {
            episode_mappings?.iter().find_map(|mapping| {
                let provider = mapping.get("provider")?;
                if json_episode_coordinates(provider)? != *coordinates {
                    return None;
                }
                mapping
                    .get("canonical_title")?
                    .as_str()
                    .map(safe_notification_field)
                    .filter(|value| !value.is_empty())
            })
        })
        .collect::<Vec<_>>();
    let canonical_title = if !provider_selected.is_empty()
        && canonical_titles.len() == provider_selected.len()
        && canonical_titles
            .iter()
            .all(|title| title == &canonical_titles[0])
    {
        canonical_titles.into_iter().next()
    } else {
        None
    };
    let title = canonical_title.unwrap_or_else(|| {
        payload
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(safe_notification_field)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Media job".to_owned())
    });
    let kind = if payload
        .get("media_kind")
        .and_then(serde_json::Value::as_str)
        == Some("movie")
    {
        "movie"
    } else {
        "series"
    };
    let season = selected
        .first()
        .map(|coordinates| coordinates.0)
        .filter(|season| selected.iter().all(|coordinates| coordinates.0 == *season))
        .or_else(|| {
            payload
                .get("season")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
        });
    let translation = payload
        .get("translation")
        .and_then(serde_json::Value::as_str)
        .map(safe_notification_field)
        .filter(|value| !value.is_empty());
    let cycle = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT notification_cycle FROM jobs WHERE id = $1",
            [job.id().into_uuid().into()],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("job disappeared".to_owned()))?
        .try_get::<i64>("", "notification_cycle")?;
    let current_ordinal = match event.kind() {
        JobEventKind::StageStarted(stage) | JobEventKind::StageCheckpoint { stage, .. } => {
            Some(stage.task_ordinal() as usize)
        }
        _ => None,
    };
    let mut completed = 0_u32;
    let mut missing = Vec::new();
    let mut task_state = std::collections::BTreeMap::new();
    for row in task_rows {
        task_state.insert(
            row.try_get::<i32>("", "ordinal")? as usize,
            row.try_get::<String>("", "state")?,
        );
    }
    for (ordinal, coordinates) in selected.iter().enumerate() {
        if task_state
            .get(&ordinal)
            .is_some_and(|value| value == "completed")
        {
            completed += 1;
        }
        if terminal
            && state == "partial"
            && task_state
                .get(&ordinal)
                .is_some_and(|value| value == "failed")
        {
            missing.push(serde_json::json!({"season": coordinates.0, "episode": coordinates.1}));
        }
    }
    let mut progress = if selected.is_empty() || task_state.is_empty() {
        None
    } else {
        Some(serde_json::json!({"completed_episodes": completed, "total_episodes": selected.len()}))
    };
    if let (Some(progress), Some(ordinal)) = (&mut progress, current_ordinal)
        && let Some((_, episode)) = selected.get(ordinal)
    {
        progress["current_episode"] = serde_json::json!(episode);
    }
    if selected.len() == 1 {
        progress.get_or_insert_with(|| serde_json::json!({}))["current_episode"] =
            serde_json::json!(selected[0].1);
    }
    if !missing.is_empty() {
        progress.get_or_insert_with(|| serde_json::json!({}))["missing_episodes"] =
            serde_json::Value::Array(missing.clone());
    }
    if let JobEventKind::StageCheckpoint { checkpoint, .. } = event.kind() {
        let transfer = progress.get_or_insert_with(|| serde_json::json!({}));
        if let Some(value) = checkpoint_unsigned(checkpoint, "downloaded_bytes") {
            transfer["downloaded_bytes"] = serde_json::json!(value);
        }
        if let Some(value) = checkpoint_unsigned(checkpoint, "download_speed_bps") {
            transfer["download_speed_bps"] = serde_json::json!(value);
        }
        if let Some(value) =
            checkpoint_unsigned(checkpoint, "progress_percent").filter(|value| *value <= 100)
        {
            transfer["percentage"] = serde_json::json!(value);
        }
    }
    if let JobEventKind::StageFailed {
        stage,
        retryable,
        error_code,
    } = event.kind()
        && let Some(attempt) = load_stage_attempt(transaction, job.id(), stage).await?
    {
        let limit = max_stage_attempts(job.provider());
        let recovery = progress.get_or_insert_with(|| serde_json::json!({}));
        recovery["connection_attempt"] = serde_json::json!(attempt);
        recovery["connection_attempt_limit"] = serde_json::json!(limit);
        recovery["vpn_rotation_pending"] = serde_json::json!(
            job.provider() == Provider::Rezka
                && *retryable
                && matches!(
                    error_code.as_str(),
                    "source_transfer_transient" | "stream_expired" | "execution_failed"
                )
                && attempt < limit
                && attempt % MAX_STICKY_VPN_ATTEMPTS == 0
        );
    }
    if state == "needs-action"
        && let Some((available, required)) = load_storage_projection(transaction, job.id()).await?
    {
        let storage = progress.get_or_insert_with(|| serde_json::json!({}));
        storage["storage_available_bytes"] = serde_json::json!(available);
        storage["storage_required_bytes"] = serde_json::json!(required);
    }
    let issue = if state == "partial" && missing.is_empty() {
        Some(
            serde_json::json!({"code":"subtitles_missing","message":"some subtitles are unavailable"}),
        )
    } else {
        issue
    };
    let actions = if state == "partial" && !missing.is_empty() {
        vec!["retry-missing", "search-alternative", "details"]
    } else {
        actions
    };
    let mut media = serde_json::json!({
        "job_id": job.id().to_string(), "title": title, "kind": kind,
        "provider": provider_value(job.provider()), "season": season, "translation": translation,
    });
    media
        .as_object_mut()
        .expect("media payload is an object")
        .retain(|_, value| !value.is_null());
    if job.result_ref().starts_with("selection:tracking:") {
        media["origin"] = serde_json::json!("tracked-episode");
    }
    let task_artifacts = load_task_artifacts(transaction, job.id(), &selected).await?;
    let result = aggregate_result(
        job.provider(),
        kind,
        &title,
        season,
        &selected,
        &task_artifacts,
        matches!(state, "publishing" | "completed" | "partial"),
    );
    Ok(Some(ProjectedNotification {
        event_type,
        state,
        terminal,
        lifecycle_cycle: u64::try_from(cycle)
            .map_err(|_| sea_orm::DbErr::Type("invalid notification cycle".to_owned()))?,
        media,
        progress,
        stage,
        next_step,
        issue,
        result,
        actions,
    }))
}

async fn insert_projected_notification(
    transaction: &sea_orm::DatabaseTransaction,
    job: &Job,
    recipient: &str,
    projected: &ProjectedNotification,
) -> Result<(), sea_orm::DbErr> {
    let card_key = format!("media-job:{}", job.id());
    let card_dedupe = [job.id().into_uuid().as_bytes().as_slice(), b"card"].concat();
    let previous = transaction.query_one_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT generation FROM notification_outbox WHERE source_dedupe_key = $1 AND recipient = $2 FOR UPDATE",
        [card_dedupe.clone().into(), recipient.into()],
    )).await?;
    let revision = previous
        .map(|row| row.try_get::<i64>("", "generation"))
        .transpose()?
        .unwrap_or(0)
        + 1;
    let payload = projected_payload(projected, "card", &card_key, revision as u64);
    transaction.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO notification_outbox (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload) \
         VALUES ($1, 'job', $2, $3, $4, $5, $6) ON CONFLICT (source_dedupe_key, recipient) DO UPDATE SET \
         event_type = EXCLUDED.event_type, payload = EXCLUDED.payload, generation = notification_outbox.generation + 1, \
         delivered_at = NULL, dead_at = NULL, next_attempt_at = now(), attempt_count = 0, last_error_code = NULL",
        [Uuid::new_v4().into(), job.id().into_uuid().into(), projected.event_type.into(), recipient.into(), card_dedupe.into(), payload.into()],
    )).await?;
    if projected.terminal {
        let dedupe = format!(
            "final-push:{}:{}:{}",
            job.id(),
            projected.lifecycle_cycle,
            projected.state
        )
        .into_bytes();
        let payload = projected_payload(projected, "final-push", &card_key, revision as u64);
        transaction.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO notification_outbox (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload) \
             VALUES ($1, 'job', $2, $3, $4, $5, $6) ON CONFLICT (source_dedupe_key, recipient) DO NOTHING",
            [Uuid::new_v4().into(), job.id().into_uuid().into(), projected.event_type.into(), recipient.into(), dedupe.into(), payload.into()],
        )).await?;
    }
    Ok(())
}

fn projected_payload(
    projected: &ProjectedNotification,
    delivery_kind: &str,
    card_key: &str,
    revision: u64,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "event_type": "media.notification", "schema_version": 2, "delivery_kind": delivery_kind,
        "card_key": card_key, "revision": revision, "lifecycle_cycle": projected.lifecycle_cycle,
        "terminal": projected.terminal, "state": projected.state, "media": projected.media, "actions": projected.actions,
    });
    if let Some(progress) = &projected.progress {
        value["progress"] = progress.clone();
    }
    if let Some(stage) = projected.stage {
        value["stage"] = serde_json::json!(stage);
    }
    if let Some(next_step) = projected.next_step {
        value["next_step"] = serde_json::json!(next_step);
    }
    if let Some(issue) = &projected.issue {
        value["issue"] = issue.clone();
    }
    if let Some(result) = &projected.result {
        value["result"] = result_payload(result);
    }
    value
}

async fn load_task_artifacts(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    selected: &[(u32, u32)],
) -> Result<Vec<TaskArtifactProjection>, sea_orm::DbErr> {
    transaction
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT t.ordinal, t.state, COALESCE(s.checkpoint, '{}'::jsonb) AS checkpoint \
             FROM job_tasks t LEFT JOIN job_stages s \
             ON s.task_id = t.id AND s.name = 'media_pipeline' \
             WHERE t.job_id = $1 ORDER BY t.ordinal",
            [job_id.into_uuid().into()],
        ))
        .await?
        .into_iter()
        .map(|row| {
            let ordinal = usize::try_from(row.try_get::<i32>("", "ordinal")?)
                .map_err(|_| sea_orm::DbErr::Type("invalid task ordinal".to_owned()))?;
            let checkpoint = row.try_get::<serde_json::Value>("", "checkpoint")?;
            Ok(task_artifact_projection(
                selected.get(ordinal).copied(),
                row.try_get("", "state")?,
                &checkpoint,
            ))
        })
        .collect()
}

fn task_artifact_projection(
    coordinates: Option<(u32, u32)>,
    state: String,
    checkpoint: &serde_json::Value,
) -> TaskArtifactProjection {
    let object = checkpoint.as_object();
    let unsigned = |name: &str| object?.get(name)?.as_u64();
    let text = |name: &str| bounded_artifact_text(object?.get(name)?.as_str()?);
    let video = (|| {
        MediaNotificationVideo::new(
            text("artifact_video_codec")?,
            text("artifact_video_profile"),
            u32::try_from(unsigned("artifact_width")?).ok()?,
            u32::try_from(unsigned("artifact_height")?).ok()?,
        )
        .ok()
    })();
    let audio = (|| {
        MediaNotificationAudio::new(
            text("artifact_audio_language"),
            text("artifact_audio_codec")?,
            unsigned("artifact_audio_channels").and_then(|value| u32::try_from(value).ok()),
            text("artifact_audio_channel_layout"),
            text("artifact_audio_title"),
        )
        .ok()
    })();
    let subtitles = match (
        unsigned("artifact_subtitles_downloaded").and_then(|value| u32::try_from(value).ok()),
        unsigned("artifact_subtitles_missing").and_then(|value| u32::try_from(value).ok()),
    ) {
        (Some(downloaded), Some(missing)) => {
            Some(MediaNotificationSubtitles::new(downloaded, missing))
        }
        _ => None,
    };
    let processing = match text("artifact_processing_mode").as_deref() {
        Some("vaapi-upscale") => Some(MediaNotificationProcessing::new(
            MediaNotificationProcessingMode::VaapiUpscale,
            unsigned("artifact_processing_seconds"),
        )),
        _ => None,
    };
    TaskArtifactProjection {
        season: coordinates.map(|value| value.0),
        episode: coordinates.map(|value| value.1),
        state,
        video,
        audio,
        subtitles,
        file_size_bytes: unsigned("artifact_file_size_bytes"),
        duration_seconds: unsigned("artifact_duration_seconds"),
        processing,
    }
}

fn aggregate_result(
    provider: Provider,
    kind: &str,
    title: &str,
    season: Option<u32>,
    selected: &[(u32, u32)],
    tasks: &[TaskArtifactProjection],
    publication_reached: bool,
) -> Option<MediaNotificationResult> {
    let artifacts = tasks
        .iter()
        .filter(|task| task.state == "completed" || task.has_measured_artifact())
        .collect::<Vec<_>>();
    if artifacts.is_empty() && !(provider == Provider::Prowlarr && publication_reached) {
        return None;
    }
    let video = equal_complete_value(&artifacts, |task| task.video.clone());
    let audio = equal_complete_value(&artifacts, |task| task.audio.clone());
    let subtitles = sum_subtitles(&artifacts);
    let file_size_bytes = sum_complete(&artifacts, |task| task.file_size_bytes);
    let duration_seconds = sum_complete(&artifacts, |task| task.duration_seconds);
    let processing = if provider == Provider::Prowlarr {
        Some(MediaNotificationProcessing::new(
            MediaNotificationProcessingMode::Original,
            None,
        ))
    } else {
        aggregate_processing(&artifacts)
    };
    let exact_coordinates = if selected.len() == 1 {
        Some(selected[0])
    } else if artifacts.len() == 1 {
        artifacts[0].season.zip(artifacts[0].episode)
    } else {
        None
    };
    let publication = if publication_reached {
        MediaNotificationPublication::new(
            if kind == "movie" {
                MediaNotificationLibrary::Movies
            } else {
                MediaNotificationLibrary::TvShows
            },
            title.to_owned(),
            if kind == "movie" {
                None
            } else {
                exact_coordinates.map(|value| value.0).or(season)
            },
            exact_coordinates.map(|value| value.1),
        )
        .ok()
    } else {
        None
    };
    Some(MediaNotificationResult::new(
        video,
        audio,
        subtitles,
        file_size_bytes,
        duration_seconds,
        processing,
        publication,
    ))
}

fn equal_complete_value<T: Clone + Eq>(
    tasks: &[&TaskArtifactProjection],
    value: impl Fn(&TaskArtifactProjection) -> Option<T>,
) -> Option<T> {
    let first = value(tasks.first()?)?;
    tasks
        .iter()
        .all(|task| value(task).as_ref() == Some(&first))
        .then_some(first)
}

fn sum_complete(
    tasks: &[&TaskArtifactProjection],
    value: impl Fn(&TaskArtifactProjection) -> Option<u64>,
) -> Option<u64> {
    tasks
        .iter()
        .try_fold(0_u64, |total, task| total.checked_add(value(task)?))
}

fn sum_subtitles(tasks: &[&TaskArtifactProjection]) -> Option<MediaNotificationSubtitles> {
    let (downloaded, missing) =
        tasks
            .iter()
            .try_fold((0_u32, 0_u32), |(downloaded, missing), task| {
                let subtitles = task.subtitles.as_ref()?;
                Some((
                    downloaded.checked_add(subtitles.downloaded())?,
                    missing.checked_add(subtitles.missing())?,
                ))
            })?;
    Some(MediaNotificationSubtitles::new(downloaded, missing))
}

fn aggregate_processing(tasks: &[&TaskArtifactProjection]) -> Option<MediaNotificationProcessing> {
    let first = tasks.first()?.processing.as_ref()?;
    if !tasks.iter().all(|task| {
        task.processing
            .as_ref()
            .map(MediaNotificationProcessing::mode)
            == Some(first.mode())
    }) {
        return None;
    }
    let elapsed_seconds = sum_complete(tasks, |task| task.processing.as_ref()?.elapsed_seconds());
    Some(MediaNotificationProcessing::new(
        first.mode(),
        elapsed_seconds,
    ))
}

async fn load_stage_attempt(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    stage: &StageRef,
) -> Result<Option<u32>, sea_orm::DbErr> {
    transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT s.attempt_count FROM job_stages s JOIN job_tasks t ON t.id = s.task_id \
             WHERE t.job_id = $1 AND t.ordinal = $2 AND s.name = $3 AND s.ordinal = $4",
            [
                job_id.into_uuid().into(),
                i32::try_from(stage.task_ordinal())
                    .map_err(|_| sea_orm::DbErr::Type("invalid task ordinal".to_owned()))?
                    .into(),
                stage.name().into(),
                i32::try_from(stage.ordinal())
                    .map_err(|_| sea_orm::DbErr::Type("invalid stage ordinal".to_owned()))?
                    .into(),
            ],
        ))
        .await?
        .map(|row| {
            u32::try_from(row.try_get::<i32>("", "attempt_count")?)
                .map_err(|_| sea_orm::DbErr::Type("invalid stage attempt count".to_owned()))
        })
        .transpose()
}

async fn load_storage_projection(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
) -> Result<Option<(u64, u64)>, sea_orm::DbErr> {
    let checkpoint = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT s.checkpoint FROM job_stages s JOIN job_tasks t ON t.id = s.task_id \
             WHERE t.job_id = $1 AND s.name = 'media_pipeline' \
             AND s.checkpoint ? 'storage_available_bytes' \
             AND s.checkpoint ? 'storage_required_bytes' \
             ORDER BY t.ordinal DESC LIMIT 1",
            [job_id.into_uuid().into()],
        ))
        .await?
        .map(|row| row.try_get::<serde_json::Value>("", "checkpoint"))
        .transpose()?;
    Ok(checkpoint.and_then(|checkpoint| {
        Some((
            checkpoint.get("storage_available_bytes")?.as_u64()?,
            checkpoint.get("storage_required_bytes")?.as_u64()?,
        ))
    }))
}

fn bounded_artifact_text(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 160
        && !value.contains("://")
        && !value.chars().any(char::is_control))
    .then(|| value.to_owned())
}

fn result_payload(result: &MediaNotificationResult) -> serde_json::Value {
    let mut value = serde_json::json!({});
    if let Some(video) = result.video() {
        value["video"] = serde_json::json!({
            "codec": video.codec(),
            "profile": video.profile(),
            "width": video.width(),
            "height": video.height(),
        });
        remove_null_fields(&mut value["video"]);
    }
    if let Some(audio) = result.audio() {
        value["audio"] = serde_json::json!({
            "language": audio.language(),
            "codec": audio.codec(),
            "channels": audio.channels(),
            "channel_layout": audio.channel_layout(),
            "title": audio.title(),
        });
        remove_null_fields(&mut value["audio"]);
    }
    if let Some(subtitles) = result.subtitles() {
        value["subtitles"] = serde_json::json!({
            "downloaded": subtitles.downloaded(),
            "missing": subtitles.missing(),
        });
    }
    if let Some(file_size_bytes) = result.file_size_bytes() {
        value["file_size_bytes"] = serde_json::json!(file_size_bytes);
    }
    if let Some(duration_seconds) = result.duration_seconds() {
        value["duration_seconds"] = serde_json::json!(duration_seconds);
    }
    if let Some(processing) = result.processing() {
        value["processing"] = serde_json::json!({
            "mode": match processing.mode() {
                MediaNotificationProcessingMode::VaapiUpscale => "vaapi-upscale",
                MediaNotificationProcessingMode::Original => "original",
            },
            "elapsed_seconds": processing.elapsed_seconds(),
        });
        remove_null_fields(&mut value["processing"]);
    }
    if let Some(publication) = result.publication() {
        value["publication"] = serde_json::json!({
            "library": match publication.library() {
                MediaNotificationLibrary::Movies => "movies",
                MediaNotificationLibrary::TvShows => "tv-shows",
            },
            "title": publication.title(),
            "season": publication.season(),
            "episode": publication.episode(),
        });
        remove_null_fields(&mut value["publication"]);
    }
    value
}

fn remove_null_fields(value: &mut serde_json::Value) {
    value
        .as_object_mut()
        .expect("notification result section is an object")
        .retain(|_, value| !value.is_null());
}

fn provider_value(provider: Provider) -> &'static str {
    match provider {
        Provider::Rezka => "rezka",
        Provider::Prowlarr => "prowlarr",
    }
}

fn checkpoint_unsigned(checkpoint: &Checkpoint, key: &str) -> Option<u64> {
    match checkpoint.get(key) {
        Some(CheckpointValue::Unsigned(value)) => Some(*value),
        _ => None,
    }
}

fn json_episode_coordinates(value: &serde_json::Value) -> Option<(u32, u32)> {
    Some((
        u32::try_from(value.get("season")?.as_u64()?).ok()?,
        u32::try_from(value.get("episode")?.as_u64()?).ok()?,
    ))
}

fn safe_notification_field(value: &str) -> String {
    let normalized = value.replace("://", " / ");
    normalized
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn replayed_lease(result: OperationResult) -> Result<Option<JobLease>, sea_orm::DbErr> {
    match result {
        OperationResult::Lease(lease) => Ok(Some(lease)),
        OperationResult::None => Ok(None),
        OperationResult::Job(_) => Err(sea_orm::DbErr::Type(
            "lease operation has an invalid result".to_owned(),
        )),
    }
}

async fn apply_event(
    transaction: &sea_orm::DatabaseTransaction,
    lease: LeaseId,
    current: Job,
    event: &JobEvent,
) -> Result<Job, sea_orm::DbErr> {
    match event.kind() {
        JobEventKind::Started => {
            transition_job(transaction, &current, JobState::Running, None).await?;
        }
        JobEventKind::StageStarted(stage) => {
            require_running(&current)?;
            start_stage(transaction, current.id(), current.provider(), stage).await?;
        }
        JobEventKind::StageCheckpoint { stage, checkpoint } => {
            require_running(&current)?;
            update_stage_checkpoint(transaction, current.id(), stage, checkpoint, false).await?;
        }
        JobEventKind::StageCompleted { stage, checkpoint } => {
            require_running(&current)?;
            update_stage_checkpoint(transaction, current.id(), stage, checkpoint, true).await?;
        }
        JobEventKind::StageFailed {
            stage,
            retryable,
            error_code,
        } => {
            require_running(&current)?;
            let terminal = fail_stage(
                transaction,
                current.id(),
                current.provider(),
                stage,
                *retryable,
                error_code,
            )
            .await?;
            if terminal {
                transition_job(transaction, &current, JobState::Failed, None).await?;
                release_lease(transaction, lease).await?;
            } else {
                reset_running_work(transaction, current.id()).await?;
                transition_job(transaction, &current, JobState::Queued, None).await?;
                release_lease(transaction, lease).await?;
            }
        }
        JobEventKind::JobTransition {
            state,
            needs_action_reason,
        } => {
            transition_job(transaction, &current, *state, *needs_action_reason).await?;
            if matches!(
                state,
                JobState::BlockedStorage
                    | JobState::NeedsAction
                    | JobState::Partial
                    | JobState::Completed
                    | JobState::Failed
                    | JobState::Cancelled
            ) {
                finish_tasks(transaction, current.id(), *state).await?;
                release_lease(transaction, lease).await?;
            }
        }
    }
    load_job(transaction, current.id().into_uuid()).await
}

async fn reset_running_work(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
) -> Result<(), sea_orm::DbErr> {
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job_stages SET state = 'pending', updated_at = now() \
             WHERE task_id IN (SELECT id FROM job_tasks WHERE job_id = $1) \
             AND state = 'running'",
            [job_id.into_uuid().into()],
        ))
        .await?;
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job_tasks SET state = 'pending', updated_at = now() \
             WHERE job_id = $1 AND state = 'running'",
            [job_id.into_uuid().into()],
        ))
        .await?;
    Ok(())
}

fn require_running(job: &Job) -> Result<(), sea_orm::DbErr> {
    if job.state() == JobState::Running {
        Ok(())
    } else {
        Err(sea_orm::DbErr::Custom(
            "stage event requires a running job".to_owned(),
        ))
    }
}

async fn transition_job(
    transaction: &sea_orm::DatabaseTransaction,
    current: &Job,
    target: JobState,
    reason: Option<media_core::NeedsActionReason>,
) -> Result<(), sea_orm::DbErr> {
    current
        .state()
        .transition(target)
        .map_err(|_| sea_orm::DbErr::Custom("invalid job transition".to_owned()))?;
    let updated = transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE jobs SET state = $2, needs_action_reason = $3, updated_at = now(), \
             started_at = CASE WHEN $2 = 'running' THEN COALESCE(started_at, now()) ELSE started_at END, \
             completed_at = CASE WHEN $2 IN ('partial', 'completed', 'failed', 'cancelled') \
             THEN now() ELSE completed_at END WHERE id = $1 AND state = $4",
            [
                current.id().into_uuid().into(),
                job_state_value(target).into(),
                reason.map(needs_action_reason_value).into(),
                job_state_value(current.state()).into(),
            ],
        ))
        .await?;
    if updated.rows_affected() != 1 {
        return Err(sea_orm::DbErr::RecordNotUpdated);
    }
    Ok(())
}

async fn ensure_task(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    ordinal: u32,
) -> Result<Uuid, sea_orm::DbErr> {
    let ordinal = i32::try_from(ordinal)
        .map_err(|_| sea_orm::DbErr::Type("task ordinal is out of range".to_owned()))?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO job_tasks (id, job_id, ordinal, state, started_at) \
             VALUES ($1, $2, $3, 'running', now()) ON CONFLICT (job_id, ordinal) \
             DO UPDATE SET state = 'running', started_at = COALESCE(job_tasks.started_at, now()), \
             updated_at = now() WHERE job_tasks.state IN ('pending', 'running') RETURNING id",
            [
                Uuid::new_v4().into(),
                job_id.into_uuid().into(),
                ordinal.into(),
            ],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("task is not resumable".to_owned()))?;
    row.try_get("", "id")
}

async fn find_task(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    ordinal: u32,
) -> Result<Uuid, sea_orm::DbErr> {
    let ordinal = i32::try_from(ordinal)
        .map_err(|_| sea_orm::DbErr::Type("task ordinal is out of range".to_owned()))?;
    transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM job_tasks WHERE job_id = $1 AND ordinal = $2",
            [job_id.into_uuid().into(), ordinal.into()],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("job task not found".to_owned()))?
        .try_get("", "id")
}

async fn start_stage(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    provider: Provider,
    stage: &StageRef,
) -> Result<(), sea_orm::DbErr> {
    let task_id = ensure_task(transaction, job_id, stage.task_ordinal()).await?;
    let ordinal = i32::try_from(stage.ordinal())
        .map_err(|_| sea_orm::DbErr::Type("stage ordinal is out of range".to_owned()))?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO job_stages \
             (id, task_id, name, ordinal, state, attempt_count, started_at) \
             VALUES ($1, $2, $3, $4, 'running', 1, now()) \
             ON CONFLICT (task_id, name) DO UPDATE SET state = 'running', \
             attempt_count = CASE WHEN job_stages.state = 'completed' \
                 THEN job_stages.attempt_count ELSE job_stages.attempt_count + 1 END, \
             completed_at = NULL, \
             started_at = COALESCE(job_stages.started_at, now()), updated_at = now() \
             WHERE job_stages.state IN ('pending', 'completed') \
             AND (job_stages.state = 'completed' OR job_stages.attempt_count < $5) \
             AND job_stages.ordinal = EXCLUDED.ordinal RETURNING attempt_count",
            [
                Uuid::new_v4().into(),
                task_id.into(),
                stage.name().into(),
                ordinal.into(),
                i32::try_from(max_stage_attempts(provider))
                    .map_err(|_| {
                        sea_orm::DbErr::Type("stage attempt limit is out of range".to_owned())
                    })?
                    .into(),
            ],
        ))
        .await?;
    if row.is_none() {
        return Err(sea_orm::DbErr::Custom(
            "stage cannot be started or retried".to_owned(),
        ));
    }
    Ok(())
}

async fn update_stage_checkpoint(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    stage: &StageRef,
    checkpoint: &Checkpoint,
    complete: bool,
) -> Result<(), sea_orm::DbErr> {
    let task_id = find_task(transaction, job_id, stage.task_ordinal()).await?;
    let ordinal = i32::try_from(stage.ordinal())
        .map_err(|_| sea_orm::DbErr::Type("stage ordinal is out of range".to_owned()))?;
    let state = if complete { "completed" } else { "running" };
    let completed_at = if complete { "now()" } else { "completed_at" };
    let allowed_states = if complete {
        "('running', 'completed')"
    } else {
        "('running')"
    };
    let sql = format!(
        "UPDATE job_stages SET checkpoint = checkpoint || $4, state = '{state}', \
         completed_at = {completed_at}, updated_at = now() WHERE task_id = $1 \
         AND name = $2 AND ordinal = $3 AND state IN {allowed_states}"
    );
    let updated = transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [
                task_id.into(),
                stage.name().into(),
                ordinal.into(),
                checkpoint_json(checkpoint).into(),
            ],
        ))
        .await?;
    if updated.rows_affected() != 1 {
        return Err(sea_orm::DbErr::RecordNotUpdated);
    }
    let task_completed = complete
        && (stage.name() == "plex_reconcile"
            || (stage.name() == "media_pipeline"
                && checkpoint.contains_key("artifact_file_size_bytes")));
    if task_completed {
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE job_tasks SET state = 'completed', completed_at = now(), \
                 updated_at = now(), error_snapshot = NULL WHERE id = $1 \
                 AND state IN ('pending', 'running', 'completed')",
                [task_id.into()],
            ))
            .await?;
    }
    Ok(())
}

async fn fail_stage(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    provider: Provider,
    stage: &StageRef,
    retryable: bool,
    error_code: &str,
) -> Result<bool, sea_orm::DbErr> {
    let task_id = find_task(transaction, job_id, stage.task_ordinal()).await?;
    let ordinal = i32::try_from(stage.ordinal())
        .map_err(|_| sea_orm::DbErr::Type("stage ordinal is out of range".to_owned()))?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT attempt_count FROM job_stages WHERE task_id = $1 AND name = $2 \
             AND ordinal = $3 AND state = 'running' FOR UPDATE",
            [task_id.into(), stage.name().into(), ordinal.into()],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("running stage not found".to_owned()))?;
    let attempt = u32::try_from(row.try_get::<i32>("", "attempt_count")?)
        .map_err(|_| sea_orm::DbErr::Type("invalid stage attempt count".to_owned()))?;
    let terminal = StageFailureOutcome::for_attempt_with_limit(
        attempt,
        retryable,
        max_stage_attempts(provider),
    ) == StageFailureOutcome::Failed;
    let state = if terminal { "failed" } else { "pending" };
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job_stages SET state = $4, error_snapshot = $5, updated_at = now(), \
             completed_at = CASE WHEN $4 = 'failed' THEN now() ELSE NULL END \
             WHERE task_id = $1 AND name = $2 AND ordinal = $3 AND state = 'running'",
            [
                task_id.into(),
                stage.name().into(),
                ordinal.into(),
                state.into(),
                serde_json::json!({"code": error_code, "retryable": retryable}).into(),
            ],
        ))
        .await?;
    if terminal {
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE job_tasks SET state = 'failed', completed_at = now(), updated_at = now(), \
                 error_snapshot = $2 WHERE id = $1",
                [
                    task_id.into(),
                    serde_json::json!({"code": error_code, "retryable": retryable}).into(),
                ],
            ))
            .await?;
    }
    Ok(terminal)
}

async fn finish_tasks(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
    state: JobState,
) -> Result<(), sea_orm::DbErr> {
    let target = match state {
        JobState::Completed => "completed",
        JobState::Cancelled => "cancelled",
        JobState::Failed => "failed",
        JobState::Partial | JobState::NeedsAction => return Ok(()),
        _ => return Ok(()),
    };
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job_tasks SET state = $2, completed_at = now(), updated_at = now() \
             WHERE job_id = $1 AND state IN ('pending', 'running')",
            [job_id.into_uuid().into(), target.into()],
        ))
        .await?;
    Ok(())
}

async fn release_lease(
    transaction: &sea_orm::DatabaseTransaction,
    lease: LeaseId,
) -> Result<(), sea_orm::DbErr> {
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM job_leases WHERE id = $1",
            [lease.into_uuid().into()],
        ))
        .await?;
    Ok(())
}

fn checkpoint_json(checkpoint: &Checkpoint) -> serde_json::Value {
    serde_json::Value::Object(
        checkpoint
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    CheckpointValue::String(value) => serde_json::Value::String(value.clone()),
                    CheckpointValue::Unsigned(value) => (*value).into(),
                    CheckpointValue::Bool(value) => (*value).into(),
                };
                (key.clone(), value)
            })
            .collect(),
    )
}

fn event_payload(event: &JobEvent) -> (&'static str, serde_json::Value) {
    match event.kind() {
        JobEventKind::Started => ("job.started", serde_json::json!({})),
        JobEventKind::StageStarted(stage) => ("stage.started", stage_payload(stage)),
        JobEventKind::StageCheckpoint { stage, checkpoint } => (
            "stage.checkpointed",
            with_checkpoint(stage_payload(stage), checkpoint),
        ),
        JobEventKind::StageCompleted { stage, checkpoint } => (
            "stage.completed",
            with_checkpoint(stage_payload(stage), checkpoint),
        ),
        JobEventKind::StageFailed {
            stage,
            retryable,
            error_code,
        } => {
            let mut payload = stage_payload(stage);
            let object = payload.as_object_mut().expect("stage payload is an object");
            object.insert("retryable".to_owned(), (*retryable).into());
            object.insert(
                "error_code".to_owned(),
                serde_json::Value::String(error_code.clone()),
            );
            ("stage.failed", payload)
        }
        JobEventKind::JobTransition {
            state,
            needs_action_reason,
        } => (
            "job.transitioned",
            serde_json::json!({
                "state": job_state_value(*state),
                "needs_action_reason": needs_action_reason.map(needs_action_reason_value),
            }),
        ),
    }
}

fn stage_payload(stage: &StageRef) -> serde_json::Value {
    serde_json::json!({
        "task_ordinal": stage.task_ordinal(),
        "stage_name": stage.name(),
        "stage_ordinal": stage.ordinal(),
    })
}

fn with_checkpoint(mut payload: serde_json::Value, checkpoint: &Checkpoint) -> serde_json::Value {
    payload
        .as_object_mut()
        .expect("stage payload is an object")
        .insert("checkpoint".to_owned(), checkpoint_json(checkpoint));
    payload
}

async fn lease_next_in_transaction(
    transaction: &sea_orm::DatabaseTransaction,
    runner: ClientId,
    ttl_seconds: i64,
) -> Result<LeaseDecision, sea_orm::DbErr> {
    transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [LEASE_ADVISORY_LOCK.into()],
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("advisory lock query failed".to_owned()))?;

    let lifecycle = transaction
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT state, sticky_job_id, sticky_attempt_count FROM runner_lifecycle \
             WHERE singleton = true FOR UPDATE"
                .to_owned(),
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("runner lifecycle is missing".to_owned()))?;
    match lifecycle.try_get::<String>("", "state")?.as_str() {
        "ready" => {}
        "rotating" => return Ok(LeaseDecision::RotationRequired),
        _ => return Ok(LeaseDecision::Available(None)),
    }

    if let Some(existing) = transaction
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT job_leases.id, job_leases.job_id, expires_at > now() AS active, \
             jobs.state AS job_state FROM job_leases JOIN jobs \
             ON jobs.id = job_leases.job_id WHERE slot = 1 \
             FOR UPDATE OF job_leases, jobs",
        ))
        .await?
    {
        if existing.try_get::<bool>("", "active")? {
            return Ok(LeaseDecision::Available(None));
        }
        let expired_lease = existing.try_get::<Uuid>("", "id")?;
        let expired_job = existing.try_get::<Uuid>("", "job_id")?;
        let cancelled = existing.try_get::<String>("", "job_state")? == "cancel_requested";
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE job_stages SET state = 'pending', updated_at = now() \
                 WHERE state = 'running' AND task_id IN \
                 (SELECT id FROM job_tasks WHERE job_id = $1)",
                [expired_job.into()],
            ))
            .await?;
        if cancelled {
            let job = load_job(transaction, expired_job).await?;
            let event = JobEvent::transition(JobEventId::new(), JobState::Cancelled, None)
                .map_err(|_| sea_orm::DbErr::Type("invalid cancellation event".to_owned()))?;
            insert_outbox(
                transaction,
                job.id(),
                "job.cancelled",
                expired_lease.as_bytes().to_vec(),
                serde_json::json!({"state": "cancelled"}),
            )
            .await?;
            insert_notification_outbox(transaction, &job, &event).await?;
        }
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE job_tasks SET state = 'pending', updated_at = now() \
                 WHERE job_id = $1 AND state = 'running'",
                [expired_job.into()],
            ))
            .await?;
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE jobs SET state = CASE \
                 WHEN state = 'cancel_requested' THEN 'cancelled' ELSE 'queued' END, \
                 completed_at = CASE WHEN state = 'cancel_requested' THEN now() \
                 ELSE completed_at END, updated_at = now() WHERE id = $1 \
                 AND state IN ('leased', 'running', 'publishing', \
                 'plex_pending', 'cancel_requested')",
                [expired_job.into()],
            ))
            .await?;
        transaction
            .execute_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "DELETE FROM job_leases WHERE slot = 1",
            ))
            .await?;
    }

    let Some(candidate) = transaction
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT id FROM jobs WHERE state = 'queued' ORDER BY created_at, id \
             FOR UPDATE SKIP LOCKED LIMIT 1"
                .to_owned(),
        ))
        .await?
    else {
        let queued = transaction
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM jobs WHERE state = 'queued') AS queued",
            ))
            .await?
            .ok_or_else(|| sea_orm::DbErr::Custom("queued job check failed".to_owned()))?;
        if queued.try_get::<bool>("", "queued")? {
            return Err(sea_orm::DbErr::RecordNotUpdated);
        }
        return Ok(LeaseDecision::Available(None));
    };
    let job_id = candidate.try_get::<Uuid>("", "id")?;
    let sticky_job_id = lifecycle.try_get::<Option<Uuid>>("", "sticky_job_id")?;
    let sticky_attempt_count = u32::try_from(lifecycle.try_get::<i32>("", "sticky_attempt_count")?)
        .map_err(|_| sea_orm::DbErr::Type("invalid sticky VPN attempt count".to_owned()))?;
    if sticky_job_id
        .is_some_and(|sticky| sticky != job_id || sticky_attempt_count >= MAX_STICKY_VPN_ATTEMPTS)
    {
        transaction
            .execute_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "UPDATE runner_lifecycle SET state = 'rotating', reason = NULL, \
                 previous_ip = current_ip, updated_at = now() WHERE singleton = true"
                    .to_owned(),
            ))
            .await?;
        return Ok(LeaseDecision::RotationRequired);
    }
    let leased = transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE jobs SET state = 'leased', attempt_count = attempt_count + 1, \
             updated_at = now() WHERE id = $1 AND state = 'queued'",
            [job_id.into()],
        ))
        .await?;
    if leased.rows_affected() != 1 {
        return Err(sea_orm::DbErr::RecordNotUpdated);
    }
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE runner_lifecycle SET sticky_job_id = $1, \
             sticky_attempt_count = CASE WHEN sticky_job_id = $1 \
             THEN sticky_attempt_count + 1 ELSE 1 END, updated_at = now() \
             WHERE singleton = true",
            [job_id.into()],
        ))
        .await?;
    let lease_id = Uuid::new_v4();
    let inserted = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO job_leases \
             (id, slot, job_id, runner_client_id, expires_at) \
             VALUES ($1, 1, $2, $3, now() + ($4::double precision * interval '1 second')) \
             ON CONFLICT DO NOTHING RETURNING expires_at",
            [
                lease_id.into(),
                job_id.into(),
                runner.into_uuid().into(),
                ttl_seconds.into(),
            ],
        ))
        .await?;
    let Some(inserted) = inserted else {
        let active = transaction
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM job_leases \
                 WHERE slot = 1 AND expires_at > now()) AS active",
            ))
            .await?
            .ok_or_else(|| sea_orm::DbErr::Custom("lease race check failed".to_owned()))?;
        if active.try_get::<bool>("", "active")? {
            return Ok(LeaseDecision::Available(None));
        }
        return Err(sea_orm::DbErr::Custom(
            "lease slot conflicted without an active lease".to_owned(),
        ));
    };
    let expires_at = inserted.try_get("", "expires_at")?;
    let job = load_job(transaction, job_id).await?;
    debug_assert_eq!(job.state(), JobState::Leased);
    Ok(LeaseDecision::Available(Some(JobLease::new(
        LeaseId::from_uuid(lease_id),
        job,
        runner,
        expires_at,
    ))))
}

async fn heartbeat_in_transaction(
    transaction: &sea_orm::DatabaseTransaction,
    lease: LeaseId,
    runner: ClientId,
    ttl_seconds: i64,
) -> Result<Option<JobLease>, sea_orm::DbErr> {
    let Some(updated) = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job_leases SET \
             expires_at = now() + ($3::double precision * interval '1 second'), \
             updated_at = now() WHERE id = $1 AND runner_client_id = $2 \
             AND expires_at > now() RETURNING job_id, expires_at",
            [
                lease.into_uuid().into(),
                runner.into_uuid().into(),
                ttl_seconds.into(),
            ],
        ))
        .await?
    else {
        return Ok(None);
    };
    let job = load_job(transaction, updated.try_get("", "job_id")?).await?;
    Ok(Some(JobLease::new(
        lease,
        job,
        runner,
        updated.try_get("", "expires_at")?,
    )))
}

fn valid_ttl_seconds(ttl: time::Duration) -> Result<i64, PortError> {
    if ttl < time::Duration::seconds(30) || ttl > time::Duration::seconds(300) {
        Err(PortError::Conflict)
    } else {
        Ok(ttl.whole_seconds())
    }
}

async fn load_job(
    transaction: &sea_orm::DatabaseTransaction,
    id: Uuid,
) -> Result<Job, sea_orm::DbErr> {
    job::Entity::find_by_id(id)
        .one(transaction)
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("leased job disappeared".to_owned()))?
        .try_into()
        .map_err(|error| sea_orm::DbErr::Type(format!("invalid persisted job: {error:?}")))
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
