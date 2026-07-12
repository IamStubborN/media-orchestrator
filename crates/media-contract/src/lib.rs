//! Versioned transport types shared by media clients and servers.

#![forbid(unsafe_code)]

mod actor;
mod error;
mod execution;
mod id;
mod job;
mod lease;
mod search;

pub use actor::{NotifyScopeDto, ProviderDto};
pub use error::{ApiError, ApiErrorCode};
pub use execution::{CheckpointValueDto, RunnerEventDto, RunnerEventRequest, RunnerEventResponse};
pub use id::PublicId;
pub use job::{
    CreateJobRequest, JobDto, JobListDto, JobStateDto, JobSummaryDto, NeedsActionReasonDto,
    QueueStatusDto,
};
pub use lease::LeaseDto;
pub use search::{
    ContinueSearchRequest, ExecutionSelectionDto, MAX_SEARCH_RESULTS_PER_PAGE, MediaKindDto,
    ProwlarrRankingDto, RezkaTranslationDto, SearchPageDto, SearchResultDto, SeasonAvailabilityDto,
    SelectResultRequest, SeriesAvailabilityDto, StartSearchRequest, TrackingPromptDto,
};
