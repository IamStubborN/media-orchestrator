//! Pure domain types and policies for media orchestration.

#![forbid(unsafe_code)]

mod action;
mod actor;
mod application;
mod id;
mod identity;
mod job;
mod operation;
mod orchestration;
mod port;
mod tracking;

pub use action::NeedsActionReason;
pub use actor::{
    Actor, ActorError, BootstrapClient, BootstrapClientError, ClientRole, CredentialDigest,
};
pub use application::{
    ApplicationError, JobApplication, LeaseApplication, LeaseTtlError, NewJobCommand,
};
pub use id::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, ClientId, EpisodeId, JobEventId, JobId, LeaseId, MediaId,
    NotificationId, RUNNER_CLIENT_ID, SeasonId, TaskId, TrackingId, UserId, SECONDARY_CLIENT_ID,
    SECONDARY_USER_ID,
};
pub use identity::{
    CanonicalEpisode, CanonicalMedia, CanonicalSeason, EpisodeProviderMapping, EpisodeResolution,
    ExternalNamespace, ExternalReference, IdentityValidationError, MappingSource,
    MediaExternalReference, MediaKind, SeriesOrdering, resolve_episode_candidates,
};
pub use job::{
    Job, JobLease, JobState, JobTransitionError, JobValidationError, MAX_RESULT_REF_BYTES, NewJob,
    NotifyScope, Provider, QueueStatus,
};
pub use operation::OperationKey;
pub use orchestration::{
    Checkpoint, CheckpointValue, JobEvent, JobEventKind, JobEventValidationError,
    MAX_STAGE_ATTEMPTS, StageFailureOutcome, StageRef,
};
pub use port::{ClientStore, IdentityStore, JobStore, LeaseStore, PortError, ReadinessPort};
pub use tracking::{
    EpisodeSnapshot, EpisodeSnapshotError, NewTrackingCommand, NewTrackingSubscription,
    TrackingApplication, TrackingApplicationError, TrackingScope, TrackingState, TrackingStore,
    TrackingSubscription, TrackingValidationError,
};
