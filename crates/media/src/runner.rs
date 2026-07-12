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
    async fn report(
        &self,
        lease: &LeaseDto,
        event: RunnerEventDto,
    ) -> Result<media_contract::JobDto, RunnerError>;
}

#[async_trait::async_trait]
pub trait JobExecutor: Send + Sync {
    async fn execute(
        &self,
        lease: &LeaseDto,
        control: &RunnerControl,
    ) -> Result<ExecutionOutcome, RunnerError>;

    async fn retire(&self, _lease: &LeaseDto) -> Result<(), RunnerError> {
        Ok(())
    }

    async fn maintain(&self, _protected_job_ids: &[String]) -> Result<(), RunnerError> {
        Ok(())
    }
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

    /// Reports a stage start. Progress events are best-effort: a transient
    /// delivery failure is logged and swallowed so it never fails the job. Only
    /// terminal transitions ([`Self::stage_failed`] and job transitions) are
    /// delivered reliably.
    pub async fn stage_started(
        &self,
        task_ordinal: u32,
        name: &str,
        stage_ordinal: u32,
    ) -> Result<(), RunnerError> {
        if let Err(error) = self
            .api
            .report(
                &self.lease,
                RunnerEventDto::StageStarted {
                    task_ordinal,
                    stage_name: name.to_owned(),
                    stage_ordinal,
                },
            )
            .await
        {
            tracing::warn!(?error, stage_name = name, "failed to report stage start");
        }
        Ok(())
    }

    /// Reports a stage completion. Like [`Self::stage_started`], this is a
    /// best-effort progress event: a transient failure (including a dropped
    /// checkpoint) is logged and swallowed rather than failing the job.
    pub async fn stage_completed(
        &self,
        task_ordinal: u32,
        name: &str,
        stage_ordinal: u32,
    ) -> Result<(), RunnerError> {
        if let Err(error) = self
            .api
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
        {
            tracing::warn!(
                ?error,
                stage_name = name,
                "failed to report stage completion"
            );
        }
        Ok(())
    }

