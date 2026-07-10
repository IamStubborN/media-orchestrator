use crate::{ClientId, JobId, LeaseId, NeedsActionReason, UserId};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Provider {
    Rezka,
    Prowlarr,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum NotifyScope {
    Initiator,
    Family,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Job {
    pub id: JobId,
    pub owner_id: UserId,
    pub provider: Provider,
    pub result_ref: String,
    pub state: JobState,
    pub needs_action_reason: Option<NeedsActionReason>,
    pub notify_scope: NotifyScope,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NewJob {
    pub id: JobId,
    pub owner_id: UserId,
    pub provider: Provider,
    pub result_ref: String,
    pub notify_scope: NotifyScope,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum JobValidationError {
    #[error("result reference cannot be empty")]
    EmptyResultReference,
}

impl NewJob {
    pub fn new(
        id: JobId,
        owner_id: UserId,
        provider: Provider,
        result_ref: String,
        notify_scope: NotifyScope,
    ) -> Result<Self, JobValidationError> {
        if result_ref.trim().is_empty() {
            return Err(JobValidationError::EmptyResultReference);
        }

        Ok(Self {
            id,
            owner_id,
            provider,
            result_ref,
            notify_scope,
        })
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct QueueStatus {
    pub queued: u64,
    pub active: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct JobLease {
    pub lease_id: LeaseId,
    pub job: Job,
    pub runner_client_id: ClientId,
    pub expires_at: time::OffsetDateTime,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum JobState {
    Queued,
    Leased,
    Running,
    CancelRequested,
    BlockedStorage,
    Publishing,
    PlexPending,
    NeedsAction,
    Partial,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("invalid job transition from {from:?} to {to:?}")]
pub struct JobTransitionError {
    pub from: JobState,
    pub to: JobState,
}

impl JobState {
    pub fn transition(self, next: Self) -> Result<Self, JobTransitionError> {
        let allowed = matches!(
            (self, next),
            (Self::Queued, Self::Leased)
                | (
                    Self::Leased,
                    Self::Running | Self::Queued | Self::CancelRequested
                )
                | (
                    Self::Running,
                    Self::CancelRequested
                        | Self::BlockedStorage
                        | Self::Publishing
                        | Self::NeedsAction
                        | Self::Failed
                )
                | (Self::CancelRequested, Self::Cancelled)
                | (Self::BlockedStorage, Self::Queued | Self::CancelRequested)
                | (Self::Publishing, Self::PlexPending | Self::Failed)
                | (
                    Self::PlexPending,
                    Self::Completed | Self::Partial | Self::NeedsAction | Self::Failed
                )
                | (Self::NeedsAction, Self::Queued | Self::CancelRequested)
                | (Self::Partial | Self::Failed, Self::Queued)
        );

        if allowed {
            Ok(next)
        } else {
            Err(JobTransitionError {
                from: self,
                to: next,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{JobState, JobValidationError, NewJob, NotifyScope, Provider};
    use crate::{PRIMARY_USER_ID, JobId};

    const STATES: [JobState; 12] = [
        JobState::Queued,
        JobState::Leased,
        JobState::Running,
        JobState::CancelRequested,
        JobState::BlockedStorage,
        JobState::Publishing,
        JobState::PlexPending,
        JobState::NeedsAction,
        JobState::Partial,
        JobState::Completed,
        JobState::Failed,
        JobState::Cancelled,
    ];

    const ALLOWED: [(JobState, JobState); 22] = [
        (JobState::Queued, JobState::Leased),
        (JobState::Leased, JobState::Running),
        (JobState::Leased, JobState::Queued),
        (JobState::Leased, JobState::CancelRequested),
        (JobState::Running, JobState::CancelRequested),
        (JobState::Running, JobState::BlockedStorage),
        (JobState::Running, JobState::Publishing),
        (JobState::Running, JobState::NeedsAction),
        (JobState::Running, JobState::Failed),
        (JobState::CancelRequested, JobState::Cancelled),
        (JobState::BlockedStorage, JobState::Queued),
        (JobState::BlockedStorage, JobState::CancelRequested),
        (JobState::Publishing, JobState::PlexPending),
        (JobState::Publishing, JobState::Failed),
        (JobState::PlexPending, JobState::Completed),
        (JobState::PlexPending, JobState::Partial),
        (JobState::PlexPending, JobState::NeedsAction),
        (JobState::PlexPending, JobState::Failed),
        (JobState::NeedsAction, JobState::Queued),
        (JobState::NeedsAction, JobState::CancelRequested),
        (JobState::Partial, JobState::Queued),
        (JobState::Failed, JobState::Queued),
    ];

    #[test]
    fn plex_verified_path_can_complete() {
        let state = JobState::Publishing
            .transition(JobState::PlexPending)
            .unwrap()
            .transition(JobState::Completed)
            .unwrap();
        assert_eq!(state, JobState::Completed);
    }

    #[test]
    fn running_job_cannot_skip_plex_verification() {
        let error = JobState::Running
            .transition(JobState::Completed)
            .unwrap_err();
        assert_eq!(error.from, JobState::Running);
        assert_eq!(error.to, JobState::Completed);
    }

    #[test]
    fn cancellation_is_cooperative() {
        let state = JobState::Running
            .transition(JobState::CancelRequested)
            .unwrap()
            .transition(JobState::Cancelled)
            .unwrap();
        assert_eq!(state, JobState::Cancelled);
    }

    #[test]
    fn expired_lease_can_return_to_queue() {
        assert_eq!(
            JobState::Leased.transition(JobState::Queued).unwrap(),
            JobState::Queued,
        );
    }

    #[test]
    fn transition_table_rejects_every_unlisted_pair() {
        for from in STATES {
            for to in STATES {
                let expected = ALLOWED.contains(&(from, to));
                assert_eq!(
                    from.transition(to).is_ok(),
                    expected,
                    "unexpected transition result for {from:?} -> {to:?}",
                );
            }
        }
    }

    #[test]
    fn new_job_rejects_an_empty_result_reference() {
        let error = NewJob::new(
            JobId::new(),
            PRIMARY_USER_ID,
            Provider::Rezka,
            " \t\n".to_owned(),
            NotifyScope::Initiator,
        )
        .unwrap_err();

        assert_eq!(error, JobValidationError::EmptyResultReference);
    }

    #[test]
    fn new_job_preserves_valid_selection_details() {
        let id = JobId::new();
        let job = NewJob::new(
            id,
            PRIMARY_USER_ID,
            Provider::Prowlarr,
            "result-42".to_owned(),
            NotifyScope::Family,
        )
        .unwrap();

        assert_eq!(job.id, id);
        assert_eq!(job.owner_id, PRIMARY_USER_ID);
        assert_eq!(job.provider, Provider::Prowlarr);
        assert_eq!(job.result_ref, "result-42");
        assert_eq!(job.notify_scope, NotifyScope::Family);
    }
}
