use std::{
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::io::AsyncWriteExt as _;

use crate::{
    Cancellation, FileSystemPort, HttpPort, MediaProbe, PlexCheck, PlexExpectation,
    PlexObservation, ProcessCommand, ProcessPort, ResumeAction, RunnerPortError, RunnerServicePort,
    SensitiveUrl, StorageRoots, decide_resume,
};

const MAX_SUBTITLE_BYTES: usize = 8 * 1024 * 1024;

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
    pub fn new(timeout: Duration) -> Result<Self, RunnerPortError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
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
    async fn probe_video_size(&self, url: &SensitiveUrl) -> Result<u64, RunnerPortError> {
        let response = self
            .client
            .get(url.as_url().clone())
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .await
            .map_err(|_| RunnerPortError::Http)?;
        if !response.status().is_success() {
            return Err(RunnerPortError::Http);
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
        if cancellation.is_cancelled() {
            return Err(RunnerPortError::Cancelled);
        }
        let mut request = self.client.get(url.as_url().clone());
        if resume_from > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
        }
        let response = request.send().await.map_err(|_| RunnerPortError::Http)?;
        let status = response.status().as_u16();
        let content_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range);
        let action =
            decide_resume(resume_from, status, content_range).map_err(|_| RunnerPortError::Http)?;
        let mut offset = match action {
            ResumeAction::Append => resume_from,
            ResumeAction::Restart => 0,
        };
        let initial_offset = offset;
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            if cancellation.is_cancelled() {
                return Err(RunnerPortError::Cancelled);
            }
            let chunk = chunk.map_err(|_| RunnerPortError::Http)?;
            if chunk.is_empty() {
                continue;
            }
            filesystem.write_chunk(partial_path, offset, &chunk).await?;
            offset = offset
                .checked_add(chunk.len() as u64)
                .ok_or(RunnerPortError::Http)?;
        }
        if offset == initial_offset {
            return Err(RunnerPortError::Http);
        }
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
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height,bit_rate:format=duration,bit_rate",
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
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| RunnerPortError::Process)?;
        let started = tokio::time::Instant::now();
        loop {
            if cancellation.is_cancelled() {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(RunnerPortError::Cancelled);
            }
            if started.elapsed() >= self.timeout {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(RunnerPortError::Process);
            }
            match child.try_wait().map_err(|_| RunnerPortError::Process)? {
                Some(status) if status.success() => return Ok(()),
                Some(_) => return Err(RunnerPortError::Process),
                None => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct ProbeDocument {
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(serde::Deserialize)]
struct ProbeStream {
    codec_name: String,
    width: u32,
    height: u32,
    bit_rate: Option<String>,
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
        .into_iter()
        .next()
        .ok_or(RunnerPortError::Process)?;
    let duration_seconds = document
        .format
        .duration
        .parse::<f64>()
        .map_err(|_| RunnerPortError::Process)?;
    let bitrate = stream
        .bit_rate
        .or(document.format.bit_rate)
        .map(|value| value.parse::<u64>().map_err(|_| RunnerPortError::Process))
        .transpose()?;
    if stream.codec_name.is_empty()
        || stream.width == 0
        || stream.height == 0
        || !duration_seconds.is_finite()
        || duration_seconds <= 0.0
    {
        return Err(RunnerPortError::Process);
    }
    Ok(MediaProbe {
        codec: stream.codec_name,
        width: stream.width,
        height: stream.height,
        duration_seconds,
        bitrate,
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
