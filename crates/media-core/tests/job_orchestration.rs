use std::collections::BTreeMap;

use media_core::{
    CheckpointValue, JobEvent, JobEventId, JobState, NeedsActionReason, Provider,
    StageFailureOutcome, max_stage_attempts,
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
fn rezka_retries_twenty_times_while_prowlarr_keeps_the_default_limit() {
    let rezka_limit = max_stage_attempts(Provider::Rezka);
    assert_eq!(rezka_limit, 20);
    assert_eq!(
        StageFailureOutcome::for_attempt_with_limit(19, true, rezka_limit),
        StageFailureOutcome::Retry,
    );
    assert_eq!(
        StageFailureOutcome::for_attempt_with_limit(20, true, rezka_limit),
        StageFailureOutcome::Failed,
    );

    let prowlarr_limit = max_stage_attempts(Provider::Prowlarr);
    assert_eq!(prowlarr_limit, 3);
    assert_eq!(
        StageFailureOutcome::for_attempt_with_limit(3, true, prowlarr_limit),
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
