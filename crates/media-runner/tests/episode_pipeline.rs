use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use media_runner::{
    Cancellation, EpisodeOutcome, EpisodePipeline, EpisodeWork, FileSystemPort, GIB, HttpPort,
    MediaProbe, MediaTransferPort, MediaTransferRequest, PlexCheck, PlexExpectation,
    PlexObservation, ProcessCommand, ProcessPort, ProviderKind, RunnerPortError, RunnerServicePort,
    SensitiveUrl, StageReporter, SubtitleTrack, VideoSourceKind,
};

#[derive(Default)]
struct RecordingReporter {
    events: Mutex<Vec<String>>,
}

#[async_trait]
impl StageReporter for RecordingReporter {
    async fn stage_started(&self, stage_name: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("started:{stage_name}"));
    }

    async fn stage_completed(&self, stage_name: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("completed:{stage_name}"));
    }
}

#[derive(Default)]
struct NeverCancelled;

impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct FakeFs {
    files: Mutex<HashMap<PathBuf, Vec<u8>>>,
    published: Mutex<Vec<PathBuf>>,
    available: u64,
}

impl FakeFs {
    fn with_available(available: u64) -> Self {
        Self {
            available,
            ..Self::default()
        }
    }
}

#[async_trait]
impl FileSystemPort for FakeFs {
    async fn available_bytes(&self, _path: &Path) -> Result<u64, RunnerPortError> {
        Ok(self.available)
    }

