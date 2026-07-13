use std::{path::PathBuf, sync::Arc};

use crate::{
    Cancellation, FileSystemPort, HttpPort, MediaProbe, PeakEstimate, PlexCheck, PlexExpectation,
    ProcessPort, RunnerPortError, RunnerServicePort, StageReporter, StoragePreflight,
    build_hls_ingest_command, build_rezka_vaapi_command, validate_plex_observation,
    validate_webvtt,
};

const DEFAULT_RESERVE_BYTES: u64 = 0;
const UNKNOWN_SOURCE_BYTES_PER_SECOND: u64 = 1_500_000;
const UNKNOWN_SOURCE_MINIMUM_BYTES: u64 = 512 * 1024 * 1024;
const UNKNOWN_SOURCE_DEFAULT_SECONDS: u64 = 60 * 60;
const UNKNOWN_SOURCE_MAX_SECONDS: f64 = 6.0 * 60.0 * 60.0;

#[derive(Clone)]
pub struct SensitiveUrl {
    value: url::Url,
    redacted_id: String,
}

impl SensitiveUrl {
    pub fn parse(value: &str, redacted_id: impl Into<String>) -> Result<Self, SensitiveUrlError> {
        let value = url::Url::parse(value).map_err(|_| SensitiveUrlError)?;
        if !matches!(value.scheme(), "http" | "https") || value.host_str().is_none() {
            return Err(SensitiveUrlError);
        }
        let redacted_id = redacted_id.into();
        if redacted_id.is_empty() {
            return Err(SensitiveUrlError);
        }
        Ok(Self { value, redacted_id })
    }

    #[must_use]
    pub fn as_url(&self) -> &url::Url {
        &self.value
    }

    #[must_use]
    pub fn redacted_id(&self) -> &str {
        &self.redacted_id
    }
}

impl std::fmt::Debug for SensitiveUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SensitiveUrl")
            .field("value", &"[REDACTED]")
            .field("id", &self.redacted_id)
            .finish()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("media URL is invalid")]
