use media_contract::{
    CreateTrackingRequest, EpisodeSnapshotDto, HermesDeliverOnlyWebhook,
    HermesMediaNotificationWebhook, HermesSourceChoiceWebhook, MediaNotificationActionDto,
    MediaNotificationDeliveryKindDto, MediaNotificationDto, MediaNotificationEpisodeDto,
    MediaNotificationKindDto, MediaNotificationNextStepDto, MediaNotificationProgressDto,
    MediaNotificationStageDto, MediaNotificationStateDto, NotificationEventTypeDto, PublicId,
    SourceChoiceActionDto, TrackingScopeDto,
};

#[test]
fn tracking_create_request_has_strict_stable_shape_without_owner_or_download_flag() {
    let value = serde_json::json!({
        "provider": "rezka",
        "title": "Ongoing Show",
        "translation": "Studio Dub",
        "known_episodes": [{"season": 1, "episode": 4}],
        "scope": "family",
        "series_ongoing": true,
        "release_identity": {"source": "tvmaze", "source_id": 77}
    });
    let request: CreateTrackingRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(request.scope, TrackingScopeDto::Family);
    assert_eq!(request.release_identity.as_ref().unwrap().source_id, 77);
    assert_eq!(
        request.known_episodes,
        vec![EpisodeSnapshotDto {
            season: 1,
            episode: 4
        }]
    );
    assert_eq!(serde_json::to_value(request).unwrap(), value);

    for forbidden in ["owner_id", "requested_by", "auto_download"] {
        let mut invalid = value.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .insert(forbidden.to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<CreateTrackingRequest>(invalid).is_err());
    }
}

#[test]
fn tracking_release_identity_rejects_unknown_sources_and_fields() {
    let base = serde_json::json!({
        "provider": "rezka",
        "title": "Ongoing Show",
        "translation": "release-calendar",
        "known_episodes": [{"season": 1, "episode": 4}],
        "scope": "personal",
        "series_ongoing": true
    });
    for identity in [
        serde_json::json!({"source": "tmdb", "source_id": 77}),
        serde_json::json!({"source": "tvmaze", "source_id": 77, "extra": true}),
    ] {
        let mut value = base.clone();
        value["release_identity"] = identity;
        assert!(serde_json::from_value::<CreateTrackingRequest>(value).is_err());
    }
}

#[test]
fn notification_event_types_are_stable_and_complete() {
    let cases = [
        (NotificationEventTypeDto::Started, "started"),
        (NotificationEventTypeDto::ChoiceNeeded, "choice-needed"),
        (
            NotificationEventTypeDto::DownloadingStarted,
            "downloading-started",
        ),
        (
            NotificationEventTypeDto::DownloadProgress,
            "download-progress",
        ),
        (NotificationEventTypeDto::Downloaded, "downloaded"),
        (
            NotificationEventTypeDto::TranscodingStarted,
            "transcoding-started",
        ),
        (
            NotificationEventTypeDto::EncodingComplete,
            "encoding-complete",
        ),
        (NotificationEventTypeDto::PlexAdded, "plex-added"),
        (NotificationEventTypeDto::Completed, "completed"),
        (NotificationEventTypeDto::Partial, "partial"),
        (NotificationEventTypeDto::BlockedStorage, "blocked-storage"),
        (NotificationEventTypeDto::Failed, "failed"),
        (NotificationEventTypeDto::Cancelled, "cancelled"),
        (
            NotificationEventTypeDto::FutureEpisodeFound,
            "future-episode-found",
        ),
    ];
    for (event, name) in cases {
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            format!("\"{name}\"")
        );
    }
}

#[test]
fn hermes_payload_is_deliver_only_compatible_and_contains_no_routing_or_secret_fields() {
    let payload = HermesDeliverOnlyWebhook {
        event_type: "media.notification".to_owned(),
        status_key: None,
        message: "Episode 5 is now available. Choose Rezka or Prowlarr.".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(payload).unwrap(),
        serde_json::json!({
            "event_type": "media.notification",
            "message": "Episode 5 is now available. Choose Rezka or Prowlarr."
        })
    );
}

