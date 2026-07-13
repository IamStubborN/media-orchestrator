use media_core::{
    PRIMARY_USER_ID, Checkpoint, CheckpointValue, ClientId, Job, JobEvent, JobEventKind, JobLease,
    JobState, LeaseId, LeaseStore, NotificationEventType, NotifyScope, OperationKey, PortError,
    StageFailureOutcome, StageRef, SECONDARY_USER_ID,
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
                OperationClaim::Replay(result) => return replayed_lease(result),
                OperationClaim::Fresh => {}
            }
            let lease = lease_next_in_transaction(&transaction, runner, ttl_seconds).await?;
            let stored = lease
                .clone()
                .map_or(OperationResult::None, OperationResult::Lease);
            operation::complete(&transaction, operation, OperationKind::LeaseNext, &stored).await?;
            Ok(lease)
        }
        .await;
        finish(transaction, result).await
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

async fn insert_notification_outbox(
    transaction: &sea_orm::DatabaseTransaction,
    job: &Job,
    event: &JobEvent,
) -> Result<(), sea_orm::DbErr> {
    let context = notification_context(transaction, job).await?;
    let notifications = notifications_for_event(job, event, &context);
    if notifications.is_empty() {
        return Ok(());
    }
    let initiator = match job.owner_id() {
        owner if owner == PRIMARY_USER_ID => "primary",
        owner if owner == SECONDARY_USER_ID => "secondary",
        _ => {
            return Err(sea_orm::DbErr::Type(
                "job owner has no notification route".to_owned(),
            ));
        }
    };
    // Terminal, action-required, and lifecycle events follow the job's configured
    // notify scope. Progress milestones are routed to the initiator only, even for
    // a Family job, so a co-owner is not pinged for every download/transcode start.
    let scope_recipients: Vec<&str> = match job.notify_scope() {
        NotifyScope::Family => vec!["primary", "secondary"],
        NotifyScope::Initiator => vec![initiator],
    };
    for (event_type, message) in notifications {
        let recipients: &[&str] = if NotificationEventType::from_wire(event_type)
            .is_some_and(NotificationEventType::is_progress_milestone)
        {
            std::slice::from_ref(&initiator)
        } else {
            &scope_recipients
        };
        let source_dedupe_key = notification_dedupe_key(job, event_type);
        for recipient in recipients {
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO notification_outbox \
                     (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload) \
                     VALUES ($1, 'job', $2, $3, $4, $5, $6) \
                     ON CONFLICT (source_dedupe_key, recipient) DO NOTHING",
                    [
                        Uuid::new_v4().into(),
                        job.id().into_uuid().into(),
                        event_type.into(),
                        (*recipient).into(),
                        source_dedupe_key.clone().into(),
                        serde_json::json!({"message": message}).into(),
                    ],
                ))
                .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct JobNotificationContext {
    title: Option<String>,
    media_kind: Option<String>,
    season: Option<u64>,
    episode: Option<u64>,
    episode_count: Option<usize>,
    translation: Option<String>,
    translation_id: Option<u64>,
}

impl JobNotificationContext {
    fn details(&self, job: &Job) -> String {
        let mut lines = Vec::with_capacity(6);
        if let Some(title) = &self.title {
            lines.push(format!("Название: {title}"));
        }
        lines.push(format!(
            "Источник: {}",
            match job.provider() {
                media_core::Provider::Rezka => "Rezka",
                media_core::Provider::Prowlarr => "Prowlarr",
            }
        ));
        if let Some(kind) = &self.media_kind {
            lines.push(format!("Тип: {kind}"));
        }
        match (self.season, self.episode) {
            (Some(season), Some(episode)) => {
                lines.push(format!("Серия: S{season:02}E{episode:02}"));
            }
            _ => {
                if let Some(count) = self.episode_count.filter(|count| *count > 0) {
                    lines.push(format!("Серий в задаче: {count}"));
                }
            }
        }
        if let Some(translation) = &self.translation {
            lines.push(format!("Перевод: {translation}"));
        } else if let Some(translation_id) = self.translation_id {
            lines.push(format!("Перевод: ID {translation_id}"));
        }
        lines.push(format!("Job ID: {}", job.id()));
        lines.join("\n")
    }

    fn message(&self, job: &Job, summary: &str) -> String {
        format!("{summary}\n{}", self.details(job))
    }
}

async fn notification_context(
    transaction: &sea_orm::DatabaseTransaction,
    job: &Job,
) -> Result<JobNotificationContext, sea_orm::DbErr> {
    let payload = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT payload FROM search_executions WHERE result_ref = $1",
            [job.result_ref().into()],
        ))
        .await?
        .map(|row| row.try_get::<serde_json::Value>("", "payload"))
        .transpose()?;
    let Some(payload) = payload else {
        return Ok(JobNotificationContext::default());
    };
    let safe = |name: &str| {
        payload
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(safe_notification_field)
            .filter(|value| !value.is_empty())
    };
    let episodes = payload
        .get("episodes")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len);
    Ok(JobNotificationContext {
        title: safe("title"),
        media_kind: payload
            .get("media_kind")
            .and_then(serde_json::Value::as_str)
            .and_then(|kind| match kind {
                "movie" => Some("фильм".to_owned()),
                "series" => Some("сериал".to_owned()),
                _ => None,
            }),
        season: payload.get("season").and_then(serde_json::Value::as_u64),
        episode: payload.get("episode").and_then(serde_json::Value::as_u64),
        episode_count: episodes,
        translation: safe("translation"),
        translation_id: payload
            .get("translation_id")
            .and_then(serde_json::Value::as_u64),
    })
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

