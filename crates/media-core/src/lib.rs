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
    LeaseId, MediaId, NotificationId, RUNNER_CLIENT_ID, SeasonId, TaskId, TrackingClaimToken,
    TrackingId, UserId, SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
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
pub use lifecycle::{
    MAX_STICKY_VPN_ATTEMPTS, RunnerLifecycle, RunnerLifecycleState, RunnerLifecycleUpdate,
};
pub use metrics::{MetricsSnapshot, MetricsSource};
pub use notification::{
    MediaNotification, MediaNotificationAction, MediaNotificationAudio,
    MediaNotificationDeliveryKind, MediaNotificationEpisode, MediaNotificationIssue,
    MediaNotificationKind, MediaNotificationLibrary, MediaNotificationMedia,
    MediaNotificationNextStep, MediaNotificationOrigin, MediaNotificationProcessing,
    MediaNotificationProcessingMode, MediaNotificationProgress, MediaNotificationPublication,
    MediaNotificationResult, MediaNotificationStage, MediaNotificationState,
    MediaNotificationSubtitles, MediaNotificationVideo, NotificationContent, NotificationDelivery,
    NotificationDeliveryFailure, NotificationDeliveryFence, NotificationDeliveryPermit,
    NotificationDispatchResult, NotificationDispatcher, NotificationEventType,
    NotificationOutboxPort, NotificationRecipient, NotificationSink, NotificationSinkOutcome,
    NotificationValidationError, SourceChoiceAction, SourceChoiceNotification,
};
pub use operation::OperationKey;
pub use orchestration::{
    Checkpoint, CheckpointValue, JobEvent, JobEventKind, JobEventValidationError,
    MAX_REZKA_STAGE_ATTEMPTS, MAX_STAGE_ATTEMPTS, StageFailureOutcome, StageRef,
    max_stage_attempts,
};
pub use port::{
    ClientStore, IdentityStore, JobStore, LeaseStore, PortError, ReadinessPort,
    RunnerLifecycleStore,
};
pub use release::*;
pub use tracking::{
    AnonymousSessionPort, ENQUEUE_FAILURE_CODE, ENQUEUE_SEARCH_FAILURE_CODE, ENQUEUE_VERIFY_FAILURE_CODE, ENQUEUE_PERSIST_FAILURE_CODE, ENQUEUE_JOB_FAILURE_CODE, EpisodeAvailability, EpisodeAvailabilityPort,
    EpisodeAvailabilityRequest, EpisodeDiscovery, EpisodeDiscoveryPort, EpisodeSnapshot,
    EpisodeSnapshotError, FutureEpisodeRecord, NewTrackingCommand, NewTrackingSubscription,
    ProviderAvailability, RELEASE_CONFLICT_FAILURE_CODE, RELEASE_INFRASTRUCTURE_FAILURE_CODE,
    SOURCE_PROBE_FAILURE_CODE, SOURCE_UNAVAILABLE_CODE, TrackedEpisodeDownloadPort,
    TrackingApplication, TrackingApplicationError, TrackingCheckOutcome, TrackingCheckStatus,
    TrackingDownload, TrackingDownloadPatch, TrackingRunResult, TrackingRuntime,
    TrackingScheduleStore, TrackingScope, TrackingState, TrackingStore, TrackingSubscription,
    TrackingValidationError, episode_choice_set_id, is_valid_tracking_poster_url,
    next_check_failure_count, tracking_failure_cooldown,
};