#[test]
fn hermes_source_choice_webhook_has_three_stable_provider_actions() {
    let tracking_id = PublicId::parse("00000000-0000-0000-0000-000000000555").unwrap();
    let payload = HermesSourceChoiceWebhook {
        event_type: "media.source-choice".to_owned(),
        schema_version: 1,
        card_key: "tracking:00000000-0000-0000-0000-000000000555:3:5".to_owned(),
        tracking_id,
        title: "Jobless Reincarnation".to_owned(),
        season: 3,
        episode: 5,
        actions: vec![
            SourceChoiceActionDto::All,
            SourceChoiceActionDto::Rezka,
            SourceChoiceActionDto::Prowlarr,
        ],
        poster_url: Some("https://static.tvmaze.com/poster.jpg".to_owned()),
        choice_set_id: None,
        choice_set_expires_at: None,
        rezka_count: None,
        prowlarr_count: None,
        season_complete: false,
    };

    assert_eq!(
        serde_json::to_value(payload).unwrap(),
        serde_json::json!({
            "event_type": "media.source-choice",
            "schema_version": 1,
            "card_key": "tracking:00000000-0000-0000-0000-000000000555:3:5",
            "tracking_id": "00000000-0000-0000-0000-000000000555",
            "title": "Jobless Reincarnation",
            "season": 3,
            "episode": 5,
            "actions": ["all", "rezka", "prowlarr"],
            "poster_url": "https://static.tvmaze.com/poster.jpg"
        })
    );
}

#[test]
fn hermes_source_choice_webhook_omits_false_season_complete_and_emits_true() {
    let tracking_id = PublicId::parse("00000000-0000-0000-0000-000000000555").unwrap();
    let mut payload = HermesSourceChoiceWebhook {
        event_type: "media.source-choice".to_owned(),
        schema_version: 1,
        card_key: "tracking:00000000-0000-0000-0000-000000000555:3:12".to_owned(),
        tracking_id,
        title: "Jobless Reincarnation".to_owned(),
        season: 3,
        episode: 12,
        actions: vec![SourceChoiceActionDto::Rezka],
        poster_url: None,
        choice_set_id: None,
        choice_set_expires_at: None,
        rezka_count: None,
        prowlarr_count: None,
        season_complete: false,
    };
    assert!(
        serde_json::to_value(&payload)
            .unwrap()
            .get("season_complete")
            .is_none()
    );

    payload.season_complete = true;
    assert_eq!(
        serde_json::to_value(payload).unwrap()["season_complete"],
        serde_json::json!(true)
    );
}

#[test]
fn hermes_media_notification_webhook_has_schema_version_two_shape() {
    let job_id = PublicId::parse("00000000-0000-0000-0000-000000000999").unwrap();
    let payload = HermesMediaNotificationWebhook {
        event_type: "media.notification".to_owned(),
        schema_version: 2,
        delivery_kind: MediaNotificationDeliveryKindDto::Card,
        card_key: "media-job:00000000-0000-0000-0000-000000000999".to_owned(),
        revision: 7,
        lifecycle_cycle: 1,
        terminal: false,
        state: MediaNotificationStateDto::Downloading,
        media: MediaNotificationDto {
            job_id,
            title: "Example Show".to_owned(),
            kind: MediaNotificationKindDto::Series,
            provider: "rezka".to_owned(),
            season: Some(1),
            translation: Some("AniLibria".to_owned()),
            origin: None,
            poster_url: None,
        },
        progress: Some(MediaNotificationProgressDto {
            completed_episodes: Some(7),
            total_episodes: Some(12),
            current_episode: Some(8),
            downloaded_bytes: Some(195_035_136),
            total_bytes: Some(688_385_900),
            download_speed_bps: Some(5_452_595),
            percentage: None,
            eta_seconds: Some(91),
            seeds: Some(3),
            peers: Some(1),
            source_state: Some("downloading".to_owned()),
            connection_attempt: None,
            connection_attempt_limit: None,
            vpn_rotation_pending: None,
            storage_available_bytes: None,
            storage_required_bytes: None,
            missing_episodes: vec![MediaNotificationEpisodeDto {
                season: 1,
                episode: 9,
            }],
        }),
        stage: Some(MediaNotificationStageDto::Download),
        next_step: Some(MediaNotificationNextStepDto::Process),
        issue: None,
        result: None,
        actions: vec![
            MediaNotificationActionDto::RetryMissing,
            MediaNotificationActionDto::ResumeStorage,
        ],
    };

    assert_eq!(
        serde_json::to_value(payload).unwrap(),
        serde_json::json!({
            "event_type": "media.notification",
            "schema_version": 2,
            "delivery_kind": "card",
            "card_key": "media-job:00000000-0000-0000-0000-000000000999",
            "revision": 7,
            "lifecycle_cycle": 1,
            "terminal": false,
            "state": "downloading",
            "media": {
                "job_id": "00000000-0000-0000-0000-000000000999",
                "title": "Example Show",
                "kind": "series",
                "provider": "rezka",
                "season": 1,
                "translation": "AniLibria"
            },
            "progress": {
                "completed_episodes": 7,
                "total_episodes": 12,
                "current_episode": 8,
                "downloaded_bytes": 195035136,
                "total_bytes": 688385900,
                "download_speed_bps": 5452595,
                "eta_seconds": 91,
                "seeds": 3,
                "peers": 1,
                "source_state": "downloading",
                "missing_episodes": [{"season": 1, "episode": 9}]
            },
            "stage": "download",
            "next_step": "process",
            "actions": ["retry-missing", "resume-storage"]
        })
    );
}

