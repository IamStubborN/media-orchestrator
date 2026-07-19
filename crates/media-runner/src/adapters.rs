use std::{
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{
    Cancellation, FileSystemPort, HttpPort, MediaProbe, PlexCheck, PlexExpectation,
    PlexObservation, ProcessCommand, ProcessPort, ResumeAction, RunnerPortError, RunnerServicePort,
    SensitiveUrl, StorageRoots, decide_resume,
};

const MAX_SUBTITLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROCESS_ERROR_BYTES: usize = 16 * 1024;

#[derive(Debug, Copy, Clone, Default)]
pub struct TokioFileSystem;

impl TokioFileSystem {
    pub async fn prepare_storage_roots(&self, roots: &StorageRoots) -> Result<(), RunnerPortError> {
        for path in [roots.staging(), roots.tv(), roots.movies()] {
            self.create_dir_all(path).await?;
        }
        let staging = tokio::fs::canonicalize(roots.staging())
            .await
            .map_err(|_| RunnerPortError::Filesystem)?;
        let tv = tokio::fs::canonicalize(roots.tv())
            .await
            .map_err(|_| RunnerPortError::Filesystem)?;
        let movies = tokio::fs::canonicalize(roots.movies())
            .await
            .map_err(|_| RunnerPortError::Filesystem)?;
        if staging.starts_with(&tv)
            || staging.starts_with(&movies)
            || tv.starts_with(&staging)
            || movies.starts_with(&staging)
            || !same_filesystem(&staging, &tv).await?
            || !same_filesystem(&staging, &movies).await?
        {
            return Err(RunnerPortError::Filesystem);
        }
        Ok(())
    }
}

#[cfg(unix)]
async fn same_filesystem(left: &Path, right: &Path) -> Result<bool, RunnerPortError> {
    use std::os::unix::fs::MetadataExt as _;

    let left = tokio::fs::metadata(left)
        .await
        .map_err(|_| RunnerPortError::Filesystem)?;
    let right = tokio::fs::metadata(right)
        .await
        .map_err(|_| RunnerPortError::Filesystem)?;
    Ok(left.dev() == right.dev())
}

#[cfg(not(unix))]
async fn same_filesystem(_left: &Path, _right: &Path) -> Result<bool, RunnerPortError> {
    Err(RunnerPortError::Filesystem)
}

#[async_trait]
impl FileSystemPort for TokioFileSystem {
    async fn available_bytes(&self, path: &Path) -> Result<u64, RunnerPortError> {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || fs2::available_space(path))
            .await
            .map_err(|_| RunnerPortError::Filesystem)?
            .map_err(|_| RunnerPortError::Filesystem)
    }

    async fn file_len(&self, path: &Path) -> Result<Option<u64>, RunnerPortError> {
        match tokio::fs::metadata(path).await {
            Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
            Ok(_) => Err(RunnerPortError::Filesystem),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(RunnerPortError::Filesystem),
        }
    }

    async fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, RunnerPortError> {
        match tokio::fs::read(path).await {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(RunnerPortError::Filesystem),
        }
    }

    async fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), RunnerPortError> {
        let path = path.to_owned();
        let contents = contents.to_vec();
        tokio::task::spawn_blocking(move || atomic_write(&path, &contents))
            .await
            .map_err(|_| RunnerPortError::Filesystem)?
    }

    async fn write_chunk(
        &self,
        path: &Path,
        offset: u64,
        contents: &[u8],
    ) -> Result<(), RunnerPortError> {
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).write(true);
        if offset == 0 {
            options.truncate(true);
        } else {
            let length = self
                .file_len(path)
                .await?
                .ok_or(RunnerPortError::Filesystem)?;
            if length != offset {
                return Err(RunnerPortError::Filesystem);
            }
            options.append(true);
        }
        let mut file = options
            .open(path)
            .await
            .map_err(|_| RunnerPortError::Filesystem)?;
        file.write_all(contents)
            .await
            .map_err(|_| RunnerPortError::Filesystem)?;
        file.flush().await.map_err(|_| RunnerPortError::Filesystem)
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), RunnerPortError> {
        tokio::fs::create_dir_all(path)
            .await
            .map_err(|_| RunnerPortError::Filesystem)
    }

    async fn publish_atomic(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), RunnerPortError> {
        match tokio::fs::hard_link(source, destination).await {
            Ok(()) => {
                sync_file(destination).await?;
                tokio::fs::remove_file(source)
                    .await
                    .map_err(|_| RunnerPortError::Filesystem)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(_) => Err(RunnerPortError::Filesystem),
        }
    }

    async fn replace_atomic(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), RunnerPortError> {
        tokio::fs::rename(source, destination)
            .await
            .map_err(|_| RunnerPortError::Filesystem)?;
        sync_file(destination).await
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), RunnerPortError> {
    let parent = path.parent().ok_or(RunnerPortError::Filesystem)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| RunnerPortError::Filesystem)?;
    temporary
        .write_all(contents)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_| RunnerPortError::Filesystem)?;
    temporary
        .persist(path)
        .map_err(|_| RunnerPortError::Filesystem)?;
    Ok(())
}

