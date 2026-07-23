use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use media_runner::{
    Cancellation, FileSystemPort, HttpPort, HttpRunnerServiceAdapter, MediaTransferPort,
    MediaTransferRequest, PlexCheck, PlexExpectation, ProcessPort, ReqwestHttpAdapter,
    RunnerPortError, RunnerServicePort, SensitiveUrl, StageReporter, StorageRoots, TokioFileSystem,
    TokioProcessAdapter, TransferObservation, TransferSource, VideoSourceKind,
    YtDlpTransferAdapter,
};
use secrecy::SecretString;
use tempfile::tempdir;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

struct Active;

impl Cancellation for Active {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct SwitchableCancellation(Arc<AtomicBool>);

impl Cancellation for SwitchableCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct ProgressRecorder {
    observations: Mutex<Vec<TransferObservation>>,
}

#[async_trait]
impl StageReporter for ProgressRecorder {
    async fn stage_started(&self, _stage_name: &str) {}
    async fn stage_completed(&self, _stage_name: &str) {}

    async fn stage_progress(&self, _stage_name: &str, observation: TransferObservation) {
        self.observations.lock().unwrap().push(observation);
    }
}

#[tokio::test]
async fn filesystem_publication_is_atomic_and_never_replaces_existing_video() {
    let temporary = tempdir().unwrap();
    let source = temporary.path().join("staging/video.partial");
    let destination = temporary.path().join("plex/video.mkv");
    let filesystem = TokioFileSystem;
    filesystem
        .create_dir_all(source.parent().unwrap())
        .await
        .unwrap();
    filesystem
        .create_dir_all(destination.parent().unwrap())
        .await
        .unwrap();
    filesystem.write_atomic(&source, b"first").await.unwrap();
    filesystem
        .publish_atomic(&source, &destination)
        .await
        .unwrap();

    let second = temporary.path().join("staging/second.partial");
    filesystem.write_atomic(&second, b"second").await.unwrap();
    filesystem
        .publish_atomic(&second, &destination)
        .await
        .unwrap();

    assert_eq!(
        filesystem.read(&destination).await.unwrap().unwrap(),
        b"first"
    );
    assert_eq!(filesystem.read(&second).await.unwrap().unwrap(), b"second");
}

#[tokio::test]
async fn filesystem_prepares_storage_roots_on_one_filesystem() {
    let temporary = tempdir().unwrap();
    let roots = StorageRoots::new(
        temporary.path().join("staging"),
        temporary.path().join("tv"),
        temporary.path().join("movies"),
    )
    .unwrap();

    TokioFileSystem.prepare_storage_roots(&roots).await.unwrap();

    assert!(roots.staging().is_dir());
    assert!(roots.tv().is_dir());
    assert!(roots.movies().is_dir());
}

#[tokio::test]
async fn opt_in_ffmpeg_fixture_preserves_odd_source_dimensions_in_probe() {
    if std::env::var_os("MEDIA_RUN_FFMPEG_FIXTURE").is_none() {
        return;
    }
    let temporary = tempdir().unwrap();
    let fixture = temporary.path().join("fixture.mkv");
    let status = tokio::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=size=1280x682:rate=1",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-y",
        ])
        .arg(&fixture)
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let adapter = TokioProcessAdapter::new("ffprobe", "ffmpeg", std::time::Duration::from_secs(10));

    let probe = adapter.probe(&fixture, &Active).await.unwrap();

    assert_eq!((probe.width, probe.height), (1280, 682));
}

