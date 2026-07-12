//! Versioned transport types shared by media clients and servers.

#![forbid(unsafe_code)]

mod actor;
mod error;
mod execution;
mod id;
mod job;
mod lease;
mod lifecycle;
mod notification;
mod plex;
mod search;
mod tracking;

pub use actor::{NotifyScopeDto, ProviderDto};
pub use error::{ApiError, ApiErrorCode};
pub use execution::{CheckpointValueDto, RunnerEventDto, RunnerEventRequest, RunnerEventResponse};
pub use id::PublicId;
pub use job::{
    CreateJobRequest, JobDetailDto, JobDto, JobListDto, JobStateDto, JobSummaryDto,
    NeedsActionReasonDto, QueueStatusDto,
};
pub use lease::LeaseDto;
pub use lifecycle::{RunnerLifecycleDto, RunnerLifecycleStateDto, UpdateRunnerLifecycleRequest};
pub use notification::{HermesDeliverOnlyWebhook, NotificationEventTypeDto};
pub use plex::{
    PlexObservationDto, PlexReconcileRequest, PlexReconcileResponse, PlexReconcileStatus,
};
pub use search::{
    ContinueSearchRequest, ExecutionSelectionDto, MAX_SEARCH_RESULTS_PER_PAGE, MediaKindDto,
    ProwlarrRankingDto, RezkaSessionRefreshRequest, RezkaTranslationDto, SearchPageDto,
    SearchResultDto, SearchScopeDto, SeasonAvailabilityDto, SelectResultRequest,
    SeriesAvailabilityDto, SeriesLifecycleStatusDto, StartSearchRequest, TrackingPromptDto,
};
pub use tracking::{
    CreateTrackingRequest, EpisodeSnapshotDto, TrackingDto, TrackingListDto, TrackingScopeDto,
    TrackingStateDto,
};
