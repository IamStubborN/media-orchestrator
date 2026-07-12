use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use media_contract::{
    JobStateDto, LeaseDto, NeedsActionReasonDto, RunnerEventDto, RunnerEventRequest,
};
use secrecy::{ExposeSecret as _, SecretString};

use crate::config::ClientConfig;

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum RunnerError {
    #[error("runner configuration is invalid")]
    Configuration,
    #[error("runner service request failed")]
    Service,
    #[error("runner execution failed")]
    Execution,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ExecutionOutcome {
    Completed,
    Partial,
    BlockedStorage,
    PlexPending,
    NeedsActionPlexMismatch,
    Cancelled,
    Failed,
}

#[async_trait::async_trait]
pub trait RunnerApi: Send + Sync {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, RunnerError>;
    async fn heartbeat(&self, lease: &LeaseDto) -> Result<LeaseDto, RunnerError>;
    async fn report(&self, lease: &LeaseDto, event: RunnerEventDto) -> Result<(), RunnerError>;
}

#[async_trait::async_trait]
pub trait JobExecutor: Send + Sync {
    async fn execute(
        &self,
        lease: &LeaseDto,
        control: &RunnerControl,
    ) -> Result<ExecutionOutcome, RunnerError>;
}

#[derive(Clone)]
pub struct RunnerControl {
    api: Arc<dyn RunnerApi>,
    lease: LeaseDto,
    cancelled: Arc<AtomicBool>,
}

impl RunnerControl {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub async fn stage_started(
        &self,
        task_ordinal: u32,
        name: &str,
        stage_ordinal: u32,
    ) -> Result<(), RunnerError> {
        self.api
            .report(
                &self.lease,
                RunnerEventDto::StageStarted {
                    task_ordinal,
                    stage_name: name.to_owned(),
                    stage_ordinal,
                },
            )
            .await
    }

    pub async fn stage_completed(
        &self,
        task_ordinal: u32,
        name: &str,
        stage_ordinal: u32,
    ) -> Result<(), RunnerError> {
        self.api
            .report(
                &self.lease,
                RunnerEventDto::StageCompleted {
                    task_ordinal,
                    stage_name: name.to_owned(),
                    stage_ordinal,
                    checkpoint: Default::default(),
                },
            )
            .await
    }
}

impl media_runner::Cancellation for RunnerControl {
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
    }
}

pub struct MediaJobExecutor {
    rezka: tokio::sync::Mutex<crate::composition::PreparedRunnerSession>,
    pipeline: media_runner::EpisodePipeline,
    qbittorrent: Option<Arc<media_integrations::qbittorrent::QbittorrentClient>>,
    gluetun: Option<Arc<media_integrations::gluetun::GluetunClient>>,
    roots: media_runner::StorageRoots,
    vaapi_device: std::path::PathBuf,
}

impl MediaJobExecutor {
    #[must_use]
    pub fn new(
        rezka: crate::composition::PreparedRunnerSession,
        pipeline: media_runner::EpisodePipeline,
        qbittorrent: Option<Arc<media_integrations::qbittorrent::QbittorrentClient>>,
        gluetun: Option<Arc<media_integrations::gluetun::GluetunClient>>,
        roots: media_runner::StorageRoots,
        vaapi_device: std::path::PathBuf,
    ) -> Self {
        Self {
            rezka: tokio::sync::Mutex::new(rezka),
            pipeline,
            qbittorrent,
            gluetun,
            roots,
            vaapi_device,
        }
    }

