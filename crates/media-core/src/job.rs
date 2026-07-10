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
    id: JobId,
    owner_id: UserId,
    provider: Provider,
    result_ref: String,
    state: JobState,
    needs_action_reason: Option<NeedsActionReason>,
    notify_scope: NotifyScope,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NewJob {
    id: JobId,
    owner_id: UserId,
    provider: Provider,
    result_ref: String,
    notify_scope: NotifyScope,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum JobValidationError {
    #[error("result reference cannot be empty")]
    EmptyResultReference,
    #[error("NeedsAction jobs require an action reason")]
    NeedsActionReasonRequired,
    #[error("job state {state:?} cannot have a NeedsAction reason")]
    UnexpectedNeedsActionReason { state: JobState },
}

impl NewJob {
    pub fn new(
        id: JobId,
        owner_id: UserId,
        provider: Provider,
        result_ref: String,
        notify_scope: NotifyScope,
    ) -> Result<Self, JobValidationError> {
        validate_result_ref(&result_ref)?;

        Ok(Self {
            id,
            owner_id,
            provider,
            result_ref,
            notify_scope,
        })
    }

    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    #[must_use]
    pub const fn owner_id(&self) -> UserId {
        self.owner_id
    }

    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }

    #[must_use]
    pub fn result_ref(&self) -> &str {
        &self.result_ref
    }

    #[must_use]
    pub const fn notify_scope(&self) -> NotifyScope {
        self.notify_scope
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct QueueStatus {
    pub queued: u64,
    pub active: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct JobLease {
    lease_id: LeaseId,
    job: Job,
    runner_client_id: ClientId,
    expires_at: time::OffsetDateTime,
}

impl JobLease {
    #[must_use]
    pub const fn new(
        lease_id: LeaseId,
        job: Job,
        runner_client_id: ClientId,
        expires_at: time::OffsetDateTime,
    ) -> Self {
        Self {
            lease_id,
            job,
            runner_client_id,
            expires_at,
        }
    }

    #[must_use]
    pub const fn lease_id(&self) -> LeaseId {
        self.lease_id
    }

    #[must_use]
    pub const fn job(&self) -> &Job {
        &self.job
    }

    #[must_use]
    pub const fn runner_client_id(&self) -> ClientId {
        self.runner_client_id
    }

    #[must_use]
    pub const fn expires_at(&self) -> time::OffsetDateTime {
        self.expires_at
    }
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

impl Job {
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        id: JobId,
        owner_id: UserId,
        provider: Provider,
        result_ref: String,
        state: JobState,
        needs_action_reason: Option<NeedsActionReason>,
        notify_scope: NotifyScope,
    ) -> Result<Self, JobValidationError> {
        validate_result_ref(&result_ref)?;
        match (state, needs_action_reason) {
            (JobState::NeedsAction, None) => {
                return Err(JobValidationError::NeedsActionReasonRequired);
            }
            (JobState::NeedsAction, Some(_)) | (_, None) => {}
            (_, Some(_)) => {
                return Err(JobValidationError::UnexpectedNeedsActionReason { state });
            }
        }

        Ok(Self {
            id,
            owner_id,
            provider,
            result_ref,
            state,
            needs_action_reason,
            notify_scope,
        })
    }

    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    #[must_use]
    pub const fn owner_id(&self) -> UserId {
        self.owner_id
    }

    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }

    #[must_use]
    pub fn result_ref(&self) -> &str {
        &self.result_ref
    }

    #[must_use]
    pub const fn state(&self) -> JobState {
        self.state
    }

    #[must_use]
    pub const fn needs_action_reason(&self) -> Option<NeedsActionReason> {
        self.needs_action_reason
    }

    #[must_use]
    pub const fn notify_scope(&self) -> NotifyScope {
        self.notify_scope
    }
}

fn validate_result_ref(result_ref: &str) -> Result<(), JobValidationError> {
    if result_ref.trim().is_empty() {
        Err(JobValidationError::EmptyResultReference)
    } else {
        Ok(())
    }
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
                    Self::Queued
                        | Self::CancelRequested
                        | Self::BlockedStorage
                        | Self::Publishing
                        | Self::NeedsAction
                        | Self::Failed
                )
                | (Self::CancelRequested, Self::Cancelled)
                | (Self::BlockedStorage, Self::Queued | Self::CancelRequested)
                | (
                    Self::Publishing,
                    Self::Queued | Self::PlexPending | Self::Failed
                )
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
    use super::{Job, JobLease, JobState, JobValidationError, NewJob, NotifyScope, Provider};
    use crate::{PRIMARY_USER_ID, JobId, LeaseId, NeedsActionReason, RUNNER_CLIENT_ID};

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

    const ALLOWED: [(JobState, JobState); 24] = [
        (JobState::Queued, JobState::Leased),
        (JobState::Leased, JobState::Running),
        (JobState::Leased, JobState::Queued),
        (JobState::Leased, JobState::CancelRequested),
        (JobState::Running, JobState::CancelRequested),
        (JobState::Running, JobState::Queued),
        (JobState::Running, JobState::BlockedStorage),
        (JobState::Running, JobState::Publishing),
        (JobState::Running, JobState::NeedsAction),
        (JobState::Running, JobState::Failed),
        (JobState::CancelRequested, JobState::Cancelled),
        (JobState::BlockedStorage, JobState::Queued),
        (JobState::BlockedStorage, JobState::CancelRequested),
        (JobState::Publishing, JobState::PlexPending),
        (JobState::Publishing, JobState::Queued),
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
    fn expired_active_jobs_can_resume_from_durable_checkpoints() {
        for state in [JobState::Leased, JobState::Running, JobState::Publishing] {
            assert_eq!(
                state.transition(JobState::Queued).unwrap(),
                JobState::Queued,
                "{state:?} must be recoverable after its lease expires",
            );
        }
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

        assert_eq!(job.id(), id);
        assert_eq!(job.owner_id(), PRIMARY_USER_ID);
        assert_eq!(job.provider(), Provider::Prowlarr);
        assert_eq!(job.result_ref(), "result-42");
        assert_eq!(job.notify_scope(), NotifyScope::Family);
    }

    #[test]
    fn rehydrated_job_rejects_an_empty_result_reference() {
        let error = Job::rehydrate(
            JobId::new(),
            PRIMARY_USER_ID,
            Provider::Rezka,
            "  ".to_owned(),
            JobState::Queued,
            None,
            NotifyScope::Initiator,
        )
        .unwrap_err();

        assert_eq!(error, JobValidationError::EmptyResultReference);
    }

    #[test]
    fn needs_action_job_requires_a_reason() {
        let error = Job::rehydrate(
            JobId::new(),
            PRIMARY_USER_ID,
            Provider::Rezka,
            "result".to_owned(),
            JobState::NeedsAction,
            None,
            NotifyScope::Initiator,
        )
        .unwrap_err();

        assert_eq!(error, JobValidationError::NeedsActionReasonRequired);
    }

    #[test]
    fn non_needs_action_job_rejects_a_reason() {
        let error = Job::rehydrate(
            JobId::new(),
            PRIMARY_USER_ID,
            Provider::Rezka,
            "result".to_owned(),
            JobState::Queued,
            Some(NeedsActionReason::IdentityAmbiguous),
            NotifyScope::Initiator,
        )
        .unwrap_err();

        assert_eq!(
            error,
            JobValidationError::UnexpectedNeedsActionReason {
                state: JobState::Queued,
            },
        );
    }

    #[test]
    fn rehydrated_job_exposes_validated_state_through_read_only_accessors() {
        let id = JobId::new();
        let job = Job::rehydrate(
            id,
            PRIMARY_USER_ID,
            Provider::Prowlarr,
            "result-42".to_owned(),
            JobState::NeedsAction,
            Some(NeedsActionReason::IdentityAmbiguous),
            NotifyScope::Family,
        )
        .unwrap();

        assert_eq!(job.id(), id);
        assert_eq!(job.owner_id(), PRIMARY_USER_ID);
        assert_eq!(job.provider(), Provider::Prowlarr);
        assert_eq!(job.result_ref(), "result-42");
        assert_eq!(job.state(), JobState::NeedsAction);
        assert_eq!(
            job.needs_action_reason(),
            Some(NeedsActionReason::IdentityAmbiguous),
        );
        assert_eq!(job.notify_scope(), NotifyScope::Family);
    }

    #[test]
    fn job_lease_exposes_an_immutable_validated_job_mapping() {
        let lease_id = LeaseId::new();
        let expires_at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(60);
        let job = Job::rehydrate(
            JobId::new(),
            PRIMARY_USER_ID,
            Provider::Rezka,
            "result".to_owned(),
            JobState::Leased,
            None,
            NotifyScope::Initiator,
        )
        .unwrap();
        let lease = JobLease::new(lease_id, job.clone(), RUNNER_CLIENT_ID, expires_at);

        assert_eq!(lease.lease_id(), lease_id);
        assert_eq!(lease.job(), &job);
        assert_eq!(lease.runner_client_id(), RUNNER_CLIENT_ID);
        assert_eq!(lease.expires_at(), expires_at);
    }

    #[test]
    fn job_lease_can_contain_a_running_job_during_heartbeat() {
        let job = Job::rehydrate(
            JobId::new(),
            PRIMARY_USER_ID,
            Provider::Rezka,
            "result".to_owned(),
            JobState::Running,
            None,
            NotifyScope::Initiator,
        )
        .unwrap();

        let lease = JobLease::new(
            LeaseId::new(),
            job,
            RUNNER_CLIENT_ID,
            time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(60),
        );

        assert_eq!(lease.job().state(), JobState::Running);
    }
}
