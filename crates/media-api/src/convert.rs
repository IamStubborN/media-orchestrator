use media_contract::{
    CheckpointValueDto, CreateJobRequest, CreateTrackingRequest, EpisodeSnapshotDto, JobDetailDto,
    JobDto, JobStateDto, LeaseDto, NeedsActionReasonDto, NotifyScopeDto, PatchTrackingRequest,
    ProviderDto, PublicId, QueueStatusDto, ReleaseCandidateDto, ReleaseLifecycleDto,
    ReleasePrecisionDto, ReleaseQueryResponse, RunnerEventDto, RunnerEventRequest,
    RunnerLifecycleStateDto, ScheduledEpisodeDto, TrackingCheckStatusDto, TrackingDto,
    TrackingReleaseIdentityDto, TrackingReleaseSourceDto, TrackingScopeDto, TrackingStateDto,
    TransferKindDto, TransferProgressDto,
};
use media_core::{
    CheckpointValue, EpisodeSnapshot, Job, JobDetail, JobEvent, JobEventId,
    JobEventValidationError, JobLease, JobState, NeedsActionReason, NewJobCommand,
    NewTrackingCommand, NotifyScope, Provider, QueueStatus, ReleaseCandidate, ReleaseIdentity,
    ReleaseLifecycle, ReleaseMetadataResult, ReleasePrecision, ReleaseSource, ScheduledEpisode,
    TrackingDownloadPatch, TrackingScope, TrackingSubscription,
};
use time::format_description::well_known::Rfc3339;

pub(crate) fn release_result(result: ReleaseMetadataResult) -> ReleaseQueryResponse {
    match result {
        ReleaseMetadataResult::Matched {
            source,
            fetched_at,
            show,
            precision,
            lifecycle,
            released_episodes,
            expected_episodes,
            next_episode,
            schedule,
        } => ReleaseQueryResponse::Matched {
            source,
            fetched_at,
            show: release_candidate(show),
            precision: release_precision(precision),
            lifecycle: release_lifecycle(lifecycle),
            released_episodes,
            expected_episodes,
            next_episode: next_episode.map(scheduled_episode),
            schedule: schedule.into_iter().map(scheduled_episode).collect(),
        },
        ReleaseMetadataResult::ChoiceNeeded {
            source,
            fetched_at,
            candidates,
        } => ReleaseQueryResponse::ChoiceNeeded {
            source,
            fetched_at,
            candidates: candidates.into_iter().map(release_candidate).collect(),
        },
    }
}

fn release_candidate(value: ReleaseCandidate) -> ReleaseCandidateDto {
    ReleaseCandidateDto {
        source_id: value.source_id,
        title: value.title,
        original_title: value.original_title,
        year: value.year,
        poster_url: value.poster_url,
        lifecycle: release_lifecycle(value.lifecycle),
    }
}

fn scheduled_episode(value: ScheduledEpisode) -> ScheduledEpisodeDto {
    ScheduledEpisodeDto {
        source_id: value.source_id,
        season: value.season,
        episode: value.episode,
        title: value.title,
        air_at: value.air_at,
        precision: release_precision(value.precision),
    }
}

const fn release_lifecycle(value: ReleaseLifecycle) -> ReleaseLifecycleDto {
    match value {
        ReleaseLifecycle::Ongoing => ReleaseLifecycleDto::Ongoing,
        ReleaseLifecycle::Ended => ReleaseLifecycleDto::Ended,
        ReleaseLifecycle::Upcoming => ReleaseLifecycleDto::Upcoming,
        ReleaseLifecycle::Unknown => ReleaseLifecycleDto::Unknown,
    }
}

const fn release_precision(value: ReleasePrecision) -> ReleasePrecisionDto {
    match value {
        ReleasePrecision::Date => ReleasePrecisionDto::Date,
        ReleasePrecision::DateTime => ReleasePrecisionDto::DateTime,
        ReleasePrecision::Unknown => ReleasePrecisionDto::Unknown,
    }
}