#[tokio::test]
async fn reqwest_adapter_resumes_only_from_matching_content_range() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/video"))
        .and(header("range", "bytes=4-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 4-7/8")
                .set_body_bytes(b"efgh"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let temporary = tempdir().unwrap();
    let partial = temporary.path().join("video.partial");
    let filesystem: Arc<dyn FileSystemPort> = Arc::new(TokioFileSystem);
    filesystem.write_atomic(&partial, b"abcd").await.unwrap();
    let adapter = ReqwestHttpAdapter::new(std::time::Duration::from_secs(5)).unwrap();
    let url =
        SensitiveUrl::parse(&format!("{}/video?token=hidden", server.uri()), "video").unwrap();

    adapter
        .download_video(&url, &partial, 4, filesystem.as_ref(), &Active)
        .await
        .unwrap();

    assert_eq!(
        filesystem.read(&partial).await.unwrap().unwrap(),
        b"abcdefgh"
    );
}

#[tokio::test]
async fn reqwest_adapter_reports_resumed_direct_download_progress() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/video"))
        .and(header("range", "bytes=4-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 4-7/8")
                .set_body_bytes(b"efgh"),
        )
        .mount(&server)
        .await;
    let temporary = tempdir().unwrap();
    let partial = temporary.path().join("video.partial");
    let filesystem: Arc<dyn FileSystemPort> = Arc::new(TokioFileSystem);
    filesystem.write_atomic(&partial, b"abcd").await.unwrap();
    let adapter = ReqwestHttpAdapter::new(std::time::Duration::from_secs(5)).unwrap();
    let url = SensitiveUrl::parse(&format!("{}/video", server.uri()), "video").unwrap();
    let reporter = ProgressRecorder::default();

    adapter
        .download_video_with_progress(
            &url,
            &partial,
            4,
            filesystem.as_ref(),
            &Active,
            media_runner::TransferProgressContext {
                total_bytes: Some(8),
                reporter: &reporter,
            },
        )
        .await
        .unwrap();

    let observations = reporter.observations.lock().unwrap();
    let final_observation = observations.last().expect("final progress observation");
    assert_eq!(final_observation.downloaded_bytes, Some(8));
    assert_eq!(final_observation.total_bytes, Some(8));
    assert_eq!(final_observation.progress_percent, Some(100));
    assert_eq!(final_observation.eta_seconds, Some(0));
    assert!(final_observation.final_observation);
}

#[tokio::test]
async fn reqwest_adapter_probes_video_size_with_a_single_byte_range() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/video"))
        .and(header("range", "bytes=0-0"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 0-0/734003200")
                .set_body_bytes(b"x"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let adapter = ReqwestHttpAdapter::new(std::time::Duration::from_secs(5)).unwrap();
    let url = SensitiveUrl::parse(&format!("{}/video", server.uri()), "video").unwrap();

    assert_eq!(adapter.probe_video_size(&url).await.unwrap(), 734_003_200);
}

#[tokio::test]
async fn video_source_statuses_preserve_expiry_and_retry_semantics() {
    for (status, expected) in [
        (403, RunnerPortError::SourceExpired),
        (410, RunnerPortError::SourceExpired),
        (429, RunnerPortError::SourceTransferTransient),
        (503, RunnerPortError::SourceTransferTransient),
        (404, RunnerPortError::SourceTransferRejected),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/video"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let adapter = ReqwestHttpAdapter::new(std::time::Duration::from_secs(5)).unwrap();
        let url = SensitiveUrl::parse(&format!("{}/video", server.uri()), "video").unwrap();

        assert_eq!(
            adapter.validate_video_source(&url).await.unwrap_err(),
            expected
        );
    }
}

#[tokio::test]
async fn ignored_range_restarts_only_the_current_partial_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/video"))
        .and(header("range", "bytes=4-"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"replacement"))
        .expect(1)
        .mount(&server)
        .await;
    let temporary = tempdir().unwrap();
    let partial = temporary.path().join("video.partial");
    let filesystem: Arc<dyn FileSystemPort> = Arc::new(TokioFileSystem);
    filesystem.write_atomic(&partial, b"old!").await.unwrap();
    let adapter = ReqwestHttpAdapter::new(std::time::Duration::from_secs(5)).unwrap();
    let url = SensitiveUrl::parse(&format!("{}/video", server.uri()), "video").unwrap();

    adapter
        .download_video(&url, &partial, 4, filesystem.as_ref(), &Active)
        .await
        .unwrap();

    assert_eq!(
        filesystem.read(&partial).await.unwrap().unwrap(),
        b"replacement"
    );
}

#[tokio::test]
async fn streaming_client_completes_transfers_that_exceed_a_total_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/video"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(300))
                .set_body_bytes(b"streamed-body".to_vec()),
        )
        .mount(&server)
        .await;
    let temporary = tempdir().unwrap();
    let partial = temporary.path().join("video.partial");
    let filesystem: Arc<dyn FileSystemPort> = Arc::new(TokioFileSystem);
    let url = SensitiveUrl::parse(&format!("{}/video", server.uri()), "video").unwrap();

    // A short total deadline aborts a transfer that runs longer than it.
    let total_deadline = ReqwestHttpAdapter::new(std::time::Duration::from_millis(50)).unwrap();
    assert!(
        total_deadline
            .download_video(&url, &partial, 0, filesystem.as_ref(), &Active)
            .await
            .is_err()
    );

    // The streaming client has no total deadline, so the same transfer completes.
    let streaming = ReqwestHttpAdapter::streaming(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    streaming
        .download_video(&url, &partial, 0, filesystem.as_ref(), &Active)
        .await
        .unwrap();

    assert_eq!(
        filesystem.read(&partial).await.unwrap().unwrap(),
        b"streamed-body"
    );
}