async fn sync_file(path: &Path) -> Result<(), RunnerPortError> {
    tokio::fs::File::open(path)
        .await
        .map_err(|_| RunnerPortError::Filesystem)?
        .sync_all()
        .await
        .map_err(|_| RunnerPortError::Filesystem)
}

#[derive(Clone)]
pub struct ReqwestHttpAdapter {
    client: reqwest::Client,
}

impl ReqwestHttpAdapter {
    /// Builds an adapter for short, bounded requests. `timeout` is a total
    /// request deadline, which is appropriate only when the whole response is
    /// expected to arrive quickly.
    pub fn new(timeout: Duration) -> Result<Self, RunnerPortError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|_| RunnerPortError::Http)?;
        Ok(Self { client })
    }

    /// Builds an adapter for streaming media transfers. It applies a connect and
    /// an idle read timeout but no total deadline, so an arbitrarily large
    /// download is bounded by continued progress rather than wall-clock time;
    /// logical hangs rely on cooperative cancellation at the call sites.
    pub fn streaming(
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self, RunnerPortError> {
        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .read_timeout(read_timeout)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|_| RunnerPortError::Http)?;
        Ok(Self { client })
    }
}

impl std::fmt::Debug for ReqwestHttpAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ReqwestHttpAdapter { client: [REDACTED] }")
    }
}

