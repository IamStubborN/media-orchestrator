use media_contract::{
    CreateTrackingRequest, EpisodeSnapshotDto, HermesDeliverOnlyWebhook,
    HermesMediaNotificationWebhook, MediaNotificationActionDto, MediaNotificationDeliveryKindDto,
    MediaNotificationDto, MediaNotificationEpisodeDto, MediaNotificationKindDto,
    MediaNotificationNextStepDto, MediaNotificationProgressDto, MediaNotificationStageDto,
    MediaNotificationStateDto, NotificationEventTypeDto, PublicId, TrackingScopeDto,
};

#[test]
fn tracking_create_request_has_strict_stable_shape_without_owner_or_download_flag() {
    let value = serde_json::json!({
        "provider": "rezka",
        "title": "Ongoing Show",
        "translation": "Studio Dub",
        "known_episodes": [{"season": 1, "episode": 4}],
        "scope": "family",
        "series_ongoing": true
    });
    let request: CreateTrackingRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(request.scope, TrackingScopeDto::Family);
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
        (
            NotificationEventTypeDto::SessionRefreshed,
            "session-refreshed",
        ),
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
        },
        progress: Some(MediaNotificationProgressDto {
            completed_episodes: Some(7),
            total_episodes: Some(12),
            current_episode: Some(8),
            downloaded_bytes: Some(195_035_136),
            download_speed_bps: Some(5_452_595),
            percentage: None,
            missing_episodes: vec![MediaNotificationEpisodeDto {
                season: 1,
                episode: 9,
            }],
        }),
        stage: Some(MediaNotificationStageDto::Download),
        next_step: Some(MediaNotificationNextStepDto::Process),
        issue: None,
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
                "download_speed_bps": 5452595,
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
        ])
        .unwrap(),
        serde_json::json!(["retry-missing", "resume-storage"])
    );
}
