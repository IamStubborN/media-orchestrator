use std::collections::BTreeMap;

use media_contract::{
    CheckpointValueDto, JobListDto, JobStateDto, NeedsActionReasonDto, PublicId, RunnerEventDto,
    RunnerEventRequest,
};

fn id(value: &str) -> PublicId {
    PublicId::parse(value).unwrap()
}

#[test]
fn job_list_has_a_stable_json_envelope() {
    let value = serde_json::to_value(JobListDto { jobs: Vec::new() }).unwrap();
    assert_eq!(value, serde_json::json!({"jobs": []}));
}

#[test]
fn checkpoint_event_round_trips_typed_checkpoint_values() {
    let request = RunnerEventRequest {
        event_id: id("018f3f86-7b4c-7b4f-9b6a-6d62f45bb120"),
        event: RunnerEventDto::StageCheckpoint {
            task_ordinal: 2,
            stage_name: "download".to_owned(),
            stage_ordinal: 1,
            checkpoint: BTreeMap::from([
                (
                    "downloaded_bytes".to_owned(),
                    CheckpointValueDto::Unsigned(4096),
                ),
                ("range_supported".to_owned(), CheckpointValueDto::Bool(true)),
                (
                    "etag".to_owned(),
                    CheckpointValueDto::String("abc".to_owned()),
                ),
            ]),
        },
    };

    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "event_id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb120",
            "event": {
                "type": "stage_checkpoint",
                "task_ordinal": 2,
                "stage_name": "download",
                "stage_ordinal": 1,
                "checkpoint": {
                    "downloaded_bytes": 4096,
                    "range_supported": true,
                    "etag": "abc"
                }
            }
        }),
    );
    assert_eq!(
        serde_json::from_value::<RunnerEventRequest>(value).unwrap(),
        request
    );
}

#[test]
fn terminal_and_needs_action_events_are_explicit() {
    let failed: RunnerEventDto = serde_json::from_value(serde_json::json!({
        "type": "stage_failed",
        "task_ordinal": 0,
        "stage_name": "resolve",
        "stage_ordinal": 0,
        "retryable": false,
        "error_code": "provider_forbidden"
    }))
    .unwrap();
    assert!(matches!(
        failed,
        RunnerEventDto::StageFailed {
            retryable: false,
            ..
        }
    ));

    let action = RunnerEventDto::JobTransition {
        state: JobStateDto::NeedsAction,
        needs_action_reason: Some(NeedsActionReasonDto::IdentityAmbiguous),
    };
    assert_eq!(
        serde_json::to_value(action).unwrap(),
        serde_json::json!({
            "type": "job_transition",
            "state": "needs_action",
            "needs_action_reason": "identity_ambiguous"
        }),
    );
}