#[async_trait]
impl HttpPort for ReqwestHttpAdapter {
    async fn validate_video_source(&self, url: &SensitiveUrl) -> Result<(), RunnerPortError> {
        let response = self
            .client
            .get(url.as_url().clone())
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .await
            .map_err(|_| RunnerPortError::SourceTransferTransient)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(classify_source_status(response.status()))
        }
    }

    async fn probe_video_size(&self, url: &SensitiveUrl) -> Result<u64, RunnerPortError> {
        let response = self
            .client
            .get(url.as_url().clone())
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .await
            .map_err(|_| RunnerPortError::SourceTransferTransient)?;
        if !response.status().is_success() {
            return Err(classify_source_status(response.status()));
        }
        let content_range_total = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range)
            .and_then(|(_, total)| total);
        content_range_total
            .or_else(|| response.content_length())
            .filter(|size| *size > 0)
            .ok_or(RunnerPortError::Http)
    }

    async fn download_video(
        &self,
        url: &SensitiveUrl,
        partial_path: &Path,
        resume_from: u64,
        filesystem: &dyn FileSystemPort,
        cancellation: &dyn Cancellation,
    ) -> Result<(), RunnerPortError> {
        self.download_video_with_progress(
            url,
            partial_path,
            resume_from,
            filesystem,
            cancellation,
            crate::TransferProgressContext {
                total_bytes: None,
                reporter: &(),
            },
        )
        .await
    }

    async fn download_video_with_progress(
        &self,
        url: &SensitiveUrl,
        partial_path: &Path,
        resume_from: u64,
        filesystem: &dyn FileSystemPort,
        cancellation: &dyn Cancellation,
        progress: crate::TransferProgressContext<'_>,
    ) -> Result<(), RunnerPortError> {
        let crate::TransferProgressContext {
            total_bytes,
            reporter,
        } = progress;
        if cancellation.is_cancelled() {
            return Err(RunnerPortError::Cancelled);
        }
        let mut request = self.client.get(url.as_url().clone());
        if resume_from > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|_| RunnerPortError::SourceTransferTransient)?;
        let status = response.status().as_u16();
        if !matches!(status, 200 | 206 | 416) {
            return Err(classify_source_status(response.status()));
        }
        let raw_content_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok());
        // A 416 carries an unsatisfied range (`bytes */total`) rather than a
        // satisfied one; surface its total so decide_resume can recognize a
        // partial that already spans the whole resource as already-complete.
        let content_range = if status == 416 {
            raw_content_range
                .and_then(parse_unsatisfied_range)
                .map(|total| (resume_from, Some(total)))
        } else {
            raw_content_range.and_then(parse_content_range)
        };
        let total_bytes = total_bytes.or_else(|| content_range.and_then(|(_, total)| total));
        let action =
            decide_resume(resume_from, status, content_range).map_err(|_| RunnerPortError::Http)?;
        let mut offset = match action {
            ResumeAction::Append => resume_from,
            ResumeAction::Restart => 0,
            ResumeAction::Complete => {
                reporter
                    .stage_progress(
                        "download",
                        direct_observation(resume_from, total_bytes, 0, Duration::ZERO, true),
                    )
                    .await;
                return Ok(());
            }
        };
        let initial_offset = offset;
        let started = tokio::time::Instant::now();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            if cancellation.is_cancelled() {
                return Err(RunnerPortError::Cancelled);
            }
            let chunk = chunk.map_err(|_| RunnerPortError::SourceTransferTransient)?;
            if chunk.is_empty() {
                continue;
            }
            filesystem.write_chunk(partial_path, offset, &chunk).await?;
            offset = offset
                .checked_add(chunk.len() as u64)
                .ok_or(RunnerPortError::Http)?;
            reporter
                .stage_progress(
                    "download",
                    direct_observation(
                        offset,
                        total_bytes,
                        initial_offset,
                        started.elapsed(),
                        false,
                    ),
                )
                .await;
        }
        if offset == initial_offset {
            return Err(RunnerPortError::SourceTransferTransient);
        }
        reporter
            .stage_progress(
                "download",
                direct_observation(offset, total_bytes, initial_offset, started.elapsed(), true),
            )
            .await;
        Ok(())
    }

    async fn fetch_subtitle(
        &self,
        url: &SensitiveUrl,
        cancellation: &dyn Cancellation,
    ) -> Result<Vec<u8>, RunnerPortError> {
        if cancellation.is_cancelled() {
            return Err(RunnerPortError::Cancelled);
        }
        let response = self
            .client
            .get(url.as_url().clone())
            .send()
            .await
            .map_err(|_| RunnerPortError::Http)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_SUBTITLE_BYTES as u64)
        {
            return Err(RunnerPortError::Http);
        }
        let mut contents = Vec::new();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            if cancellation.is_cancelled() {
                return Err(RunnerPortError::Cancelled);
            }
            let chunk = chunk.map_err(|_| RunnerPortError::Http)?;
            if contents.len().saturating_add(chunk.len()) > MAX_SUBTITLE_BYTES {
                return Err(RunnerPortError::Http);
            }
            contents.extend_from_slice(&chunk);
        }
        if contents.is_empty() {
            return Err(RunnerPortError::Http);
        }
        Ok(contents)
    }
}

fn direct_observation(
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    initial_offset: u64,
    elapsed: Duration,
    final_observation: bool,
) -> crate::TransferObservation {
    let transferred = downloaded_bytes.saturating_sub(initial_offset);
    let elapsed_seconds = elapsed.as_secs_f64();
    let download_speed_bps = (elapsed_seconds > 0.0 && transferred > 0)
        .then(|| (transferred as f64 / elapsed_seconds).round() as u64)
        .filter(|speed| *speed > 0);
    let progress_percent = total_bytes
        .filter(|total| *total > 0)
        .map(|total| ((downloaded_bytes.min(total) as f64 / total as f64) * 100.0).round() as u8);
    let eta_seconds = match (total_bytes, download_speed_bps) {
        (Some(total), Some(speed)) if speed > 0 => {
            Some(total.saturating_sub(downloaded_bytes).div_ceil(speed))
        }
        _ => None,
    };
    crate::TransferObservation {
        source: crate::TransferSource::Direct,
        state: if final_observation {
            "complete".to_owned()
        } else {
            "downloading".to_owned()
        },
        progress_percent,
        downloaded_bytes: Some(downloaded_bytes),
        total_bytes,
        download_speed_bps,
        eta_seconds,
        final_observation,
    }
}

fn classify_source_status(status: reqwest::StatusCode) -> RunnerPortError {
    match status.as_u16() {
        401 | 403 | 410 => RunnerPortError::SourceExpired,
        408 | 425 | 429 | 500..=599 => RunnerPortError::SourceTransferTransient,
        _ => RunnerPortError::SourceTransferRejected,
    }
}

fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    if end < start {
        return None;
    }
    let total = if total == "*" {
        None
    } else {
        Some(total.parse::<u64>().ok()?)
    };
    if total.is_some_and(|total| end >= total) {
        return None;
    }
    Some((start, total))
}

/// Parses the total length from an unsatisfied range header (`bytes */total`),
/// as sent with a `416 Range Not Satisfiable` response.
fn parse_unsatisfied_range(value: &str) -> Option<u64> {
    value.strip_prefix("bytes */")?.trim().parse::<u64>().ok()
}

#[derive(Debug, Clone)]
pub struct TokioProcessAdapter {
    ffprobe_program: PathBuf,
    ffmpeg_program: PathBuf,
    timeout: Duration,
}

impl TokioProcessAdapter {
    #[must_use]
    pub fn new(
        ffprobe_program: impl Into<PathBuf>,
        ffmpeg_program: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        Self {
            ffprobe_program: ffprobe_program.into(),
            ffmpeg_program: ffmpeg_program.into(),
            timeout,
        }
    }
}

#[async_trait]
impl ProcessPort for TokioProcessAdapter {
    async fn probe(
        &self,
        path: &Path,
        cancellation: &dyn Cancellation,
    ) -> Result<MediaProbe, RunnerPortError> {
        if cancellation.is_cancelled() {
            return Err(RunnerPortError::Cancelled);
        }
        let mut command = tokio::process::Command::new(&self.ffprobe_program);
        command
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name,width,height,bit_rate:stream_tags=language,title:format=duration,bit_rate",
                "-of",
                "json",
            ])
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(self.timeout, command.output())
            .await
            .map_err(|_| RunnerPortError::Process)?
            .map_err(|_| RunnerPortError::Process)?;
        if cancellation.is_cancelled() {
            return Err(RunnerPortError::Cancelled);
        }
        if !output.status.success() {
            return Err(RunnerPortError::Process);
        }
        parse_probe(&output.stdout)
    }

    async fn run(
        &self,
        command: &ProcessCommand,
        cancellation: &dyn Cancellation,
    ) -> Result<(), RunnerPortError> {
        if command.program() != "ffmpeg" {
            return Err(RunnerPortError::Process);
        }
        if cancellation.is_cancelled() {
            return Err(RunnerPortError::Cancelled);
        }
        let mut child = tokio::process::Command::new(&self.ffmpeg_program)
            .args(command.args())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| RunnerPortError::Process)?;
        let stderr = child.stderr.take().ok_or(RunnerPortError::Process)?;
        let mut stderr_task = Some(tokio::spawn(capture_process_stderr(stderr)));
        let started = tokio::time::Instant::now();
        loop {
            if cancellation.is_cancelled() {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = collect_process_stderr(&mut stderr_task).await;
                return Err(RunnerPortError::Cancelled);
            }
            if started.elapsed() >= self.timeout {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let stderr = collect_process_stderr(&mut stderr_task).await;
                log_process_failure(None, &stderr, "timeout");
                return Err(RunnerPortError::Process);
            }
            match child.try_wait().map_err(|_| RunnerPortError::Process)? {
                Some(status) if status.success() => {
                    let _ = collect_process_stderr(&mut stderr_task).await;
                    return Ok(());
                }
                Some(status) => {
                    let stderr = collect_process_stderr(&mut stderr_task).await;
                    log_process_failure(status.code(), &stderr, "exit");
                    return Err(RunnerPortError::Process);
                }
                None => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    async fn run_with_progress(
        &self,
        command: &ProcessCommand,
        output_path: &Path,
        reporter: &dyn crate::StageReporter,
        cancellation: &dyn Cancellation,
    ) -> Result<(), RunnerPortError> {
        if command.program() != "ffmpeg" {
            return Err(RunnerPortError::Process);
        }
        if cancellation.is_cancelled() {
            return Err(RunnerPortError::Cancelled);
        }
        let mut child = tokio::process::Command::new(&self.ffmpeg_program)
            .args(command.args())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| RunnerPortError::Process)?;
        let stderr = child.stderr.take().ok_or(RunnerPortError::Process)?;
        let mut stderr_task = Some(tokio::spawn(capture_process_stderr(stderr)));
        let started = tokio::time::Instant::now();
        let mut previous_sample = started;
        let mut previous_bytes = 0_u64;
        loop {
            if cancellation.is_cancelled() {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = collect_process_stderr(&mut stderr_task).await;
                return Err(RunnerPortError::Cancelled);
            }
            if started.elapsed() >= self.timeout {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let stderr = collect_process_stderr(&mut stderr_task).await;
                log_process_failure(None, &stderr, "timeout");
                return Err(RunnerPortError::SourceTransferTransient);
            }
            match child.try_wait().map_err(|_| RunnerPortError::Process)? {
                Some(status) if status.success() => {
                    let _ = collect_process_stderr(&mut stderr_task).await;
                    let downloaded_bytes = tokio::fs::metadata(output_path)
                        .await
                        .ok()
                        .map(|metadata| metadata.len());
                    reporter
                        .stage_progress("download", hls_observation(downloaded_bytes, None, true))
                        .await;
                    return Ok(());
                }
                Some(status) => {
                    let stderr = collect_process_stderr(&mut stderr_task).await;
                    log_process_failure(status.code(), &stderr, "exit");
                    return Err(RunnerPortError::SourceTransferTransient);
                }
                None => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let now = tokio::time::Instant::now();
                    let downloaded_bytes = tokio::fs::metadata(output_path)
                        .await
                        .ok()
                        .map(|metadata| metadata.len());
                    let speed = downloaded_bytes.and_then(|bytes| {
                        let elapsed = now.duration_since(previous_sample).as_secs_f64();
                        (bytes > previous_bytes && elapsed > 0.0)
                            .then(|| ((bytes - previous_bytes) as f64 / elapsed).round() as u64)
                            .filter(|speed| *speed > 0)
                    });
                    if let Some(bytes) = downloaded_bytes {
                        previous_bytes = bytes;
                        previous_sample = now;
                    }
                    reporter
                        .stage_progress("download", hls_observation(downloaded_bytes, speed, false))
                        .await;
                }
            }
        }
    }
}

