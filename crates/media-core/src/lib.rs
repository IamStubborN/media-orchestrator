//! Pure domain types and policies for media orchestration.

#![forbid(unsafe_code)]

mod action;
mod actor;
mod application;
mod id;
mod identity;
mod job;
mod port;

pub use action::NeedsActionReason;
pub use actor::{
    Actor, ActorError, BootstrapClient, BootstrapClientError, ClientRole, CredentialDigest,
};
pub use application::{
    ApplicationError, JobApplication, LeaseApplication, LeaseTtlError, NewJobCommand,
};
pub use id::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, ClientId, EpisodeId, JobId, LeaseId, MediaId,
    RUNNER_CLIENT_ID, SeasonId, TaskId, UserId, SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
pub use identity::{
    CanonicalEpisode, CanonicalMedia, CanonicalSeason, EpisodeProviderMapping, EpisodeResolution,
    ExternalNamespace, ExternalReference, IdentityValidationError, MappingSource,
    MediaExternalReference, MediaKind, SeriesOrdering, resolve_episode_candidates,
};
pub use job::{
    Job, JobLease, JobState, JobTransitionError, JobValidationError, NewJob, NotifyScope, Provider,
    QueueStatus,
};
pub use port::{ClientStore, IdentityStore, JobStore, LeaseStore, PortError, ReadinessPort};