pub struct SensitiveUrlError;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProviderKind {
    Rezka,
    Torrent,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum VideoSourceKind {
    Mp4,
    Hls,
}

#[derive(Debug, Clone)]
pub struct SubtitleTrack {
    pub id: String,
    pub url: SensitiveUrl,
    pub staging_path: PathBuf,
    pub final_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct EpisodeWork {
    pub provider: ProviderKind,
    pub job_id: String,
    pub episode_id: String,
    pub source_url: Option<SensitiveUrl>,
    pub source_kind: VideoSourceKind,
    pub staging_directory: PathBuf,
    pub source_partial: PathBuf,
    pub encoded_partial: PathBuf,
    pub final_video: PathBuf,
    pub vaapi_device: PathBuf,
    pub expected_duration_seconds: Option<f64>,
    pub subtitles: Vec<SubtitleTrack>,
    pub plex: PlexExpectation,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum EpisodeOutcome {
    Completed,
    Partial { missing_subtitles: Vec<String> },
    BlockedStorage,
    PlexPending,
    NeedsActionPlexMismatch,
    Cancelled,
}

pub struct EpisodePipeline {
    filesystem: Arc<dyn FileSystemPort>,
    http: Arc<dyn HttpPort>,
    process: Arc<dyn ProcessPort>,
    service: Arc<dyn RunnerServicePort>,
    storage: StoragePreflight,
}

impl EpisodePipeline {
    #[must_use]
    pub fn new(
        filesystem: Arc<dyn FileSystemPort>,
        http: Arc<dyn HttpPort>,
        process: Arc<dyn ProcessPort>,
        service: Arc<dyn RunnerServicePort>,
    ) -> Self {
        Self {
            filesystem,
            http,
            process,
            service,
            storage: StoragePreflight::new(DEFAULT_RESERVE_BYTES),
        }
    }

    #[must_use]
    pub const fn with_storage_reserve_bytes(mut self, reserve_bytes: u64) -> Self {
        self.storage = StoragePreflight::new(reserve_bytes);
        self
    }

    pub async fn run(
        &self,
        work: &EpisodeWork,
        cancellation: &dyn Cancellation,
        reporter: &dyn StageReporter,
    ) -> Result<EpisodeOutcome, RunnerPortError> {
        self.validate_work(work)?;
        if cancellation.is_cancelled() {
            return Ok(EpisodeOutcome::Cancelled);
        }
        if work.provider == ProviderKind::Torrent {
            return self.reconcile(&work.plex).await;
        }

        let video_published = self.filesystem.file_len(&work.final_video).await?.is_some();
        if !video_published {
            let source_url = work
                .source_url
                .as_ref()
                .ok_or(RunnerPortError::InvalidWork)?;
            let source_bytes = match work.source_kind {
                VideoSourceKind::Mp4 => self
                    .http
                    .probe_video_size(source_url)
                    .await
                    .unwrap_or_else(|_| {
                        estimate_unknown_source_bytes(work.expected_duration_seconds)
                    }),
                VideoSourceKind::Hls => {
                    estimate_unknown_source_bytes(work.expected_duration_seconds)
                }
            };
            self.filesystem
                .create_dir_all(&work.staging_directory)
                .await?;
            let available = self
                .filesystem
                .available_bytes(&work.staging_directory)
                .await?;
            let estimate = PeakEstimate::new(source_bytes, source_bytes, 0)
                .map_err(|_| RunnerPortError::InvalidWork)?;
            if self.storage.check(available, estimate).is_err() {
                return Ok(EpisodeOutcome::BlockedStorage);
            }

            let final_parent = work
                .final_video
                .parent()
                .ok_or(RunnerPortError::InvalidWork)?;
            self.filesystem.create_dir_all(final_parent).await?;

            match work.source_kind {
                VideoSourceKind::Mp4 => {
                    let partial_bytes = self
                        .filesystem
                        .file_len(&work.source_partial)
                        .await?
                        .unwrap_or(0);
                    // Skip re-fetching a complete partial. Restart an oversized
                    // partial instead of issuing an unsatisfiable range request.
                    if partial_bytes != source_bytes {
                        let resume_from = if partial_bytes < source_bytes {
                            partial_bytes
                        } else {
                            0
                        };
                        if let Err(error) = self
                            .http
                            .download_video(
                                source_url,
                                &work.source_partial,
                                resume_from,
                                self.filesystem.as_ref(),
                                cancellation,
                            )
                            .await
                        {
                            return cancellation_outcome(error);
                        }
                    }
                }
                VideoSourceKind::Hls => {
                    let command = build_hls_ingest_command(source_url, &work.source_partial)
                        .map_err(|_| RunnerPortError::InvalidWork)?;
                    reporter.stage_started("download").await;
                    if let Err(error) = self.process.run(&command, cancellation).await {
                        return cancellation_outcome(error);
                    }
                }
            }
            if cancellation.is_cancelled() {
                return Ok(EpisodeOutcome::Cancelled);
            }

            let source_probe = match self.process.probe(&work.source_partial, cancellation).await {
                Ok(probe) => probe,
                Err(error) => return cancellation_outcome(error),
            };
            validate_source_duration(&source_probe, work.expected_duration_seconds)?;
            let command = build_rezka_vaapi_command(
                &work.source_partial,
                &work.encoded_partial,
                &work.vaapi_device,
                &source_probe,
            )
            .map_err(|_| RunnerPortError::Process)?;
            // Milestone marking the start of the VAAPI/ffmpeg transcode; drives
            // the "transcoding started" notification. Best-effort progress only.
            reporter.stage_started("transcode").await;
            if let Err(error) = self.process.run(&command, cancellation).await {
                return cancellation_outcome(error);
            }
            if cancellation.is_cancelled() {
                return Ok(EpisodeOutcome::Cancelled);
            }
            let encoded_probe = match self
                .process
                .probe(&work.encoded_partial, cancellation)
                .await
            {
                Ok(probe) => probe,
                Err(error) => return cancellation_outcome(error),
            };
            validate_encoded_probe(&source_probe, &encoded_probe)?;
        }

        let missing_subtitles = match self.recover_subtitles(work, cancellation).await {
            Ok(missing) => missing,
            Err(error) => return cancellation_outcome(error),
        };
        if cancellation.is_cancelled() {
            return Ok(EpisodeOutcome::Cancelled);
        }
        if !video_published {
            self.filesystem
                .publish_atomic(&work.encoded_partial, &work.final_video)
                .await?;
        }

        match self.reconcile(&work.plex).await? {
            EpisodeOutcome::Completed if !missing_subtitles.is_empty() => {
                Ok(EpisodeOutcome::Partial { missing_subtitles })
            }
            outcome => Ok(outcome),
        }
    }

    pub async fn run_all(
        &self,
        work_items: &[EpisodeWork],
        cancellation: &dyn Cancellation,
        reporter: &dyn StageReporter,
    ) -> Result<Vec<EpisodeOutcome>, RunnerPortError> {
        let mut outcomes = Vec::with_capacity(work_items.len());
        for work in work_items {
            let outcome = self.run(work, cancellation, reporter).await?;
            let cancelled = outcome == EpisodeOutcome::Cancelled;
            outcomes.push(outcome);
            if cancelled {
                break;
            }
        }
        Ok(outcomes)
    }

    fn validate_work(&self, work: &EpisodeWork) -> Result<(), RunnerPortError> {
        if work.job_id.is_empty()
            || work.episode_id.is_empty()
            || work.final_video != work.plex.path
            || work.final_video.starts_with(&work.staging_directory)
            || !work.source_partial.starts_with(&work.staging_directory)
            || !work.encoded_partial.starts_with(&work.staging_directory)
            || work.subtitles.iter().any(|track| {
                track.id.is_empty() || !track.staging_path.starts_with(&work.staging_directory)
            })
        {
            return Err(RunnerPortError::InvalidWork);
        }
        Ok(())
    }

    async fn recover_subtitles(
        &self,
        work: &EpisodeWork,
        cancellation: &dyn Cancellation,
    ) -> Result<Vec<String>, RunnerPortError> {
        let mut missing = Vec::new();
        for track in &work.subtitles {
            if cancellation.is_cancelled() {
                break;
            }
            let final_contents = self.filesystem.read(&track.final_path).await?;
            if final_contents
                .as_deref()
                .is_some_and(|value| validate_webvtt(value).is_ok())
            {
                continue;
            }

            let contents = match self.http.fetch_subtitle(&track.url, cancellation).await {
                Ok(contents) if validate_webvtt(&contents).is_ok() => contents,
                Err(RunnerPortError::Cancelled) => return Err(RunnerPortError::Cancelled),
                Ok(_) | Err(_) => {
                    missing.push(track.id.clone());
                    continue;
                }
            };
            self.filesystem
                .write_atomic(&track.staging_path, &contents)
                .await?;
            let parent = track
                .final_path
                .parent()
                .ok_or(RunnerPortError::InvalidWork)?;
            self.filesystem.create_dir_all(parent).await?;
            if final_contents.is_some() {
                self.filesystem
                    .replace_atomic(&track.staging_path, &track.final_path)
                    .await?;
            } else {
                self.filesystem
                    .publish_atomic(&track.staging_path, &track.final_path)
                    .await?;
            }
        }
        Ok(missing)
    }

    async fn reconcile(
        &self,
        expectation: &PlexExpectation,
    ) -> Result<EpisodeOutcome, RunnerPortError> {
        let check = match self.service.scan_and_verify(expectation).await {
            Ok(check) => check,
            Err(RunnerPortError::Service | RunnerPortError::Http) => {
                return Ok(EpisodeOutcome::PlexPending);
            }
            Err(error) => return Err(error),
        };
        Ok(match check {
            PlexCheck::Matched(observation)
                if validate_plex_observation(expectation, &observation).is_ok() =>
            {
                EpisodeOutcome::Completed
            }
            PlexCheck::Pending => EpisodeOutcome::PlexPending,
            PlexCheck::Matched(_) | PlexCheck::Mismatch => EpisodeOutcome::NeedsActionPlexMismatch,
        })
    }
}

fn estimate_unknown_source_bytes(expected_duration_seconds: Option<f64>) -> u64 {
    let seconds = expected_duration_seconds
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| seconds.min(UNKNOWN_SOURCE_MAX_SECONDS))
        .and_then(|seconds| std::time::Duration::try_from_secs_f64(seconds).ok())
        .map(|duration| {
            duration
                .as_secs()
                .saturating_add(u64::from(duration.subsec_nanos() > 0))
        })
        .unwrap_or(UNKNOWN_SOURCE_DEFAULT_SECONDS);
    seconds
        .saturating_mul(UNKNOWN_SOURCE_BYTES_PER_SECOND)
        .max(UNKNOWN_SOURCE_MINIMUM_BYTES)
}

fn validate_source_duration(
    source: &crate::MediaProbe,
    expected_duration_seconds: Option<f64>,
) -> Result<(), RunnerPortError> {
    let Some(expected) = expected_duration_seconds else {
        return Ok(());
    };
    if !expected.is_finite() || expected <= 0.0 || source.duration_seconds < expected * 0.8 {
        return Err(RunnerPortError::Process);
    }
    Ok(())
}

fn cancellation_outcome(error: RunnerPortError) -> Result<EpisodeOutcome, RunnerPortError> {
    match error {
        RunnerPortError::Cancelled => Ok(EpisodeOutcome::Cancelled),
        error => Err(error),
    }
}

fn validate_encoded_probe(
    source: &MediaProbe,
    encoded: &MediaProbe,
) -> Result<(), RunnerPortError> {
    if !encoded.codec.eq_ignore_ascii_case("hevc")
        || source.width != encoded.width
        || source.height != encoded.height
        || encoded.duration_seconds <= 0.0
    {
        return Err(RunnerPortError::Process);
    }
    Ok(())
}
