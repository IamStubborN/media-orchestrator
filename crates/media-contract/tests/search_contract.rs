use media_contract::{
    ContinueSearchRequest, ExecutionSelectionDto, MediaKindDto, ProwlarrRankingDto, SearchPageDto,
    SearchResultDto, SelectResultRequest, StartSearchRequest,
};

#[test]
fn search_contract_is_versioned_paginated_and_safe() {
    let page = SearchPageDto {
        api_version: "v1".to_owned(),
        session_id: "0190d8d2-a73a-7fb0-9d57-e520abfca111".to_owned(),
        source: media_contract::ProviderDto::Prowlarr,
        expires_at: "2026-07-13T12:00:00Z".to_owned(),
        results: (0..5)
            .map(|index| SearchResultDto::Prowlarr {
                result_id: format!("result-{index}"),
                title: format!("Movie {index}"),
                indexer: Some("Indexer".to_owned()),
                size_bytes: 1_000 + index,
                seeders: 20,
                release_group: Some("GROUP".to_owned()),
                ranking: ProwlarrRankingDto {
                    exact_title: true,
                    exact_season: true,
                    quality_preference: 4,
                    language_preference: 3,
                    seeders: 20,
                    size_bytes: 1_000 + index,
                    codec_preference: 2,
                    release_group_preference: 1,
                },
            })
            .collect(),
        continuation: Some("0190d8d2-a73a-7fb0-9d57-e520abfca111:5".to_owned()),
    };

    let value = serde_json::to_value(&page).unwrap();
    assert_eq!(value["api_version"], "v1");
    assert_eq!(value["results"].as_array().unwrap().len(), 5);
    let rendered = serde_json::to_string(&value).unwrap();
    for forbidden in ["magnet", "download_url", "stream_url", "token=secret"] {
        assert!(
            !rendered.contains(forbidden),
            "public result leaked {forbidden}"
        );
    }
}

#[test]
fn start_and_continue_reject_caller_identity_fields() {
    for value in [
        serde_json::json!({"source":"rezka","query":"Arrival","requested_by":"user"}),
        serde_json::json!({"continuation":"session:5","owner_id":"user"}),
    ] {
        let error = if value.get("source").is_some() {
            serde_json::from_value::<StartSearchRequest>(value)
                .unwrap_err()
                .to_string()
        } else {
            serde_json::from_value::<ContinueSearchRequest>(value)
                .unwrap_err()
                .to_string()
        };
        assert!(error.contains("unknown field"), "unexpected error: {error}");
    }
}

#[test]
fn selection_is_explicit_and_rejects_identity_fields() {
    let request = SelectResultRequest {
        session_id: "0190d8d2-a73a-7fb0-9d57-e520abfca111".to_owned(),
        result_id: "result-2".to_owned(),
        translation_id: Some(37),
        season: Some(1),
        episode: Some(4),
        scope: media_contract::SearchScopeDto {
            platform: "telegram".to_owned(),
            chat_id: "42".to_owned(),
            thread_id: None,
        },
    };
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        serde_json::json!({
            "session_id":"0190d8d2-a73a-7fb0-9d57-e520abfca111",
            "result_id":"result-2",
            "translation_id":37,
            "season":1,
            "episode":4,
            "scope":{"platform":"telegram","chat_id":"42"}
        })
    );

    let invalid = serde_json::json!({
        "session_id":"session",
        "result_id":"result",
        "requested_by":"someone"
    });
    assert!(
        serde_json::from_value::<SelectResultRequest>(invalid)
            .unwrap_err()
            .to_string()
            .contains("unknown field")
    );
}

#[test]
fn rezka_series_exposes_translation_and_tracking_prompt_without_urls() {
    let result = SearchResultDto::Rezka {
        result_id: "rezka-1".to_owned(),
        title: "Ongoing Show".to_owned(),
        original_title: Some("Ongoing Show".to_owned()),
        year: Some(2026),
        media_kind: MediaKindDto::Series,
        thumbnail_url: Some("https://img.invalid/poster.jpg".to_owned()),
        translations: vec![media_contract::RezkaTranslationDto {
            id: 37,
            name: "Original".to_owned(),
            premium: false,
            director: false,
            camrip: false,
            has_ads: false,
        }],
        availability: Some(media_contract::SeriesAvailabilityDto {
            lifecycle_status: media_contract::SeriesLifecycleStatusDto::Ongoing,
            incomplete: true,
            seasons: vec![media_contract::SeasonAvailabilityDto {
                season: 1,
                episodes: vec![1, 2, 3, 4],
            }],
            tracking_prompt: Some(media_contract::TrackingPromptDto {
                title: "Ongoing Show".to_owned(),
                latest_season: 1,
                latest_episode: 4,
            }),
        }),
    };

    let value = serde_json::to_value(result).unwrap();
    assert_eq!(value["translations"][0]["id"], 37);
    assert_eq!(value["availability"]["incomplete"], true);
    assert_eq!(value["availability"]["lifecycle_status"], "ongoing");
    assert!(value["availability"]["tracking_prompt"].is_object());
    assert!(!serde_json::to_string(&value).unwrap().contains("stream"));
}

#[test]
fn runner_execution_payload_is_separate_from_public_search_results() {
    let execution = ExecutionSelectionDto::Prowlarr {
        source_identity: "1:guid:42".to_owned(),
        info_hash: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        uri: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".to_owned(),
        media_kind: MediaKindDto::Movie,
        season: None,
        title: "Exact Release".to_owned(),
    };
    let value = serde_json::to_value(execution).unwrap();
    assert_eq!(value["source"], "prowlarr");
    assert_eq!(value["media_kind"], "movie");
    assert!(value.get("season").is_none());
    assert!(value["uri"].as_str().unwrap().starts_with("magnet:"));
}

#[test]
fn legacy_prowlarr_execution_payloads_remain_deserializable() {
    let execution: ExecutionSelectionDto = serde_json::from_value(serde_json::json!({
        "source": "prowlarr",
        "source_identity": "1:guid:42",
        "info_hash": "0123456789abcdef0123456789abcdef01234567",
        "uri": "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
        "title": "Legacy Release"
    }))
    .unwrap();

    assert!(matches!(
        execution,
        ExecutionSelectionDto::Prowlarr {
            media_kind: MediaKindDto::Movie,
            season: None,
            ..
        }
    ));
}