    async fn file_len(&self, path: &Path) -> Result<Option<u64>, RunnerPortError> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .get(path)
            .map(|value| value.len() as u64))
    }

    async fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, RunnerPortError> {
        Ok(self.files.lock().unwrap().get(path).cloned())
    }

    async fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), RunnerPortError> {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_owned(), contents.to_vec());
        Ok(())
    }

    async fn write_chunk(
        &self,
        path: &Path,
        offset: u64,
        contents: &[u8],
    ) -> Result<(), RunnerPortError> {
        let mut files = self.files.lock().unwrap();
        let file = files.entry(path.to_owned()).or_default();
        if offset == 0 {
            file.clear();
        } else if file.len() as u64 != offset {
            return Err(RunnerPortError::Filesystem);
        }
        file.extend_from_slice(contents);
        Ok(())
    }

    async fn create_dir_all(&self, _path: &Path) -> Result<(), RunnerPortError> {
        Ok(())
    }

    async fn publish_atomic(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), RunnerPortError> {
        let mut files = self.files.lock().unwrap();
        if files.contains_key(destination) {
            return Ok(());
        }
        let contents = files.remove(source).ok_or(RunnerPortError::Filesystem)?;
        files.insert(destination.to_owned(), contents);
        self.published.lock().unwrap().push(destination.to_owned());
        Ok(())
    }

    async fn replace_atomic(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), RunnerPortError> {
        let mut files = self.files.lock().unwrap();
        let contents = files.remove(source).ok_or(RunnerPortError::Filesystem)?;
        files.insert(destination.to_owned(), contents);
        self.published.lock().unwrap().push(destination.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct FakeHttp {
    video_size: Option<u64>,
    probe_fails: bool,
    video_failures: Mutex<VecDeque<RunnerPortError>>,
    video_offsets: Mutex<Vec<u64>>,
    subtitle_failures: Mutex<HashSet<String>>,
    subtitle_requests: Mutex<Vec<String>>,
    cancel_video: bool,
}

#[async_trait]
impl HttpPort for FakeHttp {
    async fn probe_video_size(&self, _url: &SensitiveUrl) -> Result<u64, RunnerPortError> {
        if self.probe_fails {
            return Err(RunnerPortError::Http);
        }
        Ok(self.video_size.unwrap_or(2 * GIB))
    }

    async fn download_video(
        &self,
        _url: &SensitiveUrl,
        partial_path: &Path,
        resume_from: u64,
        filesystem: &dyn FileSystemPort,
        _cancellation: &dyn Cancellation,
    ) -> Result<(), RunnerPortError> {
        if let Some(error) = self.video_failures.lock().unwrap().pop_front() {
            return Err(error);
        }
        if self.cancel_video {
            return Err(RunnerPortError::Cancelled);
        }
        self.video_offsets.lock().unwrap().push(resume_from);
        filesystem.write_atomic(partial_path, b"video").await
    }

    async fn fetch_subtitle(
        &self,
        url: &SensitiveUrl,
        _cancellation: &dyn Cancellation,
    ) -> Result<Vec<u8>, RunnerPortError> {
        let id = url.redacted_id().to_owned();
        self.subtitle_requests.lock().unwrap().push(id.clone());
        if self.subtitle_failures.lock().unwrap().remove(&id) {
            return Err(RunnerPortError::Http);
        }
        Ok(b"WEBVTT\n\n00:00.000 --> 00:01.000\ntext\n".to_vec())
    }
}

#[async_trait]
impl MediaTransferPort for FakeHttp {
    async fn download(
        &self,
        _request: MediaTransferRequest<'_>,
        _reporter: &dyn StageReporter,
        _cancellation: &dyn Cancellation,
    ) -> Result<(), RunnerPortError> {
        if let Some(error) = self.video_failures.lock().unwrap().pop_front() {
            return Err(error);
        }
        if self.cancel_video {
            return Err(RunnerPortError::Cancelled);
        }
        self.video_offsets.lock().unwrap().push(0);
        Ok(())
    }
}

struct FakeProcess {
    probes: Mutex<VecDeque<MediaProbe>>,
    commands: Mutex<Vec<ProcessCommand>>,
    filesystem: Arc<FakeFs>,
}

#[async_trait]
impl ProcessPort for FakeProcess {
    async fn probe(
        &self,
        _path: &Path,
        _cancellation: &dyn Cancellation,
    ) -> Result<MediaProbe, RunnerPortError> {
        self.probes
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(RunnerPortError::Process)
    }

    async fn run(
        &self,
        command: &ProcessCommand,
        _cancellation: &dyn Cancellation,
    ) -> Result<(), RunnerPortError> {
        self.commands.lock().unwrap().push(command.clone());
        let output = command.args().last().ok_or(RunnerPortError::Process)?;
        self.filesystem
            .files
            .lock()
            .unwrap()
            .insert(PathBuf::from(output), b"encoded".to_vec());
        Ok(())
    }
}

struct FakeService {
    checks: Mutex<VecDeque<PlexCheck>>,
    scans: Mutex<Vec<PlexExpectation>>,
}

#[async_trait]
impl RunnerServicePort for FakeService {
    async fn scan_and_verify(
        &self,
        expectation: &PlexExpectation,
    ) -> Result<PlexCheck, RunnerPortError> {
        self.scans.lock().unwrap().push(expectation.clone());
        self.checks
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(RunnerPortError::Service)
    }
}

fn probe(codec: &str) -> MediaProbe {
    MediaProbe {
        codec: codec.to_owned(),
        width: 1280,
        height: 682,
        duration_seconds: 60.0,
        bitrate: Some(1_000_000),
        audio_language: None,
        audio_title: None,
    }
}

fn encoded_probe() -> MediaProbe {
    MediaProbe {
        codec: "hevc".to_owned(),
        width: 1920,
        height: 1080,
        duration_seconds: 60.0,
        bitrate: Some(1_000_000),
        audio_language: None,
        audio_title: None,
    }
}

fn work() -> EpisodeWork {
    EpisodeWork {
        provider: ProviderKind::Rezka,
        job_id: "job-1".to_owned(),
        episode_id: "s01e01".to_owned(),
        source_url: Some(
            SensitiveUrl::parse("https://cdn.invalid/video?token=secret", "video").unwrap(),
        ),
        source_kind: VideoSourceKind::Mp4,
        staging_directory: PathBuf::from("/staging/job-1/s01e01"),
        source_partial: PathBuf::from("/staging/job-1/s01e01/source.partial"),
        encoded_partial: PathBuf::from("/staging/job-1/s01e01/encoded.partial.mkv"),
        final_video: PathBuf::from("/plex/tv/Show/Season 01/Show - S01E01.mkv"),
        vaapi_device: PathBuf::from("/dev/dri/renderD128"),
        expected_duration_seconds: None,
        audio: None,
        subtitles: vec![
            SubtitleTrack {
                id: "en".to_owned(),
                url: SensitiveUrl::parse("https://cdn.invalid/en.vtt?token=secret", "en").unwrap(),
                staging_path: PathBuf::from("/staging/job-1/s01e01/Show.en.vtt.partial"),
                final_path: PathBuf::from("/plex/tv/Show/Season 01/Show - S01E01.en.vtt"),
            },
            SubtitleTrack {
                id: "uk".to_owned(),
                url: SensitiveUrl::parse("https://cdn.invalid/uk.vtt?token=secret", "uk").unwrap(),
                staging_path: PathBuf::from("/staging/job-1/s01e01/Show.uk.vtt.partial"),
                final_path: PathBuf::from("/plex/tv/Show/Season 01/Show - S01E01.uk.vtt"),
            },
        ],
        plex: PlexExpectation {
            path: PathBuf::from("/plex/tv/Show/Season 01/Show - S01E01.mkv"),
            canonical_id: "tmdb:123".to_owned(),
            season: Some(1),
            episode: Some(1),
        },
    }
}

fn matched(expectation: &PlexExpectation) -> PlexCheck {
    PlexCheck::Matched(PlexObservation {
        path: expectation.path.clone(),
        canonical_id: expectation.canonical_id.clone(),
        season: expectation.season,
        episode: expectation.episode,
    })
}

#[tokio::test]
async fn rezka_episode_runs_one_pipeline_and_publishes_video_last() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([probe("h264"), encoded_probe()])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem.clone(),
        http.clone(),
        http.clone(),
        process.clone(),
        service,
    );

    let outcome = pipeline.run(&work, &NeverCancelled, &()).await.unwrap();

    assert_eq!(outcome, EpisodeOutcome::Completed);
    assert_eq!(process.commands.lock().unwrap().len(), 1);
    assert_eq!(http.video_offsets.lock().unwrap().as_slice(), &[0]);
    let published = filesystem.published.lock().unwrap();
    assert_eq!(published.last(), Some(&work.final_video));
}

