use std::collections::BTreeMap;

use crate::{JobDto, JobStateDto, NeedsActionReasonDto, PublicId};

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CheckpointValueDto {
    String(String),
    Unsigned(u64),
    Bool(bool),
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum RunnerEventDto {
    Started,
    StageStarted {
        task_ordinal: u32,
        stage_name: String,
        stage_ordinal: u32,
    },
    StageCheckpoint {
        task_ordinal: u32,
        stage_name: String,
        stage_ordinal: u32,
        checkpoint: BTreeMap<String, CheckpointValueDto>,
    },
    StageCompleted {
        task_ordinal: u32,
        stage_name: String,
        stage_ordinal: u32,
        checkpoint: BTreeMap<String, CheckpointValueDto>,
    },
    StageFailed {
        task_ordinal: u32,
        stage_name: String,
        stage_ordinal: u32,
        retryable: bool,
        error_code: String,
    },
    JobTransition {
        state: JobStateDto,
        #[serde(skip_serializing_if = "Option::is_none")]
        needs_action_reason: Option<NeedsActionReasonDto>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerEventRequest {
    pub event_id: PublicId,
    pub event: RunnerEventDto,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunnerEventResponse {
    pub job: JobDto,
}