async fn capture_process_stderr(mut stderr: tokio::process::ChildStderr) -> Vec<u8> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let remaining = MAX_PROCESS_ERROR_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    retained
}

async fn collect_process_stderr(task: &mut Option<tokio::task::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    match task.take() {
        Some(task) => task.await.unwrap_or_default(),
        None => Vec::new(),
    }
}

fn log_process_failure(exit_code: Option<i32>, stderr: &[u8], reason: &str) {
    let summary = redact_process_stderr(stderr);
    tracing::warn!(
        exit_code,
        reason,
        stderr = %summary,
        "ffmpeg process failed"
    );
}

fn redact_process_stderr(stderr: &[u8]) -> String {
    let mut summary = String::new();
    for token in String::from_utf8_lossy(stderr).split_whitespace() {
        let token = if token.contains("://") {
            "[REDACTED_URL]"
        } else {
            token
        };
        let separator = usize::from(!summary.is_empty());
        if summary.len() + separator + token.len() > 512 {
            break;
        }
        if separator == 1 {
            summary.push(' ');
        }
        summary.push_str(token);
    }
    if summary.is_empty() {
        "[no stderr]".to_owned()
    } else {
        summary
    }
}

#[cfg(test)]
mod process_error_tests {
    use super::redact_process_stderr;

    #[test]
    fn process_stderr_redacts_complete_urls_and_keeps_the_reason() {
        let stderr =
            b"[https] HTTP error 403 Forbidden for https://cdn.example/video.m3u8?token=secret";

        let summary = redact_process_stderr(stderr);

        assert_eq!(
            summary,
            "[https] HTTP error 403 Forbidden for [REDACTED_URL]"
        );
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("cdn.example"));
    }
}

fn hls_observation(
    downloaded_bytes: Option<u64>,
    download_speed_bps: Option<u64>,
    final_observation: bool,
) -> crate::TransferObservation {
    crate::TransferObservation {
        source: crate::TransferSource::Hls,
        state: if final_observation {
            "complete".to_owned()
        } else {
            "downloading".to_owned()
        },
        progress_percent: None,
        downloaded_bytes,
        total_bytes: None,
        download_speed_bps,
        eta_seconds: None,
        final_observation,
    }
}

#[derive(serde::Deserialize)]
struct ProbeDocument {
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(serde::Deserialize)]
struct ProbeStream {
    codec_type: String,
    codec_name: String,
    width: Option<u32>,
    height: Option<u32>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: ProbeTags,
}