#[tokio::test]
async fn expired_source_retry_downloads_and_publishes_only_after_a_fresh_attempt() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    let http = Arc::new(FakeHttp {
        video_failures: Mutex::new(VecDeque::from([RunnerPortError::SourceExpired])),
        ..FakeHttp::default()
    });
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([probe("h264"), encoded_probe()])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem.clone(),
        http.clone(),
        http.clone(),
        process,
        service,
    );
    let reporter = RecordingReporter::default();

    assert_eq!(
        pipeline
            .run(&work, &NeverCancelled, &reporter)
            .await
            .unwrap_err(),
        RunnerPortError::SourceExpired
    );
    assert!(filesystem.published.lock().unwrap().is_empty());
    assert!(http.video_offsets.lock().unwrap().is_empty());

    assert_eq!(
        pipeline
            .run(&work, &NeverCancelled, &reporter)
            .await
            .unwrap(),
        EpisodeOutcome::Completed
    );
    assert_eq!(http.video_offsets.lock().unwrap().as_slice(), &[0]);
    assert_eq!(
        filesystem.published.lock().unwrap().last(),
        Some(&work.final_video)
    );
    assert_eq!(
        reporter.events.lock().unwrap().as_slice(),
        &[
            "started:download",
            "started:download",
            "completed:download",
            "started:transcode",
            "completed:transcode",
        ]
    );
}