#[must_use]
pub(crate) fn new_job_command(request: CreateJobRequest) -> NewJobCommand {
    NewJobCommand {
        provider: match request.provider {
            ProviderDto::Rezka => Provider::Rezka,
            ProviderDto::Prowlarr => Provider::Prowlarr,
        },
        result_ref: request.result_ref,
        notify_scope: match request.notify_scope {
            NotifyScopeDto::Initiator => NotifyScope::Initiator,
            NotifyScopeDto::Family => NotifyScope::Family,
        },
    }
}

pub(crate) fn new_tracking_command(
    request: CreateTrackingRequest,
) -> Result<NewTrackingCommand, ()> {
    let known_episodes = request
        .known_episodes
        .into_iter()
        .map(|episode| EpisodeSnapshot::new(episode.season, episode.episode).map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NewTrackingCommand {
        provider: match request.provider {
            ProviderDto::Rezka => Provider::Rezka,
            ProviderDto::Prowlarr => Provider::Prowlarr,
        },
        title: request.title,
        translation: request.translation,
        known_episodes,
        scope: match request.scope {
            TrackingScopeDto::Personal => TrackingScope::Personal,
            TrackingScopeDto::Family => TrackingScope::Family,
        },
        series_ongoing: request.series_ongoing,
        release_identity: request
            .release_identity
            .map(|identity| {
                ReleaseIdentity::new(
                    match identity.source {
                        TrackingReleaseSourceDto::Tvmaze => ReleaseSource::Tvmaze,
                    },
                    identity.source_id,
                )
                .map_err(|_| ())
            })
            .transpose()?,
        download: request
            .download
            .map(|download| {
                media_core::TrackingDownload::new(
                    download.provider_media_ref,
                    download.translation_id,
                    download.season,
                )
                .map_err(|_| ())
            })
            .transpose()?,
    })
}

pub(crate) fn tracking_download_patch(
    request: PatchTrackingRequest,
) -> Result<TrackingDownloadPatch, ()> {
    let download = media_core::TrackingDownload::new(
        request.download.provider_media_ref,
        request.download.translation_id,
        request.download.season,
    )
    .map_err(|_| ())?;
    TrackingDownloadPatch::new(request.translation, download).map_err(|_| ())
}

pub(crate) fn tracking(value: &TrackingSubscription) -> TrackingDto {
    TrackingDto {
        id: public_id(value.id().to_string()),
        provider: match value.provider() {
            Provider::Rezka => ProviderDto::Rezka,
            Provider::Prowlarr => ProviderDto::Prowlarr,
        },
        title: value.title().to_owned(),
        translation: value.translation().to_owned(),
        known_episodes: value
            .known_episodes()
            .iter()
            .map(|episode| EpisodeSnapshotDto {
                season: episode.season(),
                episode: episode.episode(),
            })
            .collect(),
        scope: match value.scope() {
            TrackingScope::Personal => TrackingScopeDto::Personal,
            TrackingScope::Family => TrackingScopeDto::Family,
        },
        state: TrackingStateDto::Active,
        check_status: match value.check_status() {
            media_core::TrackingCheckStatus::Never => TrackingCheckStatusDto::Never,
            media_core::TrackingCheckStatus::NoNewEpisode => TrackingCheckStatusDto::NoNewEpisode,
            media_core::TrackingCheckStatus::AwaitingSource => {
                TrackingCheckStatusDto::AwaitingSource
            }
            media_core::TrackingCheckStatus::EpisodeFound => TrackingCheckStatusDto::EpisodeFound,
            media_core::TrackingCheckStatus::DownloadQueued => {
                TrackingCheckStatusDto::DownloadQueued
            }
            media_core::TrackingCheckStatus::ReleaseError => TrackingCheckStatusDto::ReleaseError,
            media_core::TrackingCheckStatus::SourceError => TrackingCheckStatusDto::SourceError,
        },
        last_checked_at: value
            .last_checked_at()
            .map(|timestamp| timestamp.to_string()),
        next_check_at: value.next_check_at().to_string(),
        release_identity: value
            .release_identity()
            .map(|identity| TrackingReleaseIdentityDto {
                source: match identity.source() {
                    ReleaseSource::Tvmaze => TrackingReleaseSourceDto::Tvmaze,
                },
                source_id: identity.source_id(),
            }),
        download: value
            .download()
            .map(|download| media_contract::TrackingDownloadDto {
                provider_media_ref: download.provider_media_ref().to_owned(),
                translation_id: download.translation_id(),
                season: download.season(),
            }),
    }
}

