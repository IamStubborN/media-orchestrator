use media_contract::{
    ReleaseLifecycleDto, ReleasePrecisionDto, ReleaseQueryRequest, ReleaseQueryResponse,
};

#[test]
fn choice_needed_response_has_explicit_candidates() {
    let response: ReleaseQueryResponse = serde_json::from_value(serde_json::json!({
        "status": "choice_needed",
        "source": "tvmaze",
        "fetched_at": "2026-07-13T12:00:00Z",
        "candidates": [{
            "source_id": 42,
            "title": "The Office",
            "original_title": null,
            "year": 2005,
            "lifecycle": "ended"
        }]
    }))
    .expect("choice response should deserialize");

    assert!(matches!(
        response,
        ReleaseQueryResponse::ChoiceNeeded { .. }
    ));
}

#[test]
fn release_query_rejects_unknown_fields() {
    let result = serde_json::from_value::<ReleaseQueryRequest>(serde_json::json!({
        "title": "Severance",
        "unexpected": true
    }));

    assert!(result.is_err());
}

#[test]
fn release_query_preserves_exact_source_identity() {
    let request: ReleaseQueryRequest = serde_json::from_value(serde_json::json!({
        "title": "Lucky",
        "source_id": 81228
    }))
    .expect("source identity should deserialize");

    assert_eq!(request.source_id, Some(81228));
}

#[test]
fn release_enums_use_stable_snake_case_values() {
    assert_eq!(
        serde_json::to_value(ReleasePrecisionDto::Date).unwrap(),
        "date"
    );
    assert_eq!(
        serde_json::to_value(ReleaseLifecycleDto::Ongoing).unwrap(),
        "ongoing"
    );
}