#[derive(Default, serde::Deserialize)]
struct ProbeTags {
    language: Option<String>,
    title: Option<String>,
}

#[derive(serde::Deserialize)]
struct ProbeFormat {
    duration: String,
    bit_rate: Option<String>,
}

fn parse_probe(contents: &[u8]) -> Result<MediaProbe, RunnerPortError> {
    let document: ProbeDocument =
        serde_json::from_slice(contents).map_err(|_| RunnerPortError::Process)?;
    let stream = document
        .streams
        .iter()
        .find(|stream| stream.codec_type == "video")
        .ok_or(RunnerPortError::Process)?;
    let audio = document
        .streams
        .iter()
        .find(|stream| stream.codec_type == "audio")
        .map(|stream| &stream.tags);
    let duration_seconds = document
        .format
        .duration
        .parse::<f64>()
        .map_err(|_| RunnerPortError::Process)?;
    let bitrate = stream
        .bit_rate
        .clone()
        .or(document.format.bit_rate)
        .map(|value| value.parse::<u64>().map_err(|_| RunnerPortError::Process))
        .transpose()?;
    if stream.codec_name.is_empty()
        || stream.width.is_none_or(|width| width == 0)
        || stream.height.is_none_or(|height| height == 0)
        || !duration_seconds.is_finite()
        || duration_seconds <= 0.0
    {
        return Err(RunnerPortError::Process);
    }
    Ok(MediaProbe {
        codec: stream.codec_name.clone(),
        width: stream.width.ok_or(RunnerPortError::Process)?,
        height: stream.height.ok_or(RunnerPortError::Process)?,
        duration_seconds,
        bitrate,
        audio_language: audio.and_then(|tags| tags.language.clone()),
        audio_title: audio.and_then(|tags| tags.title.clone()),
    })
}

#[derive(Clone)]
pub struct HttpRunnerServiceAdapter {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    token: SecretString,
}

impl HttpRunnerServiceAdapter {
    pub fn new(
        base_url: reqwest::Url,
        token: SecretString,
        timeout: Duration,
    ) -> Result<Self, RunnerPortError> {
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
        {
            return Err(RunnerPortError::Service);
        }
        let endpoint = base_url
            .join("v1/runner/plex/reconcile")
            .map_err(|_| RunnerPortError::Service)?;
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| RunnerPortError::Service)?;
        Ok(Self {
            client,
            endpoint,
            token,
        })
    }
}

impl std::fmt::Debug for HttpRunnerServiceAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "HttpRunnerServiceAdapter { client: [REDACTED], endpoint: [REDACTED], token: [REDACTED] }",
        )
    }
}

#[derive(serde::Serialize)]
struct PlexRequest<'a> {
    path: &'a Path,
    canonical_id: &'a str,
    season: Option<u32>,
    episode: Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlexStatus {
    Matched,
    Pending,
    Mismatch,
}

#[derive(serde::Deserialize)]
struct PlexResponse {
    status: PlexStatus,
    observation: Option<PlexObservationDto>,
}

#[derive(serde::Deserialize)]
struct PlexObservationDto {
    path: PathBuf,
    canonical_id: String,
    season: Option<u32>,
    episode: Option<u32>,
}

#[async_trait]
impl RunnerServicePort for HttpRunnerServiceAdapter {
    async fn scan_and_verify(
        &self,
        expectation: &PlexExpectation,
    ) -> Result<PlexCheck, RunnerPortError> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(self.token.expose_secret())
            .json(&PlexRequest {
                path: &expectation.path,
                canonical_id: &expectation.canonical_id,
                season: expectation.season,
                episode: expectation.episode,
            })
            .send()
            .await
            .map_err(|_| RunnerPortError::Service)?;
        if !response.status().is_success() {
            return Err(RunnerPortError::Service);
        }
        let response: PlexResponse = response
            .json()
            .await
            .map_err(|_| RunnerPortError::Service)?;
        match response.status {
            PlexStatus::Pending => Ok(PlexCheck::Pending),
            PlexStatus::Mismatch => Ok(PlexCheck::Mismatch),
            PlexStatus::Matched => {
                let observation = response.observation.ok_or(RunnerPortError::Service)?;
                Ok(PlexCheck::Matched(PlexObservation {
                    path: observation.path,
                    canonical_id: observation.canonical_id,
                    season: observation.season,
                    episode: observation.episode,
                }))
            }
        }
    }
}
