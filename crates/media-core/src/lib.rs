//! Pure domain types and policies for media orchestration.

#![forbid(unsafe_code)]

mod action;
mod actor;
mod application;
mod id;
mod identity;
mod job;
mod lifecycle;
mod metrics;
mod notification;
mod operation;
mod orchestration;
mod port;
mod release;
mod tracking;

pub use action::NeedsActionReason;
pub use actor::{
    Actor, ActorError, BootstrapClient, BootstrapClientError, ClientRole, CredentialDigest,
};
pub use application::{
    ApplicationError, JobApplication, LeaseApplication, LeaseTtlError, NewJobCommand,
    RunnerLifecycleApplication,
};
pub use id::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, ClientId, EpisodeId, JobEventId, JobId, LIFECYCLE_CLIENT_ID,
    LeaseId, MediaId, NotificationId, RUNNER_CLIENT_ID, SeasonId, TaskId, TrackingId, UserId,
    SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
pub use identity::{
    CanonicalEpisode, CanonicalEpisodeCoordinates, CanonicalMedia, CanonicalSeason,
    EpisodeMappingConfirmation, EpisodeProviderMapping, EpisodeResolution, ExternalNamespace,
    ExternalReference, IdentityValidationError, MappingSource, MediaExternalReference, MediaKind,
    SeriesOrdering, resolve_episode_candidates,
};
pub use job::{
    Job, JobDetail, JobLease, JobState, JobTransitionError, JobValidationError,
    MAX_RESULT_REF_BYTES, NewJob, NotifyScope, Provider, QueueStatus, TransferKind,
    TransferProgress,
};
pub use lifecycle::{RunnerLifecycle, RunnerLifecycleState, RunnerLifecycleUpdate};
pub use metrics::{MetricsSnapshot, MetricsSource};
pub use notification::{
    NotificationDelivery, NotificationDeliveryFailure, NotificationDispatchResult,
    NotificationDispatcher, NotificationEventType, NotificationOutboxPort, NotificationRecipient,
    NotificationSink, NotificationValidationError,
};
pub use operation::OperationKey;
pub use orchestration::{
    Checkpoint, CheckpointValue, JobEvent, JobEventKind, JobEventValidationError,
    MAX_STAGE_ATTEMPTS, StageFailureOutcome, StageRef,
};
pub use port::{
    ClientStore, IdentityStore, JobStore, LeaseStore, PortError, ReadinessPort,
    RunnerLifecycleStore,
};
pub use release::*;
pub use tracking::{
    EpisodeDiscoveryPort, EpisodeSnapshot, EpisodeSnapshotError, NewTrackingCommand,
    NewTrackingSubscription, TrackedEpisodeDownloadPort, TrackingApplication,
    TrackingApplicationError, TrackingDownload, TrackingRunResult, TrackingRuntime,
    TrackingScheduleStore, TrackingScope, TrackingState, TrackingStore, TrackingSubscription,
    TrackingValidationError,
};
