use media_contract::{
    CreateTrackingRequest, EpisodeSnapshotDto, HermesDeliverOnlyWebhook, NotificationEventTypeDto,
    TrackingScopeDto,
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
        (NotificationEventTypeDto::Downloaded, "downloaded"),
        (
            NotificationEventTypeDto::EncodingComplete,
            "encoding-complete",
        ),
        (NotificationEventTypeDto::PlexAdded, "plex-added"),
        (NotificationEventTypeDto::Partial, "partial"),
        (NotificationEventTypeDto::Failed, "failed"),
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