    async fn execute_inner(
        &self,
        lease: &LeaseDto,
        control: &RunnerControl,
    ) -> Result<ExecutionOutcome, RunnerError> {
        match lease.execution.as_ref().ok_or(RunnerError::Execution)? {
            media_contract::ExecutionSelectionDto::Rezka {
                locator,
                title_id,
                media_kind,
                translation_id,
                director,
                camrip,
                has_ads,
                season,
                episode,
                title,
            } => {
                self.execute_rezka(
                    lease,
                    control,
                    locator,
                    *title_id,
                    *media_kind,
                    *translation_id,
                    *director,
                    *camrip,
                    *has_ads,
                    *season,
                    *episode,
                    title,
                )
                .await
            }
            media_contract::ExecutionSelectionDto::Prowlarr {
                source_identity,
                info_hash,
                uri,
                title,
            } => {
                self.execute_torrent(lease, control, source_identity, info_hash, uri, title)
                    .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_rezka(
        &self,
        lease: &LeaseDto,
        control: &RunnerControl,
        locator: &str,
        title_id: u64,
        media_kind: media_contract::MediaKindDto,
        translation_id: u64,
        director: bool,
        camrip: bool,
        has_ads: bool,
        season: Option<u32>,
        episode: Option<u32>,
        title: &str,
    ) -> Result<ExecutionOutcome, RunnerError> {
        control.stage_started(0, "resolve_manifest", 0).await?;
        let mut prepared = self.rezka.lock().await;
        let crate::composition::PreparedRunnerSession {
            client,
            credentials,
            probe,
            store,
        } = &mut *prepared;
        client
            .ensure_authenticated(credentials, probe)
            .await
            .map_err(|_| RunnerError::Execution)?;
        let locator =
            rezka_client::TitleLocator::new(locator).map_err(|_| RunnerError::Execution)?;
        let details = client
            .title(&locator)
            .await
            .map_err(|_| RunnerError::Execution)?;
        if details.id().get() != title_id {
            return Err(RunnerError::Execution);
        }
        let translation_id =
            rezka_client::TranslationId::new(translation_id).map_err(|_| RunnerError::Execution)?;
        let key = match media_kind {
            media_contract::MediaKindDto::Movie => rezka_client::TranslationKey::Movie {
                id: translation_id,
                is_camrip: camrip,
                has_ads,
                is_director: director,
            },
            media_contract::MediaKindDto::Series => {
                rezka_client::TranslationKey::Series { id: translation_id }
            }
        };
        let selection = details
            .select_translation(&key)
            .map_err(|_| RunnerError::Execution)?;
        let request = match media_kind {
            media_contract::MediaKindDto::Movie => selection
                .movie_request()
                .map_err(|_| RunnerError::Execution)?,
            media_contract::MediaKindDto::Series => {
                let (season, episode) = season.zip(episode).ok_or(RunnerError::Execution)?;
                client
                    .series_availability(&selection)
                    .await
                    .map_err(|_| RunnerError::Execution)?
                    .select_episode(season, episode)
                    .map_err(|_| RunnerError::Execution)?
                    .playback_request()
            }
        };
        let manifest = client
            .resolve(request)
            .await
            .map_err(|_| RunnerError::Execution)?;
        let snapshot = client
            .export_session()
            .map_err(|_| RunnerError::Execution)?;
        store.save(&snapshot).map_err(|_| RunnerError::Execution)?;
        drop(prepared);
        control.stage_completed(0, "resolve_manifest", 0).await?;
        let work = self.rezka_work(lease, &manifest, title, season, episode)?;
        control.stage_started(0, "media_pipeline", 1).await?;
        let outcome = self
            .pipeline
            .run(&work, control)
            .await
            .map_err(|_| RunnerError::Execution)?;
        control.stage_completed(0, "media_pipeline", 1).await?;
        Ok(map_pipeline_outcome(outcome))
    }

    fn rezka_work(
        &self,
        lease: &LeaseDto,
        manifest: &rezka_client::PlaybackManifest,
        title: &str,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<media_runner::EpisodeWork, RunnerError> {
        let safe_title = safe_name(title);
        let episode_id = season
            .zip(episode)
            .map_or_else(|| "movie".to_owned(), |(s, e)| format!("s{s:02}e{e:02}"));
        let staging = self
            .roots
            .staging()
            .join(lease.job.id.to_string())
            .join(&episode_id);
        let final_video = season.zip(episode).map_or_else(
            || self.roots.movies().join(format!("{safe_title}.mkv")),
            |(s, e)| {
                self.roots
                    .tv()
                    .join(&safe_title)
                    .join(format!("Season {s:02}"))
                    .join(format!("{safe_title} - S{s:02}E{e:02}.mkv"))
            },
        );
        let endpoint = manifest
            .preferred_variant()
            .endpoints()
            .iter()
            .find(|endpoint| endpoint.kind() == rezka_client::StreamKind::Mp4)
            .ok_or(RunnerError::Execution)?;
        let source = endpoint.url().with_url(|url| url.as_str().to_owned());
        let subtitles = manifest
            .subtitles()
            .iter()
            .filter_map(|track| {
                let url = track
                    .alternatives()
                    .first()?
                    .with_url(|url| url.as_str().to_owned());
                let id = format!(
                    "{}-{}",
                    safe_name(track.id().provider_label()),
                    track.id().ordinal()
                );
                let final_path = final_video.with_extension(format!("{id}.vtt"));
                Some(media_runner::SubtitleTrack {
                    id: id.clone(),
                    url: media_runner::SensitiveUrl::parse(&url, &id).ok()?,
                    staging_path: staging.join(format!("{id}.vtt.partial")),
                    final_path,
                })
            })
            .collect();
        Ok(media_runner::EpisodeWork {
            provider: media_runner::ProviderKind::Rezka,
            job_id: lease.job.id.to_string(),
            episode_id,
            source_url: Some(
                media_runner::SensitiveUrl::parse(&source, "rezka-video")
                    .map_err(|_| RunnerError::Execution)?,
            ),
            staging_directory: staging.clone(),
            source_partial: staging.join("source.partial"),
            encoded_partial: staging.join("encoded.partial.mkv"),
            final_video: final_video.clone(),
            vaapi_device: self.vaapi_device.clone(),
            expected_download_bytes: 8 * media_runner::GIB,
            expected_transcode_bytes: 5 * media_runner::GIB,
            subtitles,
            plex: media_runner::PlexExpectation {
                path: final_video,
                canonical_id: format!("rezka://{}", manifest.title().id().get()),
                season,
                episode,
            },
        })
    }

    async fn execute_torrent(
        &self,
        lease: &LeaseDto,
        control: &RunnerControl,
        source_identity: &str,
        info_hash: &str,
        uri: &str,
        title: &str,
    ) -> Result<ExecutionOutcome, RunnerError> {
        let client = self
            .qbittorrent
            .as_ref()
            .ok_or(RunnerError::Configuration)?;
        control.stage_started(0, "torrent_submit", 0).await?;
        let selection = media_integrations::qbittorrent::ExplicitTorrentSelection::new(
            source_identity,
            info_hash,
            uri,
        )
        .map_err(|_| RunnerError::Execution)?;
        let handle = client
            .submit_selected(selection)
            .await
            .map_err(|_| RunnerError::Execution)?;
        control.stage_completed(0, "torrent_submit", 0).await?;
        control.stage_started(0, "torrent_monitor", 1).await?;
        loop {
            if control.is_cancelled() {
                return Ok(ExecutionOutcome::Cancelled);
            }
            let snapshot = client
                .monitor(&handle)
                .await
                .map_err(|_| RunnerError::Execution)?;
            match snapshot.state {
                media_integrations::qbittorrent::TorrentState::Complete => break,
                media_integrations::qbittorrent::TorrentState::Error => {
                    return Ok(ExecutionOutcome::Failed);
                }
                _ => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
        let content = client
            .discover_content(&handle)
            .await
            .map_err(|_| RunnerError::Execution)?;
        let final_video = content
            .files
            .into_iter()
            .find(|path| {
                path.extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|ext| {
                        matches!(ext.to_ascii_lowercase().as_str(), "mkv" | "mp4" | "avi")
                    })
            })
            .unwrap_or(content.root);
        control.stage_completed(0, "torrent_monitor", 1).await?;
        let staging = self
            .roots
            .staging()
            .join(lease.job.id.to_string())
            .join("torrent");
        let work = media_runner::EpisodeWork {
            provider: media_runner::ProviderKind::Torrent,
            job_id: lease.job.id.to_string(),
            episode_id: "torrent".to_owned(),
            source_url: None,
            staging_directory: staging.clone(),
            source_partial: staging.join("unused.source"),
            encoded_partial: staging.join("unused.encoded"),
            final_video: final_video.clone(),
            vaapi_device: self.vaapi_device.clone(),
            expected_download_bytes: 0,
            expected_transcode_bytes: 0,
            subtitles: Vec::new(),
            plex: media_runner::PlexExpectation {
                path: final_video,
                canonical_id: format!("prowlarr://{source_identity}"),
                season: None,
                episode: None,
            },
        };
        let _ = title;
        self.pipeline
            .run(&work, control)
            .await
            .map(map_pipeline_outcome)
            .map_err(|_| RunnerError::Execution)
    }
}

#[async_trait::async_trait]
impl JobExecutor for MediaJobExecutor {
    async fn execute(
        &self,
        lease: &LeaseDto,
        control: &RunnerControl,
    ) -> Result<ExecutionOutcome, RunnerError> {
        let sticky = match &self.gluetun {
            Some(client) => Some((
                client,
                client
                    .begin_job(lease.job.id.to_string())
                    .await
                    .map_err(|_| RunnerError::Execution)?,
            )),
            None => None,
        };
        let result = self.execute_inner(lease, control).await;
        if let Some((client, sticky)) = sticky {
            client
                .end_job(sticky)
                .await
                .map_err(|_| RunnerError::Execution)?;
        }
        result
    }
}

fn safe_name(value: &str) -> String {
    let value = value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, ' ' | '-' | '_') {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        "media".to_owned()
    } else {
        value
    }
}

fn map_pipeline_outcome(outcome: media_runner::EpisodeOutcome) -> ExecutionOutcome {
    match outcome {
        media_runner::EpisodeOutcome::Completed => ExecutionOutcome::Completed,
        media_runner::EpisodeOutcome::Partial { .. } => ExecutionOutcome::Partial,
        media_runner::EpisodeOutcome::BlockedStorage => ExecutionOutcome::BlockedStorage,
        media_runner::EpisodeOutcome::PlexPending => ExecutionOutcome::PlexPending,
        media_runner::EpisodeOutcome::NeedsActionPlexMismatch => {
            ExecutionOutcome::NeedsActionPlexMismatch
        }
        media_runner::EpisodeOutcome::Cancelled => ExecutionOutcome::Cancelled,
    }
}

pub async fn run_single_iteration(
    api: Arc<dyn RunnerApi>,
    executor: Arc<dyn JobExecutor>,
    heartbeat_interval: Duration,
) -> Result<bool, RunnerError> {
    let Some(lease) = api.lease_next().await? else {
        return Ok(false);
    };
    if lease.execution.is_none() {
        return Err(RunnerError::Execution);
    }
    api.report(&lease, RunnerEventDto::Started).await?;
    let cancelled = Arc::new(AtomicBool::new(matches!(
        lease.job.state,
        JobStateDto::CancelRequested | JobStateDto::Cancelled
    )));
    let finished = Arc::new(AtomicBool::new(false));
    let heartbeat_api = api.clone();
    let heartbeat_lease = lease.clone();
    let heartbeat_cancelled = cancelled.clone();
    let heartbeat_finished = finished.clone();
    let heartbeat = tokio::spawn(async move {
        while !heartbeat_finished.load(Ordering::SeqCst) {
            let current = heartbeat_api.heartbeat(&heartbeat_lease).await?;
            if matches!(
                current.job.state,
                JobStateDto::CancelRequested | JobStateDto::Cancelled
            ) {
                heartbeat_cancelled.store(true, Ordering::SeqCst);
            }
            tokio::time::sleep(heartbeat_interval).await;
        }
        Ok::<(), RunnerError>(())
    });
    let control = RunnerControl {
        api: api.clone(),
        lease: lease.clone(),
        cancelled,
    };
    let outcome = executor.execute(&lease, &control).await;
    finished.store(true, Ordering::SeqCst);
    heartbeat.await.map_err(|_| RunnerError::Execution)??;
    let outcome = outcome?;
    let (state, reason) = match outcome {
        ExecutionOutcome::Completed => (JobStateDto::Completed, None),
        ExecutionOutcome::Partial => (JobStateDto::Partial, None),
        ExecutionOutcome::BlockedStorage => (JobStateDto::BlockedStorage, None),
        ExecutionOutcome::PlexPending => (JobStateDto::PlexPending, None),
        ExecutionOutcome::NeedsActionPlexMismatch => (
            JobStateDto::NeedsAction,
            Some(NeedsActionReasonDto::PlexMismatch),
        ),
        ExecutionOutcome::Cancelled => (JobStateDto::Cancelled, None),
        ExecutionOutcome::Failed => (JobStateDto::Failed, None),
    };
    api.report(
        &lease,
        RunnerEventDto::JobTransition {
            state,
            needs_action_reason: reason,
        },
    )
    .await?;
    Ok(true)
}

pub async fn run_loop(
    api: Arc<dyn RunnerApi>,
    executor: Arc<dyn JobExecutor>,
    heartbeat_interval: Duration,
) -> Result<(), RunnerError> {
    loop {
        if !run_single_iteration(api.clone(), executor.clone(), heartbeat_interval).await? {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

pub struct HttpRunnerApi {
    client: reqwest::Client,
    service_url: reqwest::Url,
    token: SecretString,
}

impl HttpRunnerApi {
    pub fn new(config: ClientConfig) -> Result<Self, RunnerError> {
        let (service_url, token) = config.into_parts();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| RunnerError::Configuration)?;
        Ok(Self {
            client,
            service_url,
            token,
        })
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, RunnerError> {
        let url = self
            .service_url
            .join(path)
            .map_err(|_| RunnerError::Configuration)?;
        Ok(self
            .client
            .request(method, url)
            .bearer_auth(self.token.expose_secret())
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("idempotency-key", uuid::Uuid::new_v4().to_string()))
    }

    async fn json(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<Option<serde_json::Value>, RunnerError> {
        let response = request.send().await.map_err(|_| RunnerError::Service)?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(RunnerError::Service);
        }
        response
            .json()
            .await
            .map(Some)
            .map_err(|_| RunnerError::Service)
    }
}

#[async_trait::async_trait]
impl RunnerApi for HttpRunnerApi {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, RunnerError> {
        self.json(
            self.request(reqwest::Method::POST, "v1/runner/leases")?
                .json(&serde_json::json!({})),
        )
        .await?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| RunnerError::Service)
    }

    async fn heartbeat(&self, lease: &LeaseDto) -> Result<LeaseDto, RunnerError> {
        let path = format!("v1/runner/leases/{}/heartbeat", lease.lease_id);
        let value = self
            .json(
                self.request(reqwest::Method::POST, &path)?
                    .json(&serde_json::json!({})),
            )
            .await?
            .ok_or(RunnerError::Service)?;
        serde_json::from_value(value).map_err(|_| RunnerError::Service)
    }

    async fn report(&self, lease: &LeaseDto, event: RunnerEventDto) -> Result<(), RunnerError> {
        let path = format!("v1/runner/leases/{}/events", lease.lease_id);
        let request = RunnerEventRequest {
            event_id: media_contract::PublicId::parse(&uuid::Uuid::new_v4().to_string())
                .map_err(|_| RunnerError::Execution)?,
            event,
        };
        let value = self
            .json(self.request(reqwest::Method::POST, &path)?.json(&request))
            .await?
            .ok_or(RunnerError::Service)?;
        let _: media_contract::RunnerEventResponse =
            serde_json::from_value(value).map_err(|_| RunnerError::Service)?;
        Ok(())
    }
}