#[tokio::test]
async fn mapped_special_publishes_video_and_subtitles_to_plex_specials() {
    let mut work = work();
    let special = PathBuf::from("/plex/tv/Show/Specials/Show - S00E01.mkv");
    work.episode_id = "s00e01".to_owned();
    work.final_video = special.clone();
    work.plex.path = special.clone();
    work.plex.season = Some(0);
    work.plex.episode = Some(1);
    for track in &mut work.subtitles {
        track.final_path = special.with_extension(format!("{}.vtt", track.id));
    }

    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([probe("h264"), encoded_probe()])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem.clone(),
        http.clone(),
        http,
        process,
        service.clone(),
    );

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::Completed
    );
    assert_eq!(filesystem.published.lock().unwrap().last(), Some(&special));
    assert!(work.subtitles.iter().all(|track| {
        filesystem
            .files
            .lock()
            .unwrap()
            .contains_key(&track.final_path)
    }));
    assert_eq!(service.scans.lock().unwrap().as_slice(), &[work.plex]);
}

#[tokio::test]
async fn rezka_hls_uses_the_media_transfer_port_before_transcoding() {
    let mut work = work();
    work.source_kind = VideoSourceKind::Hls;
    let filesystem = Arc::new(FakeFs::with_available(50 * GIB));
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([probe("h264"), encoded_probe()])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let reporter = RecordingReporter::default();
    let pipeline = EpisodePipeline::new(
        filesystem,
        http.clone(),
        http.clone(),
        process.clone(),
        service,
    );

    let outcome = pipeline
        .run(&work, &NeverCancelled, &reporter)
        .await
        .unwrap();

    assert_eq!(outcome, EpisodeOutcome::Completed);
    assert_eq!(http.video_offsets.lock().unwrap().as_slice(), &[0]);
    let commands = process.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(!format!("{:?}", commands[0]).contains("token=secret"));
    assert_eq!(
        reporter.events.lock().unwrap().as_slice(),
        &[
            "started:download",
            "completed:download",
            "started:transcode",
            "completed:transcode"
        ]
    );
}

#[tokio::test]
async fn storage_preflight_uses_the_probed_source_size() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(23 * GIB));
    let http = Arc::new(FakeHttp {
        video_size: Some(2 * GIB),
        ..FakeHttp::default()
    });
    let process = Arc::new(FakeProcess {
        probes: Mutex::default(),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::default(),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(filesystem, http.clone(), http.clone(), process, service)
        .with_storage_reserve_bytes(20 * GIB);
    let reporter = RecordingReporter::default();

    let EpisodeOutcome::BlockedStorage(blocked) = pipeline
        .run(&work, &NeverCancelled, &reporter)
        .await
        .unwrap()
    else {
        panic!("expected storage-blocked outcome");
    };
    assert_eq!(blocked.available_bytes(), 23 * GIB);
    assert_eq!(blocked.required_bytes(), 24 * GIB);
    assert!(http.video_offsets.lock().unwrap().is_empty());
    assert!(reporter.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unknown_source_size_uses_duration_instead_of_a_fixed_twenty_gib_peak() {
    let mut work = work();
    work.expected_duration_seconds = Some(45.0 * 60.0);
    let filesystem = Arc::new(FakeFs::with_available(9 * GIB));
    let http = Arc::new(FakeHttp {
        probe_fails: true,
        ..FakeHttp::default()
    });
    let mut source_probe = probe("h264");
    source_probe.duration_seconds = 45.0 * 60.0;
    let mut encoded_probe = encoded_probe();
    encoded_probe.duration_seconds = 45.0 * 60.0;
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([source_probe, encoded_probe])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(filesystem, http.clone(), http.clone(), process, service);

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::Completed
    );
    assert_eq!(http.video_offsets.lock().unwrap().as_slice(), &[0]);
}

#[tokio::test]
async fn failed_subtitle_is_partial_and_retry_fetches_only_that_track() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    let http = Arc::new(FakeHttp::default());
    http.subtitle_failures
        .lock()
        .unwrap()
        .insert("uk".to_owned());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([
            probe("h264"),
            encoded_probe(),
            encoded_probe(),
        ])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex), matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(filesystem, http.clone(), http.clone(), process, service);

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::Partial {
            missing_subtitles: vec!["uk".to_owned()]
        }
    );
    http.subtitle_requests.lock().unwrap().clear();
    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::Completed
    );
    assert_eq!(http.subtitle_requests.lock().unwrap().as_slice(), &["uk"]);
}