#[tokio::test]
async fn download_treats_a_fully_satisfied_partial_as_complete_on_416() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/video"))
        .and(header("range", "bytes=8-"))
        .respond_with(ResponseTemplate::new(416).insert_header("content-range", "bytes */8"))
        .expect(1)
        .mount(&server)
        .await;
    let temporary = tempdir().unwrap();
    let partial = temporary.path().join("video.partial");
    let filesystem: Arc<dyn FileSystemPort> = Arc::new(TokioFileSystem);
    filesystem
        .write_atomic(&partial, b"abcdefgh")
        .await
        .unwrap();
    let adapter = ReqwestHttpAdapter::streaming(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    let url = SensitiveUrl::parse(&format!("{}/video", server.uri()), "video").unwrap();

    adapter
        .download_video(&url, &partial, 8, filesystem.as_ref(), &Active)
        .await
        .unwrap();

    assert_eq!(
        filesystem.read(&partial).await.unwrap().unwrap(),
        b"abcdefgh"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn process_adapter_parses_truthful_ffprobe_dimensions() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempdir().unwrap();
    let ffprobe = temporary.path().join("ffprobe-fixture");
    let arguments = temporary.path().join("ffprobe-arguments");
    std::fs::write(
        &ffprobe,
        r#"#!/bin/sh
printf '%s\n' "$@" > 'ARGUMENTS_PATH'
printf '%s' '{"streams":[{"codec_type":"video","codec_name":"hevc","profile":"Main","width":1920,"height":1080,"bit_rate":"2100000"},{"codec_type":"audio","codec_name":"aac","channels":2,"channel_layout":"stereo","tags":{"language":"rus","title":"AniLibria"}}],"format":{"duration":"61.25","bit_rate":"4100000"}}'
"#
        .replace("ARGUMENTS_PATH", &arguments.display().to_string()),
    )
    .unwrap();
    std::fs::set_permissions(&ffprobe, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = TokioProcessAdapter::new(
        &ffprobe,
        "/usr/bin/false",
        std::time::Duration::from_secs(2),
    );

    let probe = adapter
        .probe(PathBuf::from("input.mkv").as_path(), &Active)
        .await
        .unwrap();

    assert_eq!((probe.width, probe.height), (1920, 1080));
    assert_eq!(probe.codec, "hevc");
    assert_eq!(probe.video_profile.as_deref(), Some("Main"));
    assert_eq!(probe.duration_seconds, 61.25);
    assert_eq!(probe.audio_language.as_deref(), Some("rus"));
    assert_eq!(probe.audio_title.as_deref(), Some("AniLibria"));
    assert_eq!(probe.audio_codec.as_deref(), Some("aac"));
    assert_eq!(probe.audio_channels, Some(2));
    assert_eq!(probe.audio_channel_layout.as_deref(), Some("stereo"));
    let arguments = std::fs::read_to_string(arguments).unwrap();
    assert!(arguments.contains("profile"));
    assert!(arguments.contains("channels"));
    assert!(arguments.contains("channel_layout"));
}

#[cfg(unix)]
#[tokio::test]
async fn yt_dlp_adapter_uses_resumable_retries_and_reports_machine_progress() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempdir().unwrap();
    let executable = temporary.path().join("yt-dlp-fixture");
    let arguments = temporary.path().join("arguments");
    std::fs::write(
        &executable,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$@" > '{}'
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output" ]; then shift; output=$1; fi
  shift
done
printf '__MEDIA_PROGRESS__\t1024\t2048\tNA\t512\t2\n'
printf 'video' > "$output"
"#,
            arguments.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = temporary.path().join("source.partial.mkv");
    let url = SensitiveUrl::parse(
        "https://cdn.example/playlist.m3u8?token=never-log-me",
        "episode-video",
    )
    .unwrap();
    let reporter = ProgressRecorder::default();
    let adapter = YtDlpTransferAdapter::new(&executable, std::time::Duration::from_secs(2));

    adapter
        .download(
            MediaTransferRequest {
                source_url: &url,
                source_kind: VideoSourceKind::Hls,
                output_path: &output,
            },
            &reporter,
            &Active,
        )
        .await
        .unwrap();

    assert_eq!(std::fs::read(&output).unwrap(), b"video");
    let arguments = std::fs::read_to_string(arguments).unwrap();
    for expected in [
        "--ignore-config",
        "--continue",
        "--retries",
        "20",
        "--fragment-retries",
        "--concurrent-fragments",
        "4",
        "--batch-file",
        "-",
    ] {
        assert!(arguments.lines().any(|argument| argument == expected));
    }
    assert!(arguments.contains("__MEDIA_PROGRESS__\t%(progress.downloaded_bytes)s"));
    assert!(!arguments.contains("never-log-me"));
    let observations = reporter.observations.lock().unwrap();
    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].source, TransferSource::Hls);
    assert_eq!(observations[0].downloaded_bytes, Some(1024));
    assert_eq!(observations[0].total_bytes, Some(2048));
    assert_eq!(observations[0].progress_percent, Some(50));
    assert_eq!(observations[0].download_speed_bps, Some(512));
    assert_eq!(observations[0].eta_seconds, Some(2));
    assert!(observations.last().unwrap().final_observation);
}

