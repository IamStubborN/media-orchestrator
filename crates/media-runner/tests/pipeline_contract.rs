use std::path::PathBuf;

use media_runner::{
    GIB, MediaProbe, PeakEstimate, PlexExpectation, PlexObservation, ResumeAction,
    StoragePreflight, StorageRoots, build_rezka_vaapi_command, decide_resume,
    validate_plex_observation, validate_webvtt,
};

#[test]
fn storage_preflight_preserves_reserve_after_expected_peak() {
    let guard = StoragePreflight::new(20 * GIB);
    let estimate = PeakEstimate::new(8 * GIB, 5 * GIB, 0).unwrap();

    assert!(guard.check(33 * GIB, estimate).is_ok());
    assert_eq!(
        guard
            .check(33 * GIB - 1, estimate)
            .unwrap_err()
            .required_bytes(),
        33 * GIB
    );
}

#[test]
fn resumed_download_appends_only_for_matching_partial_response() {
    assert_eq!(
        decide_resume(4096, 206, Some((4096, Some(10_000)))).unwrap(),
        ResumeAction::Append
    );
    assert_eq!(
        decide_resume(4096, 200, None).unwrap(),
        ResumeAction::Restart
    );
    assert!(decide_resume(4096, 206, Some((0, Some(10_000)))).is_err());
}

#[test]
fn rezka_vaapi_command_preserves_actual_dimensions_without_scaling() {
    let probe = MediaProbe {
        codec: "h264".to_owned(),
        width: 1280,
        height: 682,
        duration_seconds: 1_234.5,
        bitrate: Some(4_000_000),
    };

    let command = build_rezka_vaapi_command(
        &PathBuf::from("/staging/source.partial"),
        &PathBuf::from("/staging/encoded.partial"),
        &PathBuf::from("/dev/dri/renderD129"),
        &probe,
    )
    .unwrap();

    assert_eq!(command.program(), "ffmpeg");
    assert!(
        command
            .args()
            .windows(2)
            .any(|args| args == ["-c:v", "hevc_vaapi"])
    );
    assert!(
        command
            .args()
            .windows(2)
            .any(|args| args == ["-vaapi_device", "/dev/dri/renderD129"])
    );
    assert!(!command.args().iter().any(|arg| arg.contains("scale")));
    assert!(
        !command
            .args()
            .iter()
            .any(|arg| arg.contains("1920") || arg.contains("1080"))
    );
}

#[test]
fn subtitle_validation_accepts_webvtt_and_rejects_html_or_empty_content() {
    assert!(validate_webvtt(b"WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n").is_ok());
    assert!(validate_webvtt(b"").is_err());
    assert!(validate_webvtt(b"<html>challenge</html>").is_err());
}

#[test]
fn plex_verification_requires_exact_path_and_canonical_episode_identity() {
    let expected = PlexExpectation {
        path: PathBuf::from("/media/rezka/tv/Show/Season 01/Show - S01E02.mkv"),
        canonical_id: "tmdb:123".to_owned(),
        season: Some(1),
        episode: Some(2),
    };
    let exact = PlexObservation {
        path: expected.path.clone(),
        canonical_id: expected.canonical_id.clone(),
        season: Some(1),
        episode: Some(2),
    };

    assert!(validate_plex_observation(&expected, &exact).is_ok());
    assert!(
        validate_plex_observation(
            &expected,
            &PlexObservation {
                episode: Some(3),
                ..exact
            }
        )
        .is_err()
    );
}

#[test]
fn storage_roots_require_absolute_non_overlapping_staging_and_plex_paths() {
    assert!(StorageRoots::new("/staging/rezka", "/plex/tv", "/plex/movies").is_ok());
    assert!(StorageRoots::new("/plex/tv/staging", "/plex/tv", "/plex/movies").is_err());
    assert!(StorageRoots::new("relative/staging", "/plex/tv", "/plex/movies").is_err());
}