#[test]
fn media_notification_actions_use_exact_kebab_case_wire_tags() {
    assert_eq!(
        serde_json::to_value(vec![
            MediaNotificationActionDto::RetryMissing,
            MediaNotificationActionDto::ResumeStorage,
            MediaNotificationActionDto::SearchAlternative,
        ])
        .unwrap(),
        serde_json::json!(["retry-missing", "resume-storage", "search-alternative"])
    );
}

#[test]
fn detailed_result_serializes_as_optional_schema_v2_content() {
    let value = serde_json::json!({
        "event_type": "media.notification",
        "schema_version": 2,
        "delivery_kind": "card",
        "card_key": "media-job:00000000-0000-0000-0000-000000000999",
        "revision": 8,
        "lifecycle_cycle": 1,
        "terminal": true,
        "state": "completed",
        "media": {
            "job_id": "00000000-0000-0000-0000-000000000999",
            "title": "Клинки Хранителей",
            "kind": "series",
            "provider": "rezka",
            "season": 2,
            "translation": "AniLibria"
        },
        "progress": {
            "completed_episodes": 1,
            "total_episodes": 1,
            "current_episode": 8
        },
        "stage": "publish",
        "next_step": "none",
        "result": {
            "video": {"codec": "hevc", "profile": "Main", "width": 1920, "height": 1080},
            "audio": {
                "language": "rus",
                "codec": "aac",
                "channels": 2,
                "channel_layout": "stereo",
                "title": "AniLibria"
            },
            "subtitles": {"downloaded": 2, "missing": 0},
            "file_size_bytes": 440401920,
            "duration_seconds": 1421,
            "processing": {"mode": "vaapi-upscale", "elapsed_seconds": 252},
            "publication": {
                "library": "tv-shows",
                "title": "Клинки Хранителей",
                "season": 2,
                "episode": 8
            }
        },
        "actions": ["details"]
    });
    let payload: HermesMediaNotificationWebhook = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(payload).unwrap(), value);

    assert_eq!(value["progress"]["current_episode"], 8);
    assert_eq!(value["progress"]["total_episodes"], 1);
    assert_eq!(
        value["result"]["video"],
        serde_json::json!({
            "codec": "hevc",
            "profile": "Main",
            "width": 1920,
            "height": 1080
        })
    );
    assert_eq!(
        value["result"]["audio"],
        serde_json::json!({
            "language": "rus",
            "codec": "aac",
            "channels": 2,
            "channel_layout": "stereo",
            "title": "AniLibria"
        })
    );
    assert_eq!(
        value["result"]["subtitles"],
        serde_json::json!({"downloaded": 2, "missing": 0})
    );
    assert_eq!(
        value["result"]["processing"],
        serde_json::json!({"mode": "vaapi-upscale", "elapsed_seconds": 252})
    );
    assert_eq!(value["result"]["publication"]["episode"], 8);
}

#[test]
fn existing_schema_v2_payload_without_detailed_fields_remains_accepted() {
    let value = serde_json::json!({
        "event_type": "media.notification",
        "schema_version": 2,
        "delivery_kind": "card",
        "card_key": "media-job:00000000-0000-0000-0000-000000000999",
        "revision": 7,
        "lifecycle_cycle": 1,
        "terminal": false,
        "state": "downloading",
        "media": {
            "job_id": "00000000-0000-0000-0000-000000000999",
            "title": "Example Show",
            "kind": "series",
            "provider": "rezka"
        },
        "progress": {"completed_episodes": 0, "total_episodes": 1, "current_episode": 8},
        "actions": ["cancel"]
    });

    let payload: HermesMediaNotificationWebhook = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(payload).unwrap(), value);
}
