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
mod release;
mod search;
mod tracking;
mod trending;

pub use actor::{NotifyScopeDto, ProviderDto};
pub use error::{ApiError, ApiErrorCode};
pub use execution::{CheckpointValueDto, RunnerEventDto, RunnerEventRequest, RunnerEventResponse};
pub use id::PublicId;
pub use job::{
    CreateJobRequest, JobDetailDto, JobDto, JobListDto, JobStateDto, JobSummaryDto,
    NeedsActionReasonDto, QueueStatusDto, TransferKindDto, TransferProgressDto,
};
pub use lease::LeaseDto;
pub use lifecycle::{RunnerLifecycleDto, RunnerLifecycleStateDto, UpdateRunnerLifecycleRequest};
pub use notification::{
    HermesDeliverOnlyWebhook, HermesMediaNotificationWebhook, HermesSourceChoiceWebhook,
    MediaNotificationActionDto, MediaNotificationAudioDto, MediaNotificationDeliveryKindDto,
    MediaNotificationDto, MediaNotificationEpisodeDto, MediaNotificationIssueDto,
    MediaNotificationKindDto, MediaNotificationLibraryDto, MediaNotificationNextStepDto,
    MediaNotificationOriginDto, MediaNotificationProcessingDto, MediaNotificationProcessingModeDto,
    MediaNotificationProgressDto, MediaNotificationPublicationDto, MediaNotificationResultDto,
    MediaNotificationStageDto, MediaNotificationStateDto, MediaNotificationSubtitlesDto,
    MediaNotificationVideoDto, NotificationEventTypeDto, SourceChoiceActionDto,
};
pub use plex::{
    PlexObservationDto, PlexReconcileRequest, PlexReconcileResponse, PlexReconcileStatus,
};
pub use release::*;
pub use search::{
    AlternativeSearchRequest, AmbiguousEpisodeDto, ContinueSearchRequest, EpisodeCoordinateDto,
    EpisodeCoordinateMappingDto, EpisodeMappingActionDto, ExecutionSelectionDto,
    MAX_SEARCH_RESULTS_PER_PAGE, MediaKindDto, ProwlarrRankingDto, ResolveEpisodeMappingRequest,
    RezkaSessionRefreshRequest, RezkaTranslationDto, SearchPageDto, SearchResultDto,
    SearchScopeDto, SeasonAvailabilityDto, SelectResultRequest, SeriesAvailabilityDto,
    SeriesLifecycleStatusDto, StartSearchRequest, TrackingPromptDto,
};
pub use tracking::{
    CreateTrackingRequest, EpisodeSnapshotDto, PatchTrackingRequest, SetTrackingBaselineRequest,
    TrackingCheckStatusDto, TrackingDownloadDto, TrackingDto, TrackingListDto, TrackingScopeDto,
    TrackingStateDto,
};
pub use trending::{TrendingCategoryDto, TrendingItemDto, TrendingMediaTypeDto, TrendingPageDto};