#[tokio::test]
async fn existing_publication_skips_download_and_transcode_but_reconciles_plex() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    filesystem
        .files
        .lock()
        .unwrap()
        .insert(work.final_video.clone(), b"published".to_vec());
    for subtitle in &work.subtitles {
        filesystem.files.lock().unwrap().insert(
            subtitle.final_path.clone(),
            b"WEBVTT\n\n00 --> 01\ntext".to_vec(),
        );
    }
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([encoded_probe()])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([PlexCheck::Pending])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem,
        http.clone(),
        http.clone(),
        process.clone(),
        service,
    );

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::PlexPending
    );
    assert!(http.video_offsets.lock().unwrap().is_empty());
    assert!(process.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn legacy_hd_publication_is_replaced_with_full_hd_output() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    filesystem
        .files
        .lock()
        .unwrap()
        .insert(work.final_video.clone(), b"legacy-720p".to_vec());
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([
            probe("hevc"),
            probe("h264"),
            encoded_probe(),
        ])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem.clone(),
        http.clone(),
        http.clone(),
        process.clone(),
        service,
    );

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::Completed
    );
    assert_eq!(http.video_offsets.lock().unwrap().as_slice(), &[0]);
    assert_eq!(process.commands.lock().unwrap().len(), 1);
    assert_eq!(
        filesystem.files.lock().unwrap().get(&work.final_video),
        Some(&b"encoded".to_vec())
    );
}

#[tokio::test]
async fn rezka_transcode_emits_a_transcode_stage_start() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([probe("h264"), encoded_probe()])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(filesystem, http.clone(), http, process, service);
    let reporter = RecordingReporter::default();

    assert_eq!(
        pipeline
            .run(&work, &NeverCancelled, &reporter)
            .await
            .unwrap(),
        EpisodeOutcome::Completed
    );
    // The transfer and transcode milestones wrap the actual work, not preflight.
    assert_eq!(
        reporter.events.lock().unwrap().as_slice(),
        &[
            "started:download".to_owned(),
            "completed:download".to_owned(),
            "started:transcode".to_owned(),
            "completed:transcode".to_owned()
        ]
    );
}

#[tokio::test]
async fn publication_without_expected_audio_metadata_is_replaced() {
    let mut work = work();
    work.audio = Some(media_runner::AudioTrackMetadata {
        language: "rus".to_owned(),
        title: "DEEP".to_owned(),
    });
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    filesystem
        .files
        .lock()
        .unwrap()
        .insert(work.final_video.clone(), b"untagged".to_vec());
    let mut tagged = encoded_probe();
    tagged.audio_language = Some("rus".to_owned());
    tagged.audio_title = Some("DEEP".to_owned());
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([encoded_probe(), probe("h264"), tagged])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem,
        http.clone(),
        http.clone(),
        process.clone(),
        service,
    );

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::Completed
    );
    assert_eq!(http.video_offsets.lock().unwrap().as_slice(), &[0]);
    let commands = process.commands.lock().unwrap();
    assert!(
        commands[0]
            .args()
            .windows(2)
            .any(|args| { args == ["-metadata:s:a:0", "language=rus"] })
    );
    assert!(
        commands[0]
            .args()
            .windows(2)
            .any(|args| { args == ["-metadata:s:a:0", "title=DEEP"] })
    );
}