fn notifications_for_event(
    job: &Job,
    event: &JobEvent,
    context: &JobNotificationContext,
) -> Vec<(&'static str, String)> {
    let id = job.id();
    let session_refresh = job.result_ref().starts_with("selection:session-refresh:");
    match event.kind() {
        JobEventKind::Started if session_refresh => {
            vec![("started", format!("Rezka session refresh {id} started."))]
        }
        JobEventKind::Started => vec![(
            "started",
            context.message(job, "Задача принята в обработку."),
        )],
        // Intermediate progress. A stage start marks the beginning of a phase, so
        // it maps to a "started" notification. Deduplication on
        // (source_dedupe_key, recipient) collapses stage retries and per-episode
        // repeats into a single notification per (job, phase). The runner carries
        // no percent data, so these are milestones only, never progress fractions.
        JobEventKind::StageStarted(stage)
            if matches!(
                stage.name(),
                "download" | "torrent_monitor" | "media_pipeline"
            ) =>
        {
            vec![(
                "downloading-started",
                context.message(job, "Скачивание началось."),
            )]
        }
        JobEventKind::StageStarted(stage)
            if matches!(stage.name(), "encode" | "encoding" | "transcode") =>
        {
            vec![(
                "transcoding-started",
                context.message(job, "Перекодирование Rezka-видео через VAAPI началось."),
            )]
        }
        JobEventKind::StageCompleted { stage, .. }
            if matches!(stage.name(), "download" | "torrent_monitor") =>
        {
            vec![(
                "downloaded",
                context.message(job, "Скачивание завершено."),
            )]
        }
        JobEventKind::StageCompleted { stage, .. }
            if matches!(stage.name(), "encode" | "encoding" | "transcode") =>
        {
            vec![(
                "encoding-complete",
                context.message(job, "Перекодирование завершено."),
            )]
        }
        JobEventKind::StageFailed { error_code, .. }
            if session_refresh && job.state() == JobState::Failed =>
        {
            let error_code = sanitized_error_code(error_code);
            vec![(
                "failed",
                format!("Rezka session refresh {id} failed ({error_code})."),
            )]
        }
        JobEventKind::StageFailed { error_code, .. } if job.state() == JobState::Failed => {
            let error_code = sanitized_error_code(error_code);
            vec![(
                "failed",
                context.message(job, &failure_description(error_code)),
            )]
        }
        JobEventKind::JobTransition { state, .. } => match state {
            JobState::NeedsAction => vec![(
                "choice-needed",
                context.message(job, "Нужен дополнительный выбор, чтобы продолжить задачу."),
            )],
            JobState::BlockedStorage => vec![(
                "blocked-storage",
                context.message(
                    job,
                    "Загрузка приостановлена: недостаточно места для скачивания и перекодирования. Освободите место и повторите задачу.",
                ),
            )],
            JobState::Publishing if session_refresh => Vec::new(),
            JobState::Publishing => {
                let mut notifications = vec![(
                    "downloaded",
                    context.message(job, "Скачивание завершено."),
                )];
                if job.provider() == media_core::Provider::Rezka {
                    notifications.push((
                        "encoding-complete",
                        context.message(job, "Перекодирование завершено."),
                    ));
                }
                notifications
            }
            JobState::Partial => vec![
                ("plex-added", context.message(job, "Видео добавлено в Plex.")),
                (
                    "partial",
                    context.message(
                        job,
                        "Видео готово и добавлено в Plex, но часть субтитров скачать не удалось. Повторите задачу: уже готовое видео не будет скачиваться или перекодироваться заново.",
                    ),
                ),
            ],
            JobState::Completed if session_refresh => vec![(
                "session-refreshed",
                format!("Rezka session refresh {id} completed and was saved."),
            )],
            JobState::Completed => {
                vec![
                    ("plex-added", context.message(job, "Видео добавлено в Plex.")),
                    (
                        "completed",
                        context.message(job, "Задача полностью завершена: видео и доступные субтитры готовы."),
                    ),
                ]
            }
            JobState::Failed => vec![(
                "failed",
                context.message(job, "Задача завершилась ошибкой."),
            )],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn notification_dedupe_key(job: &Job, event_type: &str) -> Vec<u8> {
    let mut key = job.id().into_uuid().as_bytes().to_vec();
    key.extend_from_slice(event_type.as_bytes());
    key
}

fn sanitized_error_code(error_code: &str) -> &str {
    let safe_length = error_code
        .char_indices()
        .take_while(|(_, character)| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        .take(64)
        .last()
        .map_or(0, |(index, character)| index + character.len_utf8());
    if safe_length == 0 {
        "provider_error"
    } else {
        &error_code[..safe_length]
    }
}

fn failure_description(error_code: &str) -> String {
    let explanation = match error_code {
        "stream_expired" => {
            "Ссылка на видеопоток Rezka истекла. Автоматические попытки закончились; повторите задачу, чтобы получить новую ссылку."
        }
        "source_transfer_transient" => {
            "CDN временно не смог передать видео. Автоматические попытки закончились; повторите задачу позже."
        }
        "source_transfer_rejected" => {
            "CDN отклонил выбранный видеопоток. Проверьте доступ к переводу или выберите другой результат."
        }
        "runner_service_unavailable" => {
            "Runner потерял связь с media-service. Повторите задачу после восстановления сервиса."
        }
        "runner_configuration_invalid" => {
            "Runner настроен некорректно. Требуется проверить конфигурацию сервиса."
        }
        _ => {
            "Не удалось скачать или обработать медиа. Повторите задачу; если ошибка повторится, проверьте Job ID в журнале."
        }
    };
    format!("{explanation}\nКод ошибки: {error_code}")
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
            start_stage(transaction, current.id(), stage).await?;
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
            let terminal =
                fail_stage(transaction, current.id(), stage, *retryable, error_code).await?;
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
             AND (job_stages.state = 'completed' OR job_stages.attempt_count < 3) \
             AND job_stages.ordinal = EXCLUDED.ordinal RETURNING attempt_count",
            [
                Uuid::new_v4().into(),
                task_id.into(),
                stage.name().into(),
                ordinal.into(),
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
    Ok(())
}

async fn fail_stage(
    transaction: &sea_orm::DatabaseTransaction,
    job_id: media_core::JobId,
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
    let terminal =
        StageFailureOutcome::for_attempt(attempt, retryable) == StageFailureOutcome::Failed;
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
) -> Result<Option<JobLease>, sea_orm::DbErr> {
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
            "SELECT state FROM runner_lifecycle WHERE singleton = true FOR SHARE".to_owned(),
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("runner lifecycle is missing".to_owned()))?;
    if lifecycle.try_get::<String>("", "state")? != "ready" {
        return Ok(None);
    }

    if let Some(existing) = transaction
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT job_id, expires_at > now() AS active \
             FROM job_leases WHERE slot = 1 FOR UPDATE",
        ))
        .await?
    {
        if existing.try_get::<bool>("", "active")? {
            return Ok(None);
        }
        let expired_job = existing.try_get::<Uuid>("", "job_id")?;
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE job_stages SET state = 'pending', updated_at = now() \
                 WHERE state = 'running' AND task_id IN \
                 (SELECT id FROM job_tasks WHERE job_id = $1)",
                [expired_job.into()],
            ))
            .await?;
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
            "UPDATE jobs SET state = 'leased', attempt_count = attempt_count + 1, \
             updated_at = now() WHERE id = (SELECT id FROM jobs WHERE state = 'queued' \
             ORDER BY created_at, id FOR UPDATE SKIP LOCKED LIMIT 1) AND state = 'queued' \
             RETURNING id",
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
        return Ok(None);
    };
    let job_id = candidate.try_get::<Uuid>("", "id")?;
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
            return Ok(None);
        }
        return Err(sea_orm::DbErr::Custom(
            "lease slot conflicted without an active lease".to_owned(),
        ));
    };
    let expires_at = inserted.try_get("", "expires_at")?;
    let job = load_job(transaction, job_id).await?;
    debug_assert_eq!(job.state(), JobState::Leased);
    Ok(Some(JobLease::new(
        LeaseId::from_uuid(lease_id),
        job,
        runner,
        expires_at,
    )))
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
