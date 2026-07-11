use std::collections::BTreeMap;

use media_core::{
    CheckpointValue, JobEvent, JobEventId, JobState, NeedsActionReason, StageFailureOutcome,
};

#[test]
fn retryable_stage_failure_retries_twice_then_fails_on_third_attempt() {
    assert_eq!(
        StageFailureOutcome::for_attempt(1, true),
        StageFailureOutcome::Retry,
    );
    assert_eq!(
        StageFailureOutcome::for_attempt(2, true),
        StageFailureOutcome::Retry,
    );
    assert_eq!(
        StageFailureOutcome::for_attempt(3, true),
        StageFailureOutcome::Failed,
    );
    assert_eq!(
        StageFailureOutcome::for_attempt(1, false),
        StageFailureOutcome::Failed,
    );
}

#[test]
fn job_event_validates_needs_action_reason_and_stage_identity() {
    let invalid = JobEvent::transition(JobEventId::new(), JobState::NeedsAction, None);
    assert!(invalid.is_err());

    let valid = JobEvent::transition(
        JobEventId::new(),
        JobState::NeedsAction,
        Some(NeedsActionReason::PlexMismatch),
    );
    assert!(valid.is_ok());

    let checkpoint = JobEvent::stage_checkpoint(
        JobEventId::new(),
        0,
        "download".to_owned(),
        1,
        BTreeMap::from([("downloaded_bytes".to_owned(), CheckpointValue::Unsigned(42))]),
    );
    assert!(checkpoint.is_ok());
}
