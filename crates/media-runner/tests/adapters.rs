use std::{path::PathBuf, sync::Arc};

use media_runner::{
    Cancellation, FileSystemPort, HttpPort, HttpRunnerServiceAdapter, PlexCheck, PlexExpectation,
    ProcessPort, ReqwestHttpAdapter, RunnerServicePort, SensitiveUrl, StorageRoots,
    TokioFileSystem, TokioProcessAdapter,
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

#[cfg(unix)]
#[tokio::test]
async fn process_adapter_parses_truthful_ffprobe_dimensions() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempdir().unwrap();
    let ffprobe = temporary.path().join("ffprobe-fixture");
    std::fs::write(
        &ffprobe,
        r#"#!/bin/sh
printf '%s' '{"streams":[{"codec_name":"h264","width":1280,"height":682,"bit_rate":"4000000"}],"format":{"duration":"61.25","bit_rate":"4100000"}}'
"#,
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

    assert_eq!((probe.width, probe.height), (1280, 682));
    assert_eq!(probe.codec, "h264");
    assert_eq!(probe.duration_seconds, 61.25);
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