    pub async fn stage_failed(
        &self,
        task_ordinal: u32,
        name: &str,
        stage_ordinal: u32,
        retryable: bool,
        error_code: &str,
    ) -> Result<media_contract::JobDto, RunnerError> {
        self.api
            .report(
                &self.lease,
                RunnerEventDto::StageFailed {
                    task_ordinal,
                    stage_name: name.to_owned(),
                    stage_ordinal,
                    retryable,
                    error_code: error_code.to_owned(),
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

/// Stable stage ordinal for the transcode sub-stage reported from inside the
/// Rezka pipeline. It sorts after the `media_pipeline` umbrella (ordinal 1) it
/// runs under; the service keys the stage row on (task, name), so this only
/// needs to stay constant across retries of the same episode.
const TRANSCODE_STAGE_ORDINAL: u32 = 2;

/// Adapts [`RunnerControl`] to the pipeline's [`media_runner::StageReporter`]
/// port, binding each reported sub-stage to the current episode's task ordinal.
struct ControlStageReporter<'a> {
    control: &'a RunnerControl,
    task_ordinal: u32,
}

#[async_trait::async_trait]
impl media_runner::StageReporter for ControlStageReporter<'_> {
    async fn stage_started(&self, stage_name: &str) {
        // stage_started is already best-effort (it logs and swallows delivery
        // errors), so pipeline progress can never fail the job.
        let _ = self
            .control
            .stage_started(self.task_ordinal, stage_name, TRANSCODE_STAGE_ORDINAL)
            .await;
    }
}

pub struct MediaJobExecutor {
    rezka: tokio::sync::Mutex<crate::composition::PreparedRunnerSession>,
    pipeline: media_runner::EpisodePipeline,
    qbittorrent: Option<Arc<media_integrations::qbittorrent::QbittorrentClient>>,
    torrent_tv_category: String,
    torrent_movies_category: String,
    gluetun: Option<Arc<media_integrations::gluetun::GluetunClient>>,
    roots: media_runner::StorageRoots,
    vaapi_device: std::path::PathBuf,
}

pub(crate) struct TorrentRouting {
    client: Option<Arc<media_integrations::qbittorrent::QbittorrentClient>>,
    tv_category: String,
    movies_category: String,
}

impl TorrentRouting {
    pub(crate) fn new(
        client: Option<Arc<media_integrations::qbittorrent::QbittorrentClient>>,
        tv_category: String,
        movies_category: String,
    ) -> Self {
        Self {
            client,
            tv_category,
            movies_category,
        }
    }
}

struct TorrentExecution<'a> {
    source_identity: &'a str,
    info_hash: &'a str,
    uri: &'a str,
    media_kind: media_contract::MediaKindDto,
    season: Option<u16>,
    title: &'a str,
}

impl MediaJobExecutor {
    #[must_use]
    pub(crate) fn new(
        rezka: crate::composition::PreparedRunnerSession,
        pipeline: media_runner::EpisodePipeline,
        torrent: TorrentRouting,
        gluetun: Option<Arc<media_integrations::gluetun::GluetunClient>>,
        roots: media_runner::StorageRoots,
        vaapi_device: std::path::PathBuf,
    ) -> Self {
        Self {
            rezka: tokio::sync::Mutex::new(rezka),
            pipeline,
            qbittorrent: torrent.client,
            torrent_tv_category: torrent.tv_category,
            torrent_movies_category: torrent.movies_category,
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
                episodes,
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
                    episodes,
                    title,
                )
                .await
            }
            media_contract::ExecutionSelectionDto::Prowlarr {
                source_identity,
                info_hash,
                uri,
                media_kind,
                season,
                title,
            } => {
                let request = TorrentExecution {
                    source_identity,
                    info_hash,
                    uri,
                    media_kind: *media_kind,
                    season: *season,
                    title,
                };
                self.execute_torrent(lease, control, request).await
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
        episodes: &[media_contract::EpisodeSnapshotDto],
        title: &str,
    ) -> Result<ExecutionOutcome, RunnerError> {
        let mut prepared = self.rezka.lock().await;
        let crate::composition::PreparedRunnerSession {
            client,
            credentials,
            probe,
            store: _,
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
        let requests = match media_kind {
            media_contract::MediaKindDto::Movie => vec![(
                None,
                None,
                selection
                    .movie_request()
                    .map_err(|_| RunnerError::Execution)?,
            )],
            media_contract::MediaKindDto::Series => {
                let availability = client
                    .series_availability(&selection)
                    .await
                    .map_err(|_| RunnerError::Execution)?;
                let targets = if !episodes.is_empty() {
                    episodes
                        .iter()
                        .map(|episode| (episode.season, episode.episode))
                        .collect()
                } else {
                    match (season, episode) {
                        (Some(season), Some(episode)) => vec![(season, episode)],
                        (None, None) => availability
                            .seasons()
                            .iter()
                            .flat_map(|season| {
                                season
                                    .episodes()
                                    .iter()
                                    .map(move |episode| (season.number(), episode.number()))
                            })
                            .collect(),
                        _ => return Err(RunnerError::Execution),
                    }
                };
                let requests = targets
                    .into_iter()
                    .map(|(season, episode)| {
                        availability
                            .select_episode(season, episode)
                            .map(|selection| {
                                (Some(season), Some(episode), selection.playback_request())
                            })
                            .map_err(|_| RunnerError::Execution)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if requests.is_empty() {
                    return Err(RunnerError::Execution);
                }
                requests
            }
        };
        drop(prepared);

        let mut aggregate = ExecutionOutcome::Completed;
        for (task_ordinal, (season, episode, request)) in requests.into_iter().enumerate() {
            let task_ordinal = u32::try_from(task_ordinal).map_err(|_| RunnerError::Execution)?;
            control
                .stage_started(task_ordinal, "resolve_manifest", 0)
                .await?;
            let mut prepared = self.rezka.lock().await;
            let manifest = prepared
                .client
                .resolve(request)
                .await
                .map_err(|_| RunnerError::Execution)?;
            let snapshot = prepared
                .client
                .export_session()
                .map_err(|_| RunnerError::Execution)?;
            prepared
                .store
                .save(&snapshot)
                .map_err(|_| RunnerError::Execution)?;
            drop(prepared);
            control
                .stage_completed(task_ordinal, "resolve_manifest", 0)
                .await?;

            let work = self.rezka_work(lease, &manifest, title, season, episode)?;
            control
                .stage_started(task_ordinal, "media_pipeline", 1)
                .await?;
            let reporter = ControlStageReporter {
                control,
                task_ordinal,
            };
            let outcome = self
                .pipeline
                .run(&work, control, &reporter)
                .await
                .map_err(|_| RunnerError::Execution)?;
            control
                .stage_completed(task_ordinal, "media_pipeline", 1)
                .await?;
            aggregate = combine_episode_outcome(aggregate, map_pipeline_outcome(outcome));
            if !matches!(
                aggregate,
                ExecutionOutcome::Completed | ExecutionOutcome::Partial
            ) {
                break;
            }
        }
        Ok(aggregate)
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
        request: TorrentExecution<'_>,
    ) -> Result<ExecutionOutcome, RunnerError> {
        let client = self
            .qbittorrent
            .as_ref()
            .ok_or(RunnerError::Configuration)?;
        control.stage_started(0, "torrent_submit", 0).await?;
        let selection = media_integrations::qbittorrent::ExplicitTorrentSelection::new(
            request.source_identity,
            request.info_hash,
            request.uri,
        )
        .map_err(|_| RunnerError::Execution)?;
        let category = match request.media_kind {
            media_contract::MediaKindDto::Movie => &self.torrent_movies_category,
            media_contract::MediaKindDto::Series => &self.torrent_tv_category,
        };
        let handle = client
            .submit_selected_to_category(selection, category)
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
        control.stage_completed(0, "torrent_monitor", 1).await?;
        let mut videos = content
            .files
            .into_iter()
            .filter(|path| is_video_path(path))
            .collect::<Vec<_>>();
        if videos.is_empty() && is_video_path(&content.root) {
            videos.push(content.root);
        }
        videos.sort();
        if videos.is_empty() {
            return Ok(ExecutionOutcome::NeedsActionPlexMismatch);
        }
        let expected_season = request.season.map(u32::from);
        let work_items = match request.media_kind {
            media_contract::MediaKindDto::Movie => {
                videos.truncate(1);
                videos
                    .into_iter()
                    .map(|path| (path, None))
                    .collect::<Vec<_>>()
            }
            media_contract::MediaKindDto::Series => {
                let Some(expected_season) = expected_season else {
                    return Ok(ExecutionOutcome::NeedsActionPlexMismatch);
                };
                let items = matching_episode_videos(videos, expected_season)
                    .into_iter()
                    .map(|(path, coordinates)| (path, Some(coordinates)))
                    .collect::<Vec<_>>();
                if items.is_empty() {
                    return Ok(ExecutionOutcome::NeedsActionPlexMismatch);
                }
                items
            }
        };
        let mut aggregate = ExecutionOutcome::Completed;
        for (index, (final_video, coordinates)) in work_items.into_iter().enumerate() {
            let task_ordinal = u32::try_from(index + 1).map_err(|_| RunnerError::Execution)?;
            control
                .stage_started(task_ordinal, "plex_reconcile", 0)
                .await?;
            let staging = self
                .roots
                .staging()
                .join(lease.job.id.to_string())
                .join(format!("torrent-{task_ordinal}"));
            let (season, episode) = coordinates.unzip();
            let work = media_runner::EpisodeWork {
                provider: media_runner::ProviderKind::Torrent,
                job_id: lease.job.id.to_string(),
                episode_id: coordinates.map_or_else(
                    || "movie".to_owned(),
                    |(season, episode)| format!("s{season:02}e{episode:02}"),
                ),
                source_url: None,
                staging_directory: staging.clone(),
                source_partial: staging.join("unused.source"),
                encoded_partial: staging.join("unused.encoded"),
                final_video: final_video.clone(),
                vaapi_device: self.vaapi_device.clone(),
                subtitles: Vec::new(),
                plex: media_runner::PlexExpectation {
                    path: final_video,
                    canonical_id: format!("prowlarr://{}", request.source_identity),
                    season,
                    episode,
                },
            };
            // Torrent work reconciles Plex without a transcode step, so no
            // pipeline sub-stage is reported.
            let outcome = self
                .pipeline
                .run(&work, control, &())
                .await
                .map_err(|_| RunnerError::Execution)?;
            control
                .stage_completed(task_ordinal, "plex_reconcile", 0)
                .await?;
            aggregate = combine_episode_outcome(aggregate, map_pipeline_outcome(outcome));
            if !matches!(
                aggregate,
                ExecutionOutcome::Completed | ExecutionOutcome::Partial
            ) {
                break;
            }
        }
        let _ = request.title;
        Ok(aggregate)
    }
}

#[async_trait::async_trait]
impl JobExecutor for MediaJobExecutor {
    async fn execute(
        &self,
        lease: &LeaseDto,
        control: &RunnerControl,
    ) -> Result<ExecutionOutcome, RunnerError> {
        let current_job_id = lease.job.id.to_string();
        let uses_rezka_vpn = matches!(
            &lease.execution,
            Some(media_contract::ExecutionSelectionDto::Rezka { .. })
        );
        if uses_rezka_vpn {
            // Only create the staging directory here. Stamping it terminal is
            // deferred to `retire`, on a completed or terminal outcome, so
            // retention never counts an in-progress job's staging as terminal
            // from its start time.
            tokio::fs::create_dir_all(self.roots.staging().join(&current_job_id))
                .await
                .map_err(|_| RunnerError::Execution)?;
        }
        match media_runner::cleanup_terminal_staging(
            self.roots.staging(),
            std::time::SystemTime::now(),
            Duration::from_secs(7 * 24 * 60 * 60),
            &[current_job_id.as_str()],
        )
        .await
        {
            Ok(removed) if !removed.is_empty() => {
                tracing::info!(
                    directories_removed = removed.len(),
                    "expired staging removed"
                );
            }
            Ok(_) => {}
            Err(_) => tracing::warn!("staging retention pass failed"),
        }
        let sticky = match (&self.gluetun, uses_rezka_vpn) {
            (Some(client), true) => Some((
                client,
                client
                    .begin_job(lease.job.id.to_string())
                    .await
                    .map_err(|_| RunnerError::Execution)?,
            )),
            _ => None,
        };
        let result = self.execute_inner(lease, control).await;
        if let Some((client, sticky)) = sticky {
            if client.end_job(sticky).await.is_err() {
                tracing::warn!("failed to end the sticky Rezka VPN job");
            }
            if client.rotate_between_jobs().await.is_err() {
                tracing::warn!("failed to rotate the Rezka VPN between jobs");
            }
        }
        result
    }

    async fn retire(&self, lease: &LeaseDto) -> Result<(), RunnerError> {
        let job_id = lease.job.id.to_string();
        let result = media_runner::mark_terminal(
            self.roots.staging(),
            &job_id,
            std::time::SystemTime::now(),
        )
        .await;
        result.map_err(|_| RunnerError::Execution)
    }

    async fn maintain(&self, protected_job_ids: &[String]) -> Result<(), RunnerError> {
        let protected = protected_job_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let removed = media_runner::cleanup_terminal_staging(
            self.roots.staging(),
            std::time::SystemTime::now(),
            Duration::from_secs(7 * 24 * 60 * 60),
            &protected,
        )
        .await
        .map_err(|_| RunnerError::Execution)?;
        if !removed.is_empty() {
            tracing::info!(
                directories_removed = removed.len(),
                "expired staging removed"
            );
        }
        Ok(())
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

fn is_video_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mkv" | "mp4" | "avi"
            )
        })
}

fn parse_episode_coordinates(path: &std::path::Path) -> Option<(u32, u32)> {
    let name = path.file_name()?.to_str()?.to_ascii_uppercase();
    let bytes = name.as_bytes();
    for start in 0..bytes.len() {
        if bytes[start] != b'S' {
            continue;
        }
        let season_start = start + 1;
        let Some(e_offset) = bytes[season_start..].iter().position(|byte| *byte == b'E') else {
            continue;
        };
        let episode_marker = season_start + e_offset;
        if !(1..=3).contains(&e_offset) {
            continue;
        }
        let episode_start = episode_marker + 1;
        let episode_len = bytes[episode_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .take(3)
            .count();
        if episode_len == 0 {
            continue;
        }
        let season = name[season_start..episode_marker].parse::<u32>().ok()?;
        let episode = name[episode_start..episode_start + episode_len]
            .parse::<u32>()
            .ok()?;
        if season > 0 && episode > 0 {
            return Some((season, episode));
        }
    }
    None
}

fn matching_episode_videos(
    videos: Vec<std::path::PathBuf>,
    expected_season: u32,
) -> Vec<(std::path::PathBuf, (u32, u32))> {
    videos
        .into_iter()
        .filter_map(|path| {
            let coordinates = parse_episode_coordinates(&path)?;
            (coordinates.0 == expected_season).then_some((path, coordinates))
        })
        .collect()
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

fn combine_episode_outcome(
    aggregate: ExecutionOutcome,
    current: ExecutionOutcome,
) -> ExecutionOutcome {
    match (aggregate, current) {
        (_, ExecutionOutcome::Cancelled) => ExecutionOutcome::Cancelled,
        (_, ExecutionOutcome::NeedsActionPlexMismatch) => ExecutionOutcome::NeedsActionPlexMismatch,
        (_, ExecutionOutcome::BlockedStorage) => ExecutionOutcome::BlockedStorage,
        (_, ExecutionOutcome::PlexPending) => ExecutionOutcome::PlexPending,
        (_, ExecutionOutcome::Failed) => ExecutionOutcome::Failed,
        (ExecutionOutcome::Partial, ExecutionOutcome::Completed)
        | (ExecutionOutcome::Completed, ExecutionOutcome::Partial)
        | (ExecutionOutcome::Partial, ExecutionOutcome::Partial) => ExecutionOutcome::Partial,
        (aggregate, ExecutionOutcome::Partial | ExecutionOutcome::Completed) => aggregate,
    }
}

/// Consecutive heartbeat failures tolerated before the lease is treated as lost
/// and execution is cancelled cooperatively. Paired with [`heartbeat_retry_backoff`]
/// the accumulated delay spans roughly a lease TTL before giving up.
const MAX_HEARTBEAT_FAILURES: u32 = 5;

/// Initial delay for retrying a failed runner-loop iteration.
const RUN_LOOP_INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Upper bound for the runner-loop retry backoff.
const RUN_LOOP_MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Backoff between heartbeat retries after a transient failure. Retries start
/// below the steady cadence so a brief blip recovers quickly, then grow and are
/// capped at the heartbeat interval.
fn heartbeat_retry_backoff(consecutive_failures: u32, interval: Duration) -> Duration {
    let base = (interval / 4).max(Duration::from_millis(1));
    let factor = 1u32 << (consecutive_failures.saturating_sub(1)).min(5);
    base.saturating_mul(factor).min(interval.max(base))
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
    let _ = api.report(&lease, RunnerEventDto::Started).await?;
    let cancelled = Arc::new(AtomicBool::new(matches!(
        lease.job.state,
        JobStateDto::CancelRequested | JobStateDto::Cancelled
    )));
    let finished = Arc::new(tokio::sync::Notify::new());
    let heartbeat_api = api.clone();
    let heartbeat_lease = lease.clone();
    let heartbeat_cancelled = cancelled.clone();
    let heartbeat_finished = finished.clone();
    let heartbeat = tokio::spawn(async move {
        let mut consecutive_failures: u32 = 0;
        loop {
            match heartbeat_api.heartbeat(&heartbeat_lease).await {
                Ok(current) => {
                    consecutive_failures = 0;
                    if matches!(
                        current.job.state,
                        JobStateDto::CancelRequested | JobStateDto::Cancelled
                    ) {
                        heartbeat_cancelled.store(true, Ordering::SeqCst);
                    }
                }
                Err(_) => {
                    consecutive_failures += 1;
                    // Retrying a transient failure keeps the lease alive across a
                    // brief service blip. Once failures pile up far enough that
                    // the lease is effectively lost, the service may re-lease the
                    // job, so cancel execution cooperatively to avoid running it
                    // twice, then stop heartbeating.
                    if consecutive_failures >= MAX_HEARTBEAT_FAILURES {
                        heartbeat_cancelled.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
            let wait = if consecutive_failures == 0 {
                heartbeat_interval
            } else {
                heartbeat_retry_backoff(consecutive_failures, heartbeat_interval)
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = heartbeat_finished.notified() => break,
            }
        }
    });
    let control = RunnerControl {
        api: api.clone(),
        lease: lease.clone(),
        cancelled,
    };
    control.stage_started(0, "execution", 2).await?;
    let outcome = executor.execute(&lease, &control).await;
    // Wake the heartbeat task immediately instead of waiting out its sleep.
    finished.notify_one();
    let _ = heartbeat.await;
    let outcome = match outcome {
        Ok(outcome) => {
            control.stage_completed(0, "execution", 2).await?;
            outcome
        }
        Err(_) => {
            let job = control
                .stage_failed(0, "execution", 2, true, "execution_failed")
                .await?;
            if job.state == JobStateDto::Failed && executor.retire(&lease).await.is_err() {
                tracing::warn!("failed to mark terminal staging for retention");
            }
            return Ok(true);
        }
    };
    let retire_staging = matches!(
        outcome,
        ExecutionOutcome::Completed | ExecutionOutcome::Partial | ExecutionOutcome::Cancelled
    );
    let transitions = match outcome {
        ExecutionOutcome::Completed => vec![
            (JobStateDto::Publishing, None),
            (JobStateDto::PlexPending, None),
            (JobStateDto::Completed, None),
        ],
        ExecutionOutcome::Partial => vec![
            (JobStateDto::Publishing, None),
            (JobStateDto::PlexPending, None),
            (JobStateDto::Partial, None),
        ],
        ExecutionOutcome::BlockedStorage => vec![(JobStateDto::BlockedStorage, None)],
        ExecutionOutcome::PlexPending => vec![
            (JobStateDto::Publishing, None),
            (JobStateDto::PlexPending, None),
        ],
        ExecutionOutcome::NeedsActionPlexMismatch => vec![
            (JobStateDto::Publishing, None),
            (JobStateDto::PlexPending, None),
            (
                JobStateDto::NeedsAction,
                Some(NeedsActionReasonDto::PlexMismatch),
            ),
        ],
        ExecutionOutcome::Cancelled => vec![(JobStateDto::Cancelled, None)],
        ExecutionOutcome::Failed => vec![(JobStateDto::Failed, None)],
    };
    for (state, needs_action_reason) in transitions {
        let _ = api
            .report(
                &lease,
                RunnerEventDto::JobTransition {
                    state,
                    needs_action_reason,
                },
            )
            .await?;
    }
    if retire_staging && executor.retire(&lease).await.is_err() {
        tracing::warn!("failed to mark terminal staging for retention");
    }
    Ok(true)
}

pub async fn run_loop(
    api: Arc<dyn RunnerApi>,
    executor: Arc<dyn JobExecutor>,
    heartbeat_interval: Duration,
) -> Result<(), RunnerError> {
    let mut next_maintenance = tokio::time::Instant::now();
    let mut backoff = RUN_LOOP_INITIAL_BACKOFF;
    loop {
        let worked =
            match run_single_iteration(api.clone(), executor.clone(), heartbeat_interval).await {
                Ok(worked) => {
                    backoff = RUN_LOOP_INITIAL_BACKOFF;
                    worked
                }
                // A misconfigured runner fails identically on every iteration, so
                // stop rather than spin.
                Err(RunnerError::Configuration) => return Err(RunnerError::Configuration),
                // Transient service or execution errors (leasing, reporting the
                // Started event, joining the heartbeat task, terminal transition
                // reporting) must not tear down the long-lived runner process. Log,
                // back off, and retry the loop.
                Err(error) => {
                    tracing::warn!(?error, "runner iteration failed; retrying after backoff");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RUN_LOOP_MAX_BACKOFF);
                    continue;
                }
            };
        if tokio::time::Instant::now() >= next_maintenance {
            if executor.maintain(&[]).await.is_err() {
                tracing::warn!("staging retention pass failed");
            }
            next_maintenance = tokio::time::Instant::now() + Duration::from_secs(60 * 60);
        }
        if !worked {
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

    async fn report(
        &self,
        lease: &LeaseDto,
        event: RunnerEventDto,
    ) -> Result<media_contract::JobDto, RunnerError> {
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
        let response: media_contract::RunnerEventResponse =
            serde_json::from_value(value).map_err(|_| RunnerError::Service)?;
        Ok(response.job)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ExecutionOutcome, combine_episode_outcome, matching_episode_videos,
        parse_episode_coordinates,
    };

    #[test]
    fn series_outcomes_preserve_partial_and_stop_on_blocking_states() {
        assert_eq!(
            combine_episode_outcome(ExecutionOutcome::Completed, ExecutionOutcome::Partial),
            ExecutionOutcome::Partial,
        );
        assert_eq!(
            combine_episode_outcome(ExecutionOutcome::Partial, ExecutionOutcome::Completed),
            ExecutionOutcome::Partial,
        );
        assert_eq!(
            combine_episode_outcome(ExecutionOutcome::Partial, ExecutionOutcome::BlockedStorage),
            ExecutionOutcome::BlockedStorage,
        );
    }

    #[test]
    fn torrent_episode_coordinates_are_parsed_without_guessing_ambiguous_names() {
        assert_eq!(
            parse_episode_coordinates(Path::new("Show.Name.S02E04.1080p.mkv")),
            Some((2, 4))
        );
        assert_eq!(
            parse_episode_coordinates(Path::new("show s1e12.mp4")),
            Some((1, 12))
        );
        assert_eq!(parse_episode_coordinates(Path::new("Episode 04.mkv")), None);
    }

    #[test]
    fn season_pack_ignores_samples_extras_and_other_seasons() {
        let matched = matching_episode_videos(
            vec![
                "Show.S02E01.mkv".into(),
                "sample.mkv".into(),
                "Show.S01E09.mkv".into(),
                "extras.mp4".into(),
                "Show.S02E02.mkv".into(),
            ],
            2,
        );

        assert_eq!(
            matched
                .iter()
                .map(|(path, coordinates)| (path.to_string_lossy().into_owned(), *coordinates))
                .collect::<Vec<_>>(),
            vec![
                ("Show.S02E01.mkv".to_owned(), (2, 1)),
                ("Show.S02E02.mkv".to_owned(), (2, 2)),
            ]
        );
    }
}
