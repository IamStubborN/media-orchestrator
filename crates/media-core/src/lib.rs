//! Pure domain types and policies for media orchestration.

#![forbid(unsafe_code)]

mod action;
mod id;
mod identity;
mod job;

pub use action::NeedsActionReason;
pub use id::{EpisodeId, JobId, MediaId, SeasonId, TaskId, UserId};
pub use identity::{
    EpisodeResolution, ExternalNamespace, ExternalReference, MappingSource, SeriesOrdering,
    resolve_episode_candidates,
};
pub use job::{JobState, JobTransitionError};