pub(crate) fn runner_event(
    request: RunnerEventRequest,
) -> Result<JobEvent, JobEventValidationError> {
    let id = JobEventId::from_uuid(*request.event_id.as_uuid());
    match request.event {
        RunnerEventDto::Started => Ok(JobEvent::started(id)),
        RunnerEventDto::StageStarted {
            task_ordinal,
            stage_name,
            stage_ordinal,
        } => JobEvent::stage_started(id, task_ordinal, stage_name, stage_ordinal),
        RunnerEventDto::StageCheckpoint {
            task_ordinal,
            stage_name,
            stage_ordinal,
            checkpoint,
        } => JobEvent::stage_checkpoint(
            id,
            task_ordinal,
            stage_name,
            stage_ordinal,
            checkpoint
                .into_iter()
                .map(|(key, value)| (key, checkpoint_value(value)))
                .collect(),
        ),
        RunnerEventDto::StageCompleted {
            task_ordinal,
            stage_name,
            stage_ordinal,
            checkpoint,
        } => JobEvent::stage_completed(
            id,
            task_ordinal,
            stage_name,
            stage_ordinal,
            checkpoint
                .into_iter()
                .map(|(key, value)| (key, checkpoint_value(value)))
                .collect(),
        ),
        RunnerEventDto::StageFailed {
            task_ordinal,
            stage_name,
            stage_ordinal,
            retryable,
            error_code,
        } => JobEvent::stage_failed(
            id,
            task_ordinal,
            stage_name,
            stage_ordinal,
            retryable,
            error_code,
        ),
        RunnerEventDto::JobTransition {
            state,
            needs_action_reason,
        } => JobEvent::transition(
            id,
            domain_job_state(state),
            needs_action_reason.map(domain_needs_action_reason),
        ),
    }
}

fn checkpoint_value(value: CheckpointValueDto) -> CheckpointValue {
    match value {
        CheckpointValueDto::String(value) => CheckpointValue::String(value),
        CheckpointValueDto::Unsigned(value) => CheckpointValue::Unsigned(value),
        CheckpointValueDto::Bool(value) => CheckpointValue::Bool(value),
    }
}

const fn domain_job_state(value: JobStateDto) -> JobState {
    match value {
        JobStateDto::Queued => JobState::Queued,
        JobStateDto::Leased => JobState::Leased,
        JobStateDto::Running => JobState::Running,
        JobStateDto::CancelRequested => JobState::CancelRequested,
        JobStateDto::BlockedStorage => JobState::BlockedStorage,
        JobStateDto::Publishing => JobState::Publishing,
        JobStateDto::PlexPending => JobState::PlexPending,
        JobStateDto::NeedsAction => JobState::NeedsAction,
        JobStateDto::Partial => JobState::Partial,
        JobStateDto::Completed => JobState::Completed,
        JobStateDto::Failed => JobState::Failed,
        JobStateDto::Cancelled => JobState::Cancelled,
    }
}

const fn domain_needs_action_reason(value: NeedsActionReasonDto) -> NeedsActionReason {
    match value {
        NeedsActionReasonDto::IdentityAmbiguous => NeedsActionReason::IdentityAmbiguous,
        NeedsActionReasonDto::PlexMismatch => NeedsActionReason::PlexMismatch,
    }
}