#[cfg(unix)]
#[tokio::test]
async fn yt_dlp_cancellation_terminates_the_process_group_without_waiting_for_children() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempdir().unwrap();
    let executable = temporary.path().join("yt-dlp-cancellation-fixture");
    std::fs::write(
        &executable,
        "#!/bin/sh\n(sleep 30) >&2 &\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = temporary.path().join("source.partial.mkv");
    let url = SensitiveUrl::parse("https://cdn.example/playlist.m3u8", "episode-video").unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation = SwitchableCancellation(cancelled.clone());
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancelled.store(true, Ordering::SeqCst);
    });
    let adapter = YtDlpTransferAdapter::new(&executable, std::time::Duration::from_secs(10));
    let started = std::time::Instant::now();

    let result = adapter
        .download(
            MediaTransferRequest {
                source_url: &url,
                source_kind: VideoSourceKind::Hls,
                output_path: &output,
            },
            &ProgressRecorder::default(),
            &cancellation,
        )
        .await;

    assert_eq!(result, Err(RunnerPortError::Cancelled));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "cancellation waited for a child process: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn service_adapter_requests_scan_and_decodes_exact_plex_observation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/runner/plex/reconcile"))
        .and(header("authorization", "Bearer runner-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "matched",
            "observation": {
                "path": "/plex/tv/Show/Season 01/Show - S01E01.mkv",
                "canonical_id": "tmdb:123",
                "season": 1,
                "episode": 1
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = HttpRunnerServiceAdapter::new(
        reqwest::Url::parse(&format!("{}/", server.uri())).unwrap(),
        SecretString::from("runner-secret"),
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    let expected = PlexExpectation {
        path: PathBuf::from("/plex/tv/Show/Season 01/Show - S01E01.mkv"),
        canonical_id: "tmdb:123".to_owned(),
        season: Some(1),
        episode: Some(1),
    };

    assert!(matches!(
        adapter.scan_and_verify(&expected).await.unwrap(),
        PlexCheck::Matched(observation) if observation.path == expected.path
    ));
    assert!(!format!("{adapter:?}").contains("runner-secret"));
}
