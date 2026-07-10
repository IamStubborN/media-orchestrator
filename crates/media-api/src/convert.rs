use media_contract::{
    CreateJobRequest, JobDto, JobStateDto, LeaseDto, NeedsActionReasonDto, NotifyScopeDto,
    ProviderDto, PublicId, QueueStatusDto,
};
use media_core::{
    Job, JobLease, JobState, NeedsActionReason, NewJobCommand, NotifyScope, Provider, QueueStatus,
};
use time::format_description::well_known::Rfc3339;

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

pub(crate) fn lease(lease: &JobLease) -> Result<LeaseDto, time::error::Format> {
    Ok(LeaseDto {
        lease_id: public_id(lease.lease_id().to_string()),
        job: job(lease.job()),
        expires_at: lease.expires_at().format(&Rfc3339)?,
    })
}

#[must_use]
pub(crate) const fn queue_status(status: QueueStatus) -> QueueStatusDto {
    QueueStatusDto {
        queued: status.queued,
        active: status.active,
    }
}

fn public_id(value: String) -> PublicId {
    PublicId::parse(&value).expect("domain IDs are valid UUIDs")
}
