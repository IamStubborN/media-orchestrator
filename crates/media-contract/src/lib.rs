//! Versioned transport types shared by media clients and servers.

#![forbid(unsafe_code)]

mod actor;
mod error;
mod execution;
mod id;
mod job;
mod lease;
mod notification;
mod tracking;

pub use actor::{NotifyScopeDto, ProviderDto};
pub use error::{ApiError, ApiErrorCode};
pub use execution::{CheckpointValueDto, RunnerEventDto, RunnerEventRequest, RunnerEventResponse};
pub use id::PublicId;
pub use job::{
    CreateJobRequest, JobDto, JobListDto, JobStateDto, JobSummaryDto, NeedsActionReasonDto,
    QueueStatusDto,
};
pub use lease::LeaseDto;
pub use notification::{HermesDeliverOnlyWebhook, NotificationEventTypeDto};
pub use tracking::{
    CreateTrackingRequest, EpisodeSnapshotDto, TrackingDto, TrackingListDto, TrackingScopeDto,
    TrackingStateDto,
};
