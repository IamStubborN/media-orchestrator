use std::collections::BTreeMap;

use crate::{JobEventId, JobState, NeedsActionReason, Provider};

pub const MAX_STAGE_ATTEMPTS: u32 = 3;
pub const MAX_REZKA_STAGE_ATTEMPTS: u32 = 20;

#[must_use]
pub const fn max_stage_attempts(provider: Provider) -> u32 {
    match provider {
        Provider::Rezka => MAX_REZKA_STAGE_ATTEMPTS,
        Provider::Prowlarr => MAX_STAGE_ATTEMPTS,
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CheckpointValue {
    String(String),
    Unsigned(u64),
    Bool(bool),
}

pub type Checkpoint = BTreeMap<String, CheckpointValue>;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StageFailureOutcome {
    Retry,
    Failed,
}

impl StageFailureOutcome {
    #[must_use]
    pub const fn for_attempt(attempt: u32, retryable: bool) -> Self {
        Self::for_attempt_with_limit(attempt, retryable, MAX_STAGE_ATTEMPTS)
    }

    #[must_use]
    pub const fn for_attempt_with_limit(attempt: u32, retryable: bool, max_attempts: u32) -> Self {
        if retryable && attempt < max_attempts {
            Self::Retry
        } else {
            Self::Failed
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct JobEvent {
    id: JobEventId,
    kind: JobEventKind,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum JobEventKind {
    Started,
    StageStarted(StageRef),
    StageCheckpoint {
        stage: StageRef,
        checkpoint: Checkpoint,
    },
    StageCompleted {
        stage: StageRef,
        checkpoint: Checkpoint,
    },
    StageFailed {
        stage: StageRef,
        retryable: bool,
        error_code: String,
    },
    JobTransition {
        state: JobState,
        needs_action_reason: Option<NeedsActionReason>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StageRef {
    task_ordinal: u32,
    name: String,
    ordinal: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum JobEventValidationError {
    #[error("stage name cannot be empty")]
    EmptyStageName,
    #[error("error code cannot be empty")]
    EmptyErrorCode,
    #[error("NeedsAction transition requires an action reason")]
    NeedsActionReasonRequired,
    #[error("job state {state:?} cannot have a NeedsAction reason")]
    UnexpectedNeedsActionReason { state: JobState },
}

impl StageRef {
    fn new(task_ordinal: u32, name: String, ordinal: u32) -> Result<Self, JobEventValidationError> {
        if name.trim().is_empty() {
            return Err(JobEventValidationError::EmptyStageName);
        }
        Ok(Self {
            task_ordinal,
            name,
            ordinal,
        })
    }

    #[must_use]
    pub const fn task_ordinal(&self) -> u32 {
        self.task_ordinal
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
}

impl JobEvent {
    #[must_use]
    pub const fn started(id: JobEventId) -> Self {
        Self {
            id,
            kind: JobEventKind::Started,
        }
    }

    pub fn stage_started(
        id: JobEventId,
        task_ordinal: u32,
        stage_name: String,
        stage_ordinal: u32,
    ) -> Result<Self, JobEventValidationError> {
        Ok(Self {
            id,
            kind: JobEventKind::StageStarted(StageRef::new(
                task_ordinal,
                stage_name,
                stage_ordinal,
            )?),
        })
    }

    pub fn stage_checkpoint(
        id: JobEventId,
        task_ordinal: u32,
        stage_name: String,
        stage_ordinal: u32,
        checkpoint: Checkpoint,
    ) -> Result<Self, JobEventValidationError> {
        Ok(Self {
            id,
            kind: JobEventKind::StageCheckpoint {
                stage: StageRef::new(task_ordinal, stage_name, stage_ordinal)?,
                checkpoint,
            },
        })
    }

    pub fn stage_completed(
        id: JobEventId,
        task_ordinal: u32,
        stage_name: String,
        stage_ordinal: u32,
        checkpoint: Checkpoint,
    ) -> Result<Self, JobEventValidationError> {
        Ok(Self {
            id,
            kind: JobEventKind::StageCompleted {
                stage: StageRef::new(task_ordinal, stage_name, stage_ordinal)?,
                checkpoint,
            },
        })
    }

    pub fn stage_failed(
        id: JobEventId,
        task_ordinal: u32,
        stage_name: String,
        stage_ordinal: u32,
        retryable: bool,
        error_code: String,
    ) -> Result<Self, JobEventValidationError> {
        if error_code.trim().is_empty() {
            return Err(JobEventValidationError::EmptyErrorCode);
        }
        Ok(Self {
            id,
            kind: JobEventKind::StageFailed {
                stage: StageRef::new(task_ordinal, stage_name, stage_ordinal)?,
                retryable,
                error_code,
            },
        })
    }

    pub fn transition(
        id: JobEventId,
        state: JobState,
        needs_action_reason: Option<NeedsActionReason>,
    ) -> Result<Self, JobEventValidationError> {
        match (state, needs_action_reason) {
            (JobState::NeedsAction, None) => {
                return Err(JobEventValidationError::NeedsActionReasonRequired);
            }
            (JobState::NeedsAction, Some(_)) | (_, None) => {}
            (_, Some(_)) => {
                return Err(JobEventValidationError::UnexpectedNeedsActionReason { state });
            }
        }
        Ok(Self {
            id,
            kind: JobEventKind::JobTransition {
                state,
                needs_action_reason,
            },
        })
    }

    #[must_use]
    pub const fn id(&self) -> JobEventId {
        self.id
    }

    #[must_use]
    pub const fn kind(&self) -> &JobEventKind {
        &self.kind
    }
}