#[must_use]
pub(crate) fn job(job: &Job) -> JobDto {
    JobDto {
        id: public_id(job.id().to_string()),
        provider: match job.provider() {
            Provider::Rezka => ProviderDto::Rezka,
            Provider::Prowlarr => ProviderDto::Prowlarr,
        },
        result_ref: job.result_ref().to_owned(),
        state: match job.state() {
            JobState::Queued => JobStateDto::Queued,
            JobState::Leased => JobStateDto::Leased,
            JobState::Running => JobStateDto::Running,
            JobState::CancelRequested => JobStateDto::CancelRequested,
            JobState::BlockedStorage => JobStateDto::BlockedStorage,
            JobState::Publishing => JobStateDto::Publishing,
            JobState::PlexPending => JobStateDto::PlexPending,
            JobState::NeedsAction => JobStateDto::NeedsAction,
            JobState::Partial => JobStateDto::Partial,
            JobState::Completed => JobStateDto::Completed,
            JobState::Failed => JobStateDto::Failed,
            JobState::Cancelled => JobStateDto::Cancelled,
        },
        needs_action_reason: job.needs_action_reason().map(|reason| match reason {
            NeedsActionReason::IdentityAmbiguous => NeedsActionReasonDto::IdentityAmbiguous,
            NeedsActionReason::PlexMismatch => NeedsActionReasonDto::PlexMismatch,
        }),
        notify_scope: match job.notify_scope() {
            NotifyScope::Initiator => NotifyScopeDto::Initiator,
            NotifyScope::Family => NotifyScopeDto::Family,
        },
    }
}

#[must_use]
pub(crate) fn job_detail(detail: &JobDetail) -> JobDetailDto {
    JobDetailDto {
        job: job(&detail.job),
        current_stage: detail.current_stage.clone(),
        progress: detail
            .progress
            .as_ref()
            .map(|progress| TransferProgressDto {
                kind: match progress.kind {
                    media_core::TransferKind::Direct => TransferKindDto::Direct,
                    media_core::TransferKind::Hls => TransferKindDto::Hls,
                    media_core::TransferKind::Torrent => TransferKindDto::Torrent,
                },
                state: progress.state.clone(),
                progress_percent: progress.progress_percent,
                downloaded_bytes: progress.downloaded_bytes,
                total_bytes: progress.total_bytes,
                download_speed_bps: progress.download_speed_bps,
                eta_seconds: progress.eta_seconds,
                seeds: progress.seeds,
                peers: progress.peers,
                updated_at: progress
                    .updated_at
                    .format(&Rfc3339)
                    .expect("progress timestamps format as RFC3339"),
            }),
    }
}

pub(crate) fn lease(lease: &JobLease) -> Result<LeaseDto, time::error::Format> {
    Ok(LeaseDto {
        lease_id: public_id(lease.lease_id().to_string()),
        job: job(lease.job()),
        execution: None,
        completed_task_ordinals: lease.completed_task_ordinals().to_vec(),
        expires_at: lease.expires_at().format(&Rfc3339)?,
    })
}

#[must_use]
pub(crate) fn queue_status(status: QueueStatus) -> QueueStatusDto {
    QueueStatusDto {
        queued: status.queued,
        active: status.active,
        runner_state: match status.runner_state {
            media_core::RunnerLifecycleState::Ready => RunnerLifecycleStateDto::Ready,
            media_core::RunnerLifecycleState::Rotating => RunnerLifecycleStateDto::Rotating,
            media_core::RunnerLifecycleState::Blocked => RunnerLifecycleStateDto::Blocked,
        },
        blocked_reason: status.blocked_reason,
    }
}

fn public_id(value: String) -> PublicId {
    PublicId::parse(&value).expect("domain IDs are valid UUIDs")
}