#[tokio::test]
async fn skipped_transcode_emits_no_transcode_stage_start() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    // The episode is already published, so download and transcode are skipped.
    filesystem
        .files
        .lock()
        .unwrap()
        .insert(work.final_video.clone(), b"published".to_vec());
    for subtitle in &work.subtitles {
        filesystem.files.lock().unwrap().insert(
            subtitle.final_path.clone(),
            b"WEBVTT\n\n00 --> 01\ntext".to_vec(),
        );
    }
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([encoded_probe()])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(filesystem, http.clone(), http, process, service);
    let reporter = RecordingReporter::default();

    pipeline
        .run(&work, &NeverCancelled, &reporter)
        .await
        .unwrap();

    assert!(reporter.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn complete_partial_skips_download_but_still_transcodes_and_publishes() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    // A prior attempt already downloaded the full source (equal to the probed
    // size) but failed before publishing.
    filesystem
        .files
        .lock()
        .unwrap()
        .insert(work.source_partial.clone(), b"video".to_vec());
    let http = Arc::new(FakeHttp {
        video_size: Some(5),
        ..FakeHttp::default()
    });
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([probe("h264"), encoded_probe()])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([matched(&work.plex)])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem.clone(),
        http.clone(),
        http.clone(),
        process.clone(),
        service,
    );

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::Completed
    );
    // The download was skipped because the partial already spanned the source.
    assert!(http.video_offsets.lock().unwrap().is_empty());
    // Transcode and publication still ran on the recovered partial.
    assert_eq!(process.commands.lock().unwrap().len(), 1);
    assert_eq!(
        filesystem.published.lock().unwrap().last(),
        Some(&work.final_video)
    );
}

#[tokio::test]
async fn torrent_work_never_touches_download_transcode_or_publication() {
    let mut work = work();
    work.provider = ProviderKind::Torrent;
    work.source_url = None;
    let filesystem = Arc::new(FakeFs::with_available(0));
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::default(),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([PlexCheck::Mismatch])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem.clone(),
        http.clone(),
        http.clone(),
        process.clone(),
        service,
    );

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::NeedsActionPlexMismatch
    );
    assert!(filesystem.published.lock().unwrap().is_empty());
    assert!(http.video_offsets.lock().unwrap().is_empty());
    assert!(process.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_from_a_port_stops_before_transcode_and_publication() {
    let work = work();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    let http = Arc::new(FakeHttp {
        cancel_video: true,
        ..FakeHttp::default()
    });
    let process = Arc::new(FakeProcess {
        probes: Mutex::default(),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::default(),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(
        filesystem.clone(),
        http.clone(),
        http,
        process.clone(),
        service,
    );

    assert_eq!(
        pipeline.run(&work, &NeverCancelled, &()).await.unwrap(),
        EpisodeOutcome::Cancelled
    );
    assert!(process.commands.lock().unwrap().is_empty());
    assert!(filesystem.published.lock().unwrap().is_empty());
}

#[tokio::test]
async fn job_runner_processes_episode_work_items_sequentially() {
    let first = work();
    let mut second = work();
    second.episode_id = "s01e02".to_owned();
    second.staging_directory = PathBuf::from("/staging/job-1/s01e02");
    second.source_partial = second.staging_directory.join("source.partial");
    second.encoded_partial = second.staging_directory.join("encoded.partial.mkv");
    second.final_video = PathBuf::from("/plex/tv/Show/Season 01/Show - S01E02.mkv");
    second.plex.path = second.final_video.clone();
    second.plex.episode = Some(2);
    second.subtitles.clear();
    let filesystem = Arc::new(FakeFs::with_available(30 * GIB));
    let http = Arc::new(FakeHttp::default());
    let process = Arc::new(FakeProcess {
        probes: Mutex::new(VecDeque::from([
            probe("h264"),
            encoded_probe(),
            probe("h264"),
            encoded_probe(),
        ])),
        commands: Mutex::default(),
        filesystem: filesystem.clone(),
    });
    let service = Arc::new(FakeService {
        checks: Mutex::new(VecDeque::from([
            matched(&first.plex),
            matched(&second.plex),
        ])),
        scans: Mutex::default(),
    });
    let pipeline = EpisodePipeline::new(filesystem, http.clone(), http, process, service);

    assert_eq!(
        pipeline
            .run_all(&[first, second], &NeverCancelled, &())
            .await
            .unwrap(),
        vec![EpisodeOutcome::Completed, EpisodeOutcome::Completed]
    );
}
