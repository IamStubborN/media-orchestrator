//! Versioned transport types shared by media clients and servers.

#![forbid(unsafe_code)]

mod actor;
mod error;
mod id;
mod job;
mod lease;

pub use actor::{NotifyScopeDto, ProviderDto};
pub use error::{ApiError, ApiErrorCode};
pub use id::PublicId;
pub use job::{
    CreateJobRequest, JobDto, JobStateDto, JobSummaryDto, NeedsActionReasonDto, QueueStatusDto,
};
pub use lease::LeaseDto;
