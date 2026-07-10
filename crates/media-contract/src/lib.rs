//! Versioned transport types shared by media clients and servers.

#![forbid(unsafe_code)]

mod error;
mod id;
mod job;

pub use error::{ApiError, ApiErrorCode};
pub use id::PublicId;
pub use job::{JobStateDto, JobSummaryDto, NeedsActionReasonDto};
