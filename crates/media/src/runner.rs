use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use media_contract::{
    ApiError, ApiErrorCode, CheckpointValueDto, JobStateDto, LeaseDto, NeedsActionReasonDto,
    RunnerEventDto, RunnerEventRequest,
};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::io::AsyncWriteExt as _;
use unicode_normalization::UnicodeNormalization as _;

use crate::config::ClientConfig;

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum RunnerError {
    #[error("runner configuration is invalid")]
    Configuration,
    #[error("runner service request failed")]
    Service,
    #[error("VPN rotation is required before another lease")]
    RotationRequired,
    #[error("runner execution failed")]
    Execution,
    #[error("source stream expired")]
    SourceExpired,
    #[error("source transfer failed transiently")]
    SourceTransferTransient,
    #[error("source transfer was rejected")]
    SourceTransferRejected,
    #[error("Rezka translation requires premium access")]
    RezkaPremiumRequired,
    #[error("runner task stage failed")]
    TaskStage {
        task_ordinal: u32,
        stage_name: &'static str,
        stage_ordinal: u32,
        failure: RunnerFailureKind,
    },
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RunnerFailureKind {
    Execution,
    SourceExpired,
    SourceTransferTransient,
    SourceTransferRejected,
    RezkaPremiumRequired,
}

impl RunnerError {
    const fn stage_failure(self) -> (bool, &'static str) {
        match self {
            Self::Configuration => (false, "runner_configuration_invalid"),
            Self::Service => (true, "runner_service_unavailable"),
            Self::RotationRequired => (false, "vpn_rotation_required"),
            Self::Execution => (true, "execution_failed"),
            Self::SourceExpired => (true, "stream_expired"),
            Self::SourceTransferTransient => (true, "source_transfer_transient"),
            Self::SourceTransferRejected => (false, "source_transfer_rejected"),
            Self::RezkaPremiumRequired => (false, "rezka_premium_required"),
            Self::TaskStage { failure, .. } => failure.stage_failure(),
        }
    }

    #[must_use]
    pub const fn at_stage(
        self,
        task_ordinal: u32,
        stage_name: &'static str,
        stage_ordinal: u32,
    ) -> Self {
        let failure = match self {
            Self::SourceExpired => RunnerFailureKind::SourceExpired,
            Self::SourceTransferTransient => RunnerFailureKind::SourceTransferTransient,
            Self::SourceTransferRejected => RunnerFailureKind::SourceTransferRejected,
            Self::RezkaPremiumRequired => RunnerFailureKind::RezkaPremiumRequired,
            _ => RunnerFailureKind::Execution,
        };
        Self::TaskStage {
            task_ordinal,
            stage_name,
            stage_ordinal,
            failure,
        }
    }

    const fn failure_stage(self) -> (u32, &'static str, u32) {
        match self {
            Self::TaskStage {
                task_ordinal,
                stage_name,
                stage_ordinal,
                ..
            } => (task_ordinal, stage_name, stage_ordinal),
            _ => (EXECUTION_TASK_ORDINAL, "execution", EXECUTION_STAGE_ORDINAL),
        }
    }
}

impl RunnerFailureKind {
    const fn stage_failure(self) -> (bool, &'static str) {
        match self {
            Self::Execution => (true, "execution_failed"),
            Self::SourceExpired => (true, "stream_expired"),
            Self::SourceTransferTransient => (true, "source_transfer_transient"),
            Self::SourceTransferRejected => (false, "source_transfer_rejected"),
            Self::RezkaPremiumRequired => (false, "rezka_premium_required"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ExecutionOutcome {
    Completed,
    Partial,
    BlockedStorage {
        available_bytes: u64,
        required_bytes: u64,
    },
    PlexPending,
    NeedsActionPlexMismatch,
    NeedsActionIdentityAmbiguous,
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

    async fn cleanup_cancelled(&self, _lease: &LeaseDto) -> Result<(), RunnerError> {
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
        self.stage_completed_with_checkpoint(task_ordinal, name, stage_ordinal, Default::default())
            .await
    }

    async fn stage_completed_with_checkpoint(
        &self,
        task_ordinal: u32,
        name: &str,
        stage_ordinal: u32,
        checkpoint: std::collections::BTreeMap<String, CheckpointValueDto>,
    ) -> Result<(), RunnerError> {
        if let Err(error) = self
            .api
            .report(
                &self.lease,
                RunnerEventDto::StageCompleted {
                    task_ordinal,
                    stage_name: name.to_owned(),
                    stage_ordinal,
                    checkpoint,
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

    /// Persists a best-effort progress observation for a running stage. A
    /// checkpoint transport failure is intentionally swallowed so telemetry
    /// can never interrupt the transfer it describes.
    pub async fn stage_checkpoint(
        &self,
        task_ordinal: u32,
        name: &str,
        stage_ordinal: u32,
        checkpoint: std::collections::BTreeMap<String, CheckpointValueDto>,
    ) -> Result<(), RunnerError> {
        if let Err(error) = self
            .api
            .report(
                &self.lease,
                RunnerEventDto::StageCheckpoint {
                    task_ordinal,
                    stage_name: name.to_owned(),
                    stage_ordinal,
                    checkpoint,
                },
            )
            .await
        {
            tracing::warn!(
                ?error,
                stage_name = name,
                "failed to report stage checkpoint"
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

/// Stable stage ordinal for the actual source transfer. Keep transcode at its
/// historical ordinal 2 so in-flight jobs can resume, and place download at 3
/// to satisfy the storage `UNIQUE (task_id, ordinal)` constraint.
const DOWNLOAD_STAGE_ORDINAL: u32 = 3;

/// Reserved task ordinal for the job-level execution wrapper. It must not share
/// task 0 with the first movie/episode, otherwise a retry of a later episode can
/// overwrite the already completed first task with the wrapper's failure.
const EXECUTION_TASK_ORDINAL: u32 = 2_000_000_000;

/// Stage ordinal for the internal job-level "execution" wrapper. It is
/// deliberately placed in a reserved high band, well above
/// any pipeline sub-stage ordinal (`resolve_manifest` 0, `media_pipeline` 1,
/// `transcode` 2, `download` 3, torrent stages 0/1), so it can never collide with a real
/// sub-stage under the `job_stages` `UNIQUE (task_id, ordinal)` constraint — a
/// collision at ordinal 2 previously rolled back and silently dropped the
/// transcode milestone for every movie and first episode. The service keys the
/// stage row on (task, name), so the exact value only has to stay constant and
/// non-colliding; `current_stage` excludes this wrapper by name.
const EXECUTION_STAGE_ORDINAL: u32 = 1_000_000;

// Compile-time guard: the execution wrapper must never share an ordinal with a
// task-0 pipeline sub-stage (resolve_manifest 0, media_pipeline 1, transcode 2),
// or the transcode `stage_started` would violate the job_stages
// UNIQUE(task_id, ordinal) constraint and be silently dropped (its reporter is
// best-effort) for every movie and first episode.
const _: () = assert!(EXECUTION_STAGE_ORDINAL != TRANSCODE_STAGE_ORDINAL);
const _: () = assert!(EXECUTION_STAGE_ORDINAL != DOWNLOAD_STAGE_ORDINAL);
const _: () = assert!(TRANSCODE_STAGE_ORDINAL != DOWNLOAD_STAGE_ORDINAL);
const _: () = assert!(EXECUTION_STAGE_ORDINAL > 3);

/// Retention window for runner-owned staging directories. A directory stamped
/// terminal is removed this long after retirement; an unstamped directory (a
/// non-terminal outcome or a dead runner) is swept once it has seen no activity
/// for the same window. The window is far longer than any job can run, so an
/// in-progress download or transcode is never mistaken for an orphan.
const STAGING_RETENTION_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const PROGRESS_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
struct ProgressCheckpointGate {
    last_reported_at: Option<tokio::time::Instant>,
    last_state: Option<String>,
}

impl ProgressCheckpointGate {
    fn should_report(
        &mut self,
        now: tokio::time::Instant,
        state: &str,
        final_observation: bool,
    ) -> bool {
        let state_changed = self.last_state.as_deref() != Some(state);
        let interval_elapsed = self
            .last_reported_at
            .is_none_or(|last| now.duration_since(last) >= PROGRESS_CHECKPOINT_INTERVAL);
        if !(final_observation || state_changed || interval_elapsed) {
            return false;
        }
        self.last_reported_at = Some(now);
        self.last_state = Some(state.to_owned());
        true
    }
}

/// Adapts [`RunnerControl`] to the pipeline's [`media_runner::StageReporter`]
/// port, binding each reported sub-stage to the current episode's task ordinal.
struct ControlStageReporter<'a> {
    control: &'a RunnerControl,
    task_ordinal: u32,
    progress_gate: tokio::sync::Mutex<ProgressCheckpointGate>,
}

#[async_trait::async_trait]
impl media_runner::StageReporter for ControlStageReporter<'_> {
    async fn stage_started(&self, stage_name: &str) {
        // stage_started is already best-effort (it logs and swallows delivery
        // errors), so pipeline progress can never fail the job.
        let _ = self
            .control
            .stage_started(
                self.task_ordinal,
                stage_name,
                pipeline_stage_ordinal(stage_name),
            )
            .await;
    }

    async fn stage_completed(&self, stage_name: &str) {
        let _ = self
            .control
            .stage_completed(
                self.task_ordinal,
                stage_name,
                pipeline_stage_ordinal(stage_name),
            )
            .await;
    }

    async fn stage_progress(
        &self,
        stage_name: &str,
        observation: media_runner::TransferObservation,
    ) {
        let should_report = self.progress_gate.lock().await.should_report(
            tokio::time::Instant::now(),
            &observation.state,
            observation.final_observation,
        );
        if should_report {
            let _ = self
                .control
                .stage_checkpoint(
                    self.task_ordinal,
                    stage_name,
                    pipeline_stage_ordinal(stage_name),
                    transfer_checkpoint(&observation),
                )
                .await;
        }
    }
}

fn pipeline_stage_ordinal(stage_name: &str) -> u32 {
    if stage_name == "download" {
        DOWNLOAD_STAGE_ORDINAL
    } else {
        TRANSCODE_STAGE_ORDINAL
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
    episode: Option<u32>,
    title: &'a str,
}

struct RezkaWorkRequest<'a> {
    lease: &'a LeaseDto,
    manifest: &'a rezka_client::PlaybackManifest,
    title: &'a str,
    release_year: Option<u16>,
    season: Option<u32>,
    episode: Option<u32>,
    premium_status: rezka_client::PremiumStatus,
    expected_duration_seconds: Option<f64>,
    translation: &'a str,
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
                episode_mappings,
                ambiguous_episodes,
                library_title,
                library_path_title,
                library_path_aliases,
                title,
                translation: _,
                release_year: _,
                thumbnail_url: _,
            } => {
                if !ambiguous_episodes.is_empty() {
                    return Ok(ExecutionOutcome::NeedsActionIdentityAmbiguous);
                }
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
                    episode_mappings,
                    library_title.as_deref(),
                    library_path_title.as_deref(),
                    library_path_aliases,
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
                episode,
                title,
                ..
            } => {
                let request = TorrentExecution {
                    source_identity,
                    info_hash,
                    uri,
                    media_kind: *media_kind,
                    season: *season,
                    episode: *episode,
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
        episode_mappings: &[media_contract::EpisodeCoordinateMappingDto],
        library_title: Option<&str>,
        library_path_title: Option<&str>,
        library_path_aliases: &[String],
        title: &str,
    ) -> Result<ExecutionOutcome, RunnerError> {
        let mut prepared = self.rezka.lock().await;
        let crate::composition::PreparedRunnerSession {
            client,
            probe,
            store,
            ..
        } = &mut *prepared;
        client
            .ensure_session(probe)
            .await
            .map_err(|_| RunnerError::Execution)?;
        let snapshot = client
            .export_session()
            .map_err(|_| RunnerError::Execution)?;
        store.save(&snapshot).map_err(|_| RunnerError::Execution)?;
        let locator =
            rezka_client::TitleLocator::new(locator).map_err(|_| RunnerError::Execution)?;
        let details = client
            .title(&locator)
            .await
            .map_err(|_| RunnerError::Execution)?;
        if details.id().get() != title_id {
            return Err(RunnerError::Execution);
        }
        let premium_status = client
            .premium_status()
            .await
            .map_err(|_| RunnerError::Execution)?;
        tracing::info!(premium = ?premium_status, "resolved Rezka account status");
        let expected_duration_seconds =
            expected_source_duration_seconds(media_kind, details.duration_minutes());
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
        if selection.translation().is_premium()
            && premium_status != rezka_client::PremiumStatus::Active
        {
            tracing::warn!(
                translation_id = translation_id.get(),
                "refusing premium Rezka translation for a non-premium account"
            );
            return Err(RunnerError::RezkaPremiumRequired);
        }
        let translation = selection.translation().name().to_owned();
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
                        let canonical = episode_mappings
                            .iter()
                            .find(|mapping| {
                                mapping.provider.season == season
                                    && mapping.provider.episode == episode
                            })
                            .map_or((season, episode), |mapping| {
                                (mapping.canonical.season, mapping.canonical.episode)
                            });
                        availability
                            .select_episode(season, episode)
                            .map(|selection| {
                                (
                                    Some(canonical.0),
                                    Some(canonical.1),
                                    selection.playback_request(),
                                )
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

        let resolved_library_path_title = if media_kind == media_contract::MediaKindDto::Series {
            let current = library_path_title.unwrap_or(title);
            Some(
                resolve_existing_series_path_title(
                    self.roots.tv(),
                    &safe_name(current),
                    library_path_aliases,
                )
                .await?,
            )
        } else {
            library_path_title.map(str::to_owned)
        };

        let mut aggregate = ExecutionOutcome::Completed;
        for (task_ordinal, (season, episode, request)) in requests.into_iter().enumerate() {
            let task_ordinal = u32::try_from(task_ordinal).map_err(|_| RunnerError::Execution)?;
            if lease.completed_task_ordinals.contains(&task_ordinal) {
                tracing::info!(
                    task_ordinal,
                    season,
                    episode,
                    "skipping completed media task"
                );
                continue;
            }
            control
                .stage_started(task_ordinal, "resolve_manifest", 0)
                .await?;
            let mut prepared = self.rezka.lock().await;
            let manifest = prepared
                .client
                .resolve(request)
                .await
                .map_err(|error| {
                    tracing::warn!(
                        error_code = ?error.code(),
                        error = %error,
                        "Rezka playback resolve failed"
                    );
                    RunnerError::Execution
                })
                .map_err(|error| error.at_stage(task_ordinal, "resolve_manifest", 0))?;
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

            let mapped_title = season.zip(episode).and_then(|(season, episode)| {
                episode_mappings
                    .iter()
                    .find(|mapping| {
                        mapping.canonical.season == season && mapping.canonical.episode == episode
                    })
                    .map(|mapping| mapping.canonical_title.as_str())
            });
            let work = self.rezka_work(RezkaWorkRequest {
                lease,
                manifest: &manifest,
                title: rezka_physical_title(
                    resolved_library_path_title.as_deref(),
                    mapped_title,
                    library_title,
                    title,
                ),
                release_year: details.release_year(),
                season,
                episode,
                premium_status,
                expected_duration_seconds,
                translation: &translation,
            })?;
            if media_kind == media_contract::MediaKindDto::Series {
                let tmdb_id = library_path_title.and_then(tmdb_id_from_path_title);
                let display_title = resolved_library_path_title
                    .as_deref()
                    .and_then(|path| tmdb_id.and_then(|id| tmdb_display_title_from_path(path, id)))
                    .or(library_title)
                    .or(mapped_title)
                    .unwrap_or(title);
                ensure_plex_match(&work.final_video, display_title, tmdb_id)
                    .await
                    .map_err(|error| error.at_stage(task_ordinal, "media_pipeline", 1))?;
            }
            control
                .stage_started(task_ordinal, "media_pipeline", 1)
                .await?;
            let reporter = ControlStageReporter {
                control,
                task_ordinal,
                progress_gate: tokio::sync::Mutex::new(ProgressCheckpointGate::default()),
            };
            let report = self
                .pipeline
                .run(&work, control, &reporter)
                .await
                .map_err(|error| {
                    tracing::warn!(error = ?error, "Rezka media pipeline failed");
                    map_pipeline_error(error)
                })
                .map_err(|error| error.at_stage(task_ordinal, "media_pipeline", 1))?;
            let mut checkpoint = match &report.outcome {
                media_runner::EpisodeOutcome::BlockedStorage(blocked) => {
                    storage_checkpoint(blocked)
                }
                _ => Default::default(),
            };
            if let Some(artifact) = &report.artifact {
                checkpoint.extend(artifact_checkpoint(artifact));
            }
            control
                .stage_completed_with_checkpoint(task_ordinal, "media_pipeline", 1, checkpoint)
                .await?;
            aggregate = combine_episode_outcome(aggregate, map_pipeline_outcome(report.outcome));
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
        request: RezkaWorkRequest<'_>,
    ) -> Result<media_runner::EpisodeWork, RunnerError> {
        let RezkaWorkRequest {
            lease,
            manifest,
            title,
            release_year,
            season,
            episode,
            premium_status,
            expected_duration_seconds,
            translation,
        } = request;
        let safe_title = safe_name(title);
        let episode_id = season
            .zip(episode)
            .map_or_else(|| "movie".to_owned(), |(s, e)| format!("s{s:02}e{e:02}"));
        let staging = self
            .roots
            .staging()
            .join(lease.job.id.to_string())
            .join(&episode_id);
        let final_video =
            rezka_final_video_path(&self.roots, &safe_title, release_year, season, episode);
        let variant = match premium_status {
            rezka_client::PremiumStatus::Active => manifest.preferred_variant(),
            rezka_client::PremiumStatus::Inactive => {
                highest_standard_variant(manifest.variants()).ok_or(RunnerError::Execution)?
            }
        };
        tracing::info!(
            advertised_height = variant.advertised_quality().vertical_hint(),
            premium = ?premium_status,
            "selected Rezka stream quality"
        );
        let endpoint = variant
            .endpoints()
            .iter()
            .find(|endpoint| endpoint.kind() == rezka_client::StreamKind::Hls)
            .or_else(|| {
                variant
                    .endpoints()
                    .iter()
                    .find(|endpoint| endpoint.kind() == rezka_client::StreamKind::Mp4)
            })
            .ok_or(RunnerError::Execution)?;
        let source_kind = match endpoint.kind() {
            rezka_client::StreamKind::Mp4 => media_runner::VideoSourceKind::Mp4,
            rezka_client::StreamKind::Hls => media_runner::VideoSourceKind::Hls,
        };
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
            source_kind,
            staging_directory: staging.clone(),
            source_partial: staging.join("source.partial.mkv"),
            encoded_partial: staging.join("encoded.partial.mkv"),
            final_video: final_video.clone(),
            vaapi_device: self.vaapi_device.clone(),
            expected_duration_seconds,
            audio: rezka_audio_language(translation).map(|language| {
                media_runner::AudioTrackMetadata {
                    language: language.to_owned(),
                    title: translation.to_owned(),
                }
            }),
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
        let episode_selection = request
            .episode
            .map(|episode| {
                let season = request.season.ok_or(RunnerError::Execution)?;
                media_integrations::qbittorrent::EpisodeFileSelection::new(
                    u32::from(season),
                    episode,
                )
                .map_err(|_| RunnerError::Execution)
            })
            .transpose()?;
        let handle =
            submit_torrent_with_retry(client, selection, category, episode_selection).await?;
        control.stage_completed(0, "torrent_submit", 0).await?;
        control.stage_started(0, "torrent_monitor", 1).await?;
        // qBittorrent 5.2 can acknowledge an add request before the torrent is
        // visible through /torrents/info. Keep that visibility grace bounded.
        let visibility_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let mut progress_gate = ProgressCheckpointGate::default();
        loop {
            if control.is_cancelled() {
                let cleanup = if episode_selection.is_some() {
                    client.remove_managed_episode(&handle).await.map(|_| ())
                } else {
                    client.stop_selected(&handle).await
                };
                if let Err(error) = cleanup {
                    tracing::warn!(
                        error_code = ?error.code(),
                        "failed to stop cancelled torrent"
                    );
                }
                return Ok(ExecutionOutcome::Cancelled);
            }
            let snapshot = match client.monitor(&handle).await {
                Ok(snapshot) => snapshot,
                Err(error)
                    if error.code()
                        == media_integrations::qbittorrent::QbittorrentErrorCode::TorrentNotFound
                        && tokio::time::Instant::now() < visibility_deadline =>
                {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(error_code = ?error.code(), "qBittorrent monitor failed");
                    return Err(RunnerError::Execution);
                }
            };
            let state = torrent_state_name(&snapshot.state);
            let final_observation = matches!(
                snapshot.state,
                media_integrations::qbittorrent::TorrentState::Complete
                    | media_integrations::qbittorrent::TorrentState::Error
            );
            if progress_gate.should_report(tokio::time::Instant::now(), state, final_observation) {
                control
                    .stage_checkpoint(0, "torrent_monitor", 1, torrent_checkpoint(&snapshot))
                    .await?;
            }
            match snapshot.state {
                media_integrations::qbittorrent::TorrentState::Complete => break,
                media_integrations::qbittorrent::TorrentState::Error => {
                    return Ok(ExecutionOutcome::Failed);
                }
                _ => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
        let content = match episode_selection {
            Some(episode) => client.discover_episode_content(&handle, episode).await,
            None => client.discover_content(&handle).await,
        }
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
                let items = torrent_series_work_items(videos, expected_season, request.episode)
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
                source_kind: media_runner::VideoSourceKind::Mp4,
                staging_directory: staging.clone(),
                source_partial: staging.join("unused.source"),
                encoded_partial: staging.join("unused.encoded"),
                final_video: final_video.clone(),
                vaapi_device: self.vaapi_device.clone(),
                expected_duration_seconds: None,
                audio: None,
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
            let report = self
                .pipeline
                .run(&work, control, &())
                .await
                .map_err(|_| RunnerError::Execution)?;
            control
                .stage_completed(task_ordinal, "plex_reconcile", 0)
                .await?;
            aggregate = combine_episode_outcome(aggregate, map_pipeline_outcome(report.outcome));
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

fn torrent_checkpoint(
    snapshot: &media_integrations::qbittorrent::TorrentSnapshot,
) -> std::collections::BTreeMap<String, CheckpointValueDto> {
    let mut checkpoint = std::collections::BTreeMap::from([
        (
            "kind".to_owned(),
            CheckpointValueDto::String("torrent".to_owned()),
        ),
        (
            "state".to_owned(),
            CheckpointValueDto::String(torrent_state_name(&snapshot.state).to_owned()),
        ),
        (
            "progress_percent".to_owned(),
            CheckpointValueDto::Unsigned(u64::from(progress_percent(snapshot.progress))),
        ),
    ]);
    insert_checkpoint_value(
        &mut checkpoint,
        "downloaded_bytes",
        snapshot.downloaded_bytes,
    );
    insert_checkpoint_value(&mut checkpoint, "total_bytes", snapshot.total_bytes);
    insert_checkpoint_value(
        &mut checkpoint,
        "download_speed_bps",
        snapshot.download_speed_bps,
    );
    insert_checkpoint_value(&mut checkpoint, "eta_seconds", snapshot.eta_seconds);
    insert_checkpoint_value(&mut checkpoint, "seeds", snapshot.seeds);
    insert_checkpoint_value(&mut checkpoint, "peers", snapshot.peers);
    checkpoint
}

fn transfer_checkpoint(
    observation: &media_runner::TransferObservation,
) -> std::collections::BTreeMap<String, CheckpointValueDto> {
    let kind = match observation.source {
        media_runner::TransferSource::Direct => "direct",
        media_runner::TransferSource::Hls => "hls",
    };
    let mut checkpoint = std::collections::BTreeMap::from([
        (
            "kind".to_owned(),
            CheckpointValueDto::String(kind.to_owned()),
        ),
        (
            "state".to_owned(),
            CheckpointValueDto::String(observation.state.clone()),
        ),
    ]);
    insert_checkpoint_value(
        &mut checkpoint,
        "progress_percent",
        observation.progress_percent.map(u64::from),
    );
    insert_checkpoint_value(
        &mut checkpoint,
        "downloaded_bytes",
        observation.downloaded_bytes,
    );
    insert_checkpoint_value(&mut checkpoint, "total_bytes", observation.total_bytes);
    insert_checkpoint_value(
        &mut checkpoint,
        "download_speed_bps",
        observation.download_speed_bps,
    );
    insert_checkpoint_value(&mut checkpoint, "eta_seconds", observation.eta_seconds);
    checkpoint
}

fn storage_checkpoint(
    blocked: &media_runner::StorageBlocked,
) -> BTreeMap<String, CheckpointValueDto> {
    BTreeMap::from([
        (
            "storage_available_bytes".to_owned(),
            CheckpointValueDto::Unsigned(blocked.available_bytes()),
        ),
        (
            "storage_required_bytes".to_owned(),
            CheckpointValueDto::Unsigned(blocked.required_bytes()),
        ),
    ])
}

fn artifact_checkpoint(
    artifact: &media_runner::PublishedArtifact,
) -> BTreeMap<String, CheckpointValueDto> {
    let mut checkpoint = BTreeMap::from([
        (
            "artifact_width".to_owned(),
            CheckpointValueDto::Unsigned(u64::from(artifact.probe.width)),
        ),
        (
            "artifact_height".to_owned(),
            CheckpointValueDto::Unsigned(u64::from(artifact.probe.height)),
        ),
        (
            "artifact_duration_seconds".to_owned(),
            CheckpointValueDto::Unsigned(artifact.probe.duration_seconds.floor() as u64),
        ),
        (
            "artifact_file_size_bytes".to_owned(),
            CheckpointValueDto::Unsigned(artifact.file_size_bytes),
        ),
        (
            "artifact_subtitles_downloaded".to_owned(),
            CheckpointValueDto::Unsigned(u64::from(artifact.subtitles_downloaded)),
        ),
        (
            "artifact_subtitles_missing".to_owned(),
            CheckpointValueDto::Unsigned(u64::from(artifact.subtitles_missing)),
        ),
    ]);
    insert_sanitized_checkpoint_text(
        &mut checkpoint,
        "artifact_video_codec",
        &artifact.probe.codec,
    );
    if let Some(value) = artifact.probe.video_profile.as_deref() {
        insert_sanitized_checkpoint_text(&mut checkpoint, "artifact_video_profile", value);
    }
    if let Some(value) = artifact.probe.audio_language.as_deref() {
        insert_sanitized_checkpoint_text(&mut checkpoint, "artifact_audio_language", value);
    }
    if let Some(value) = artifact.probe.audio_codec.as_deref() {
        insert_sanitized_checkpoint_text(&mut checkpoint, "artifact_audio_codec", value);
    }
    if let Some(value) = artifact.probe.audio_channel_layout.as_deref() {
        insert_sanitized_checkpoint_text(&mut checkpoint, "artifact_audio_channel_layout", value);
    }
    if let Some(value) = artifact.probe.audio_title.as_deref() {
        insert_sanitized_checkpoint_text(&mut checkpoint, "artifact_audio_title", value);
    }
    if let Some(value) = artifact.probe.audio_channels {
        checkpoint.insert(
            "artifact_audio_channels".to_owned(),
            CheckpointValueDto::Unsigned(u64::from(value)),
        );
    }
    if let Some(processing) = &artifact.processing {
        let mode = match processing.mode {
            media_runner::ProcessingMode::VaapiUpscale => "vaapi-upscale",
        };
        checkpoint.insert(
            "artifact_processing_mode".to_owned(),
            CheckpointValueDto::String(mode.to_owned()),
        );
        checkpoint.insert(
            "artifact_processing_seconds".to_owned(),
            CheckpointValueDto::Unsigned(processing.elapsed_seconds),
        );
    }
    checkpoint
}

fn insert_sanitized_checkpoint_text(
    checkpoint: &mut BTreeMap<String, CheckpointValueDto>,
    name: &str,
    value: &str,
) {
    if let Some(value) = sanitize_checkpoint_text(value) {
        checkpoint.insert(name.to_owned(), CheckpointValueDto::String(value));
    }
}

fn sanitize_checkpoint_text(value: &str) -> Option<String> {
    let sanitized = value
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = if trimmed.len() <= 64 {
        trimmed.len()
    } else {
        let mut end = 64;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        end
    };
    Some(trimmed[..end].to_owned())
}

fn insert_checkpoint_value(
    checkpoint: &mut std::collections::BTreeMap<String, CheckpointValueDto>,
    name: &str,
    value: Option<u64>,
) {
    if let Some(value) = value {
        checkpoint.insert(name.to_owned(), CheckpointValueDto::Unsigned(value));
    }
}

fn progress_percent(progress: f64) -> u8 {
    (progress.clamp(0.0, 1.0) * 100.0).round() as u8
}

fn torrent_state_name(state: &media_integrations::qbittorrent::TorrentState) -> &str {
    match state {
        media_integrations::qbittorrent::TorrentState::Downloading => "downloading",
        media_integrations::qbittorrent::TorrentState::Checking => "checking",
        media_integrations::qbittorrent::TorrentState::Queued => "queued",
        media_integrations::qbittorrent::TorrentState::Stalled => "stalled",
        media_integrations::qbittorrent::TorrentState::Complete => "complete",
        media_integrations::qbittorrent::TorrentState::Error => "error",
        media_integrations::qbittorrent::TorrentState::Unknown(_) => "unknown",
    }
}

fn highest_standard_variant(
    variants: &[rezka_client::StreamVariant],
) -> Option<&rezka_client::StreamVariant> {
    variants
        .iter()
        .find(|variant| variant.advertised_quality().tier() == rezka_client::QualityTier::Standard)
}

fn expected_source_duration_seconds(
    media_kind: media_contract::MediaKindDto,
    title_duration_minutes: Option<u16>,
) -> Option<f64> {
    match media_kind {
        media_contract::MediaKindDto::Movie => {
            title_duration_minutes.map(|minutes| f64::from(minutes) * 60.0)
        }
        // Rezka exposes a title-level duration for a series. Individual episodes can
        // legitimately be shorter, so it is not a valid truncation check for episodes.
        media_contract::MediaKindDto::Series => None,
    }
}

fn rezka_audio_language(translation: &str) -> Option<&'static str> {
    let normalized = translation.to_lowercase();
    if normalized.contains("оригинал") || normalized.contains("original") {
        None
    } else if normalized.contains("україн")
        || normalized.contains("украин")
        || normalized.contains("ukrain")
        || normalized
            .split(|character: char| !character.is_alphanumeric())
            .any(|part| part == "ukr")
    {
        Some("ukr")
    } else if normalized.contains("англий")
        || normalized.contains("english")
        || normalized
            .split(|character: char| !character.is_alphanumeric())
            .any(|part| part == "eng")
    {
        Some("eng")
    } else {
        Some("rus")
    }
}

async fn submit_torrent_with_retry(
    client: &media_integrations::qbittorrent::QbittorrentClient,
    selection: media_integrations::qbittorrent::ExplicitTorrentSelection,
    category: &str,
    episode: Option<media_integrations::qbittorrent::EpisodeFileSelection>,
) -> Result<media_integrations::qbittorrent::TorrentHandle, RunnerError> {
    const ATTEMPTS: usize = 12;
    for attempt in 1..=ATTEMPTS {
        let submission = match episode {
            Some(episode) => {
                client
                    .submit_episode_to_category(selection.clone(), category, episode)
                    .await
            }
            None => {
                client
                    .submit_selected_to_category(selection.clone(), category)
                    .await
            }
        };
        match submission {
            Ok(handle) => return Ok(handle),
            Err(error) if error.is_transient() && attempt < ATTEMPTS => {
                tracing::warn!(attempt, "qBittorrent submit transport failed; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(error) => {
                tracing::warn!(error_code = ?error.code(), "qBittorrent submit failed");
                return Err(RunnerError::Execution);
            }
        }
    }
    Err(RunnerError::Execution)
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
            STAGING_RETENTION_WINDOW,
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
        // Sweep staging left behind by non-terminal outcomes and dead runners,
        // which never get a terminal stamp. The current job is protected so its
        // in-progress staging is never touched.
        match media_runner::cleanup_orphan_staging(
            self.roots.staging(),
            std::time::SystemTime::now(),
            STAGING_RETENTION_WINDOW,
            &[current_job_id.as_str()],
        )
        .await
        {
            Ok(removed) if !removed.is_empty() => {
                tracing::info!(
                    directories_removed = removed.len(),
                    "orphan staging removed"
                );
            }
            Ok(_) => {}
            Err(_) => tracing::warn!("orphan staging retention pass failed"),
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

    async fn cleanup_cancelled(&self, lease: &LeaseDto) -> Result<(), RunnerError> {
        let Some(media_contract::ExecutionSelectionDto::Prowlarr {
            source_identity,
            info_hash,
            media_kind,
            episode,
            ..
        }) = lease.execution.as_ref()
        else {
            return Ok(());
        };
        let client = self
            .qbittorrent
            .as_ref()
            .ok_or(RunnerError::Configuration)?;
        let category = match media_kind {
            media_contract::MediaKindDto::Movie => &self.torrent_movies_category,
            media_contract::MediaKindDto::Series => &self.torrent_tv_category,
        };
        let handle = media_integrations::qbittorrent::TorrentHandle {
            source_identity: source_identity.clone(),
            hash: info_hash.to_ascii_lowercase(),
            category: category.clone(),
        };
        let result = if episode.is_some() {
            client.remove_managed_episode(&handle).await.map(|_| ())
        } else {
            client.stop_selected(&handle).await
        };
        match result {
            Ok(()) | Err(media_integrations::qbittorrent::QbittorrentError::TorrentNotFound) => {
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    error_code = ?error.code(),
                    "failed to clean up cancelled torrent"
                );
                Err(RunnerError::Execution)
            }
        }
    }

    async fn maintain(&self, protected_job_ids: &[String]) -> Result<(), RunnerError> {
        let protected = protected_job_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let now = std::time::SystemTime::now();
        let removed = media_runner::cleanup_terminal_staging(
            self.roots.staging(),
            now,
            STAGING_RETENTION_WINDOW,
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
        // Also sweep unstamped orphans (non-terminal outcomes, dead runners) that
        // the terminal fast path above never removes.
        let orphaned = media_runner::cleanup_orphan_staging(
            self.roots.staging(),
            now,
            STAGING_RETENTION_WINDOW,
            &protected,
        )
        .await
        .map_err(|_| RunnerError::Execution)?;
        if !orphaned.is_empty() {
            tracing::info!(
                directories_removed = orphaned.len(),
                "orphan staging removed"
            );
        }
        Ok(())
    }
}

fn safe_name(value: &str) -> String {
    let (value, plex_identity) = value
        .strip_suffix('}')
        .and_then(|value| value.rsplit_once(" {tmdb-"))
        .filter(|(_, id)| {
            !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()) && id != &"0"
        })
        .map_or((value, None), |(title, id)| (title, Some(id)));
    let value = value
        .nfc()
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
    let value = if value.is_empty() {
        "media".to_owned()
    } else {
        value
    };
    if let Some(id) = plex_identity {
        format!("{value} {{tmdb-{id}}}")
    } else {
        value
    }
}

fn tmdb_id_from_path_title(value: &str) -> Option<u64> {
    value
        .strip_suffix('}')?
        .rsplit_once(" {tmdb-")?
        .1
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
}

fn tmdb_display_title_from_path(value: &str, tmdb_id: u64) -> Option<&str> {
    let braced = format!(" {{tmdb-{tmdb_id}}}");
    let legacy = format!(" tmdb-{tmdb_id}");
    value
        .strip_suffix(&braced)
        .or_else(|| value.strip_suffix(&legacy))
        .map(str::trim)
        .filter(|title| !title.is_empty())
}

async fn resolve_existing_series_path_title(
    shows_root: &std::path::Path,
    current: &str,
    legacy: &[String],
) -> Result<String, RunnerError> {
    let mut candidates = std::iter::once(current)
        .chain(legacy.iter().map(String::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(tmdb_id) = tmdb_id_from_path_title(current) {
        let mut entries = match tokio::fs::read_dir(shows_root).await {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(RunnerError::Execution),
        };
        if let Some(entries) = entries.as_mut() {
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|_| RunnerError::Execution)?
            {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if name.ends_with(&format!(" {{tmdb-{tmdb_id}}}"))
                    || name.ends_with(&format!(" tmdb-{tmdb_id}"))
                {
                    candidates.push(name);
                }
            }
        }
    }

    let mut existing = Vec::new();
    for candidate in &candidates {
        if safe_name(candidate) != *candidate || existing.iter().any(|value| value == candidate) {
            continue;
        }
        match tokio::fs::symlink_metadata(shows_root.join(candidate)).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                existing.push(candidate.to_owned())
            }
            Ok(_) => return Err(RunnerError::Execution),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RunnerError::Execution),
        }
    }
    match existing.as_slice() {
        [] => Ok(current.to_owned()),
        [selected] => Ok(selected.clone()),
        _ => Err(RunnerError::Execution),
    }
}

fn plex_match_matches(
    contents: &[u8],
    expected_title: &str,
    expected_tmdb_id: Option<u64>,
) -> bool {
    let Ok(contents) = std::str::from_utf8(contents) else {
        return false;
    };
    let mut fields = BTreeMap::<String, String>::new();
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.starts_with('#') {
            continue;
        }
        let Some((raw_key, value)) = line.split_once(':') else {
            return false;
        };
        let key = match raw_key.trim().to_ascii_lowercase().as_str() {
            "show" => "title".to_owned(),
            key => key.to_owned(),
        };
        let value = value.trim();
        if !matches!(
            key.as_str(),
            "title" | "year" | "guid" | "tmdbid" | "tvdbid" | "imdbid"
        ) || value.is_empty()
            || fields.get(&key).is_some_and(|existing| existing != value)
        {
            return false;
        }
        fields.insert(key, value.to_owned());
    }
    let parsed_tmdb_id = fields
        .get("tmdbid")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|id| *id > 0);
    if fields.contains_key("tmdbid") && parsed_tmdb_id.is_none() {
        return false;
    }
    if fields
        .get("year")
        .is_some_and(|value| value.len() != 4 || value.parse::<u16>().is_err())
        || fields
            .get("tvdbid")
            .is_some_and(|value| value.parse::<u64>().ok().is_none_or(|id| id == 0))
        || fields.get("imdbid").is_some_and(|value| {
            value.strip_prefix("tt").is_none_or(|digits| {
                digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
    {
        return false;
    }
    let guid_tmdb_id = if let Some(guid) = fields.get("guid") {
        let Some((namespace, value)) = guid.split_once("://") else {
            return false;
        };
        match namespace.to_ascii_lowercase().as_str() {
            "tmdb" => {
                let Some(id) = value.parse::<u64>().ok().filter(|id| *id > 0) else {
                    return false;
                };
                Some(id)
            }
            "tvdb" => {
                if value.parse::<u64>().ok().filter(|id| *id > 0).is_none()
                    || fields.get("tvdbid").is_some_and(|id| id != value)
                {
                    return false;
                }
                None
            }
            "imdb" => {
                if !value.starts_with("tt") || fields.get("imdbid").is_some_and(|id| id != value) {
                    return false;
                }
                None
            }
            _ => return false,
        }
    } else {
        None
    };
    if expected_tmdb_id.is_some()
        && parsed_tmdb_id
            .or(guid_tmdb_id)
            .is_none_or(|id| Some(id) != expected_tmdb_id)
    {
        return false;
    }
    if parsed_tmdb_id.is_some() && guid_tmdb_id.is_some() && parsed_tmdb_id != guid_tmdb_id {
        return false;
    }
    if expected_tmdb_id.is_some() {
        true
    } else {
        fields.get("title").is_some_and(|title| {
            title.nfc().collect::<String>() == expected_title.nfc().collect::<String>()
        })
    }
}

async fn ensure_plex_match(
    final_video: &std::path::Path,
    display_title: &str,
    tmdb_id: Option<u64>,
) -> Result<(), RunnerError> {
    let show_directory = final_video
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or(RunnerError::Execution)?;
    tokio::fs::create_dir_all(show_directory)
        .await
        .map_err(|_| RunnerError::Execution)?;
    let title = display_title
        .trim()
        .nfc()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        return Err(RunnerError::Execution);
    }
    let mut contents = format!("# PlexMatch\nTitle: {title}\n");
    if let Some(tmdb_id) = tmdb_id {
        contents.push_str(&format!("tmdbid: {tmdb_id}\n"));
    }
    let path = show_directory.join(".plexmatch");
    match tokio::fs::read(&path).await {
        Ok(existing) => {
            return if plex_match_matches(&existing, &title, tmdb_id) {
                Ok(())
            } else {
                Err(RunnerError::Execution)
            };
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(RunnerError::Execution),
    }
    let temporary = show_directory.join(format!(".plexmatch.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .await
    {
        Ok(file) => file,
        Err(_) => return Err(RunnerError::Execution),
    };
    if file.write_all(contents.as_bytes()).await.is_err() || file.sync_all().await.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(RunnerError::Execution);
    }
    drop(file);
    let published = match tokio::fs::hard_link(&temporary, &path).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(_) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(RunnerError::Execution);
        }
    };
    tokio::fs::remove_file(&temporary)
        .await
        .map_err(|_| RunnerError::Execution)?;
    if published {
        Ok(())
    } else {
        let existing = tokio::fs::read(path)
            .await
            .map_err(|_| RunnerError::Execution)?;
        if plex_match_matches(&existing, &title, tmdb_id) {
            Ok(())
        } else {
            Err(RunnerError::Execution)
        }
    }
}

fn rezka_physical_title<'a>(
    path_title: Option<&'a str>,
    mapped_title: Option<&'a str>,
    library_title: Option<&'a str>,
    provider_title: &'a str,
) -> &'a str {
    path_title
        .or(mapped_title)
        .or(library_title)
        .unwrap_or(provider_title)
}

fn canonical_movie_name(safe_title: &str, release_year: Option<u16>) -> String {
    release_year.map_or_else(
        || safe_title.to_owned(),
        |year| format!("{safe_title} ({year})"),
    )
}

fn rezka_final_video_path(
    roots: &media_runner::StorageRoots,
    safe_title: &str,
    release_year: Option<u16>,
    season: Option<u32>,
    episode: Option<u32>,
) -> std::path::PathBuf {
    season.zip(episode).map_or_else(
        || {
            let movie_name = canonical_movie_name(safe_title, release_year);
            roots.movies().join(format!("{movie_name}.mkv"))
        },
        |(season, episode)| {
            let season_directory = if season == 0 {
                "Specials".to_owned()
            } else {
                format!("Season {season:02}")
            };
            roots
                .tv()
                .join(safe_title)
                .join(season_directory)
                .join(format!("{safe_title} - S{season:02}E{episode:02}.mkv"))
        },
    )
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
        if episode > 0 {
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

fn torrent_series_work_items(
    mut videos: Vec<std::path::PathBuf>,
    expected_season: u32,
    expected_episode: Option<u32>,
) -> Vec<(std::path::PathBuf, (u32, u32))> {
    if let Some(expected_episode) = expected_episode {
        return (videos.len() == 1)
            .then(|| {
                (
                    videos.pop().expect("one video was checked"),
                    (expected_season, expected_episode),
                )
            })
            .into_iter()
            .collect();
    }
    matching_episode_videos(videos, expected_season)
}

fn map_pipeline_outcome(outcome: media_runner::EpisodeOutcome) -> ExecutionOutcome {
    match outcome {
        media_runner::EpisodeOutcome::Completed => ExecutionOutcome::Completed,
        media_runner::EpisodeOutcome::Partial { .. } => ExecutionOutcome::Partial,
        media_runner::EpisodeOutcome::BlockedStorage(blocked) => ExecutionOutcome::BlockedStorage {
            available_bytes: blocked.available_bytes(),
            required_bytes: blocked.required_bytes(),
        },
        media_runner::EpisodeOutcome::PlexPending => ExecutionOutcome::PlexPending,
        media_runner::EpisodeOutcome::NeedsActionPlexMismatch => {
            ExecutionOutcome::NeedsActionPlexMismatch
        }
        media_runner::EpisodeOutcome::Cancelled => ExecutionOutcome::Cancelled,
    }
}

fn map_pipeline_error(error: media_runner::RunnerPortError) -> RunnerError {
    match error {
        media_runner::RunnerPortError::SourceExpired => RunnerError::SourceExpired,
        media_runner::RunnerPortError::SourceTransferTransient => {
            RunnerError::SourceTransferTransient
        }
        media_runner::RunnerPortError::SourceTransferRejected => {
            RunnerError::SourceTransferRejected
        }
        _ => RunnerError::Execution,
    }
}

fn combine_episode_outcome(
    aggregate: ExecutionOutcome,
    current: ExecutionOutcome,
) -> ExecutionOutcome {
    match (aggregate, current) {
        (_, ExecutionOutcome::Cancelled) => ExecutionOutcome::Cancelled,
        (_, ExecutionOutcome::NeedsActionPlexMismatch) => ExecutionOutcome::NeedsActionPlexMismatch,
        (_, ExecutionOutcome::NeedsActionIdentityAmbiguous) => {
            ExecutionOutcome::NeedsActionIdentityAmbiguous
        }
        (_, blocked @ ExecutionOutcome::BlockedStorage { .. }) => blocked,
        (_, ExecutionOutcome::PlexPending) => ExecutionOutcome::PlexPending,
        (_, ExecutionOutcome::Failed) => ExecutionOutcome::Failed,
        (ExecutionOutcome::Partial, ExecutionOutcome::Completed)
        | (ExecutionOutcome::Completed, ExecutionOutcome::Partial)
        | (ExecutionOutcome::Partial, ExecutionOutcome::Partial) => ExecutionOutcome::Partial,
        (aggregate, ExecutionOutcome::Partial | ExecutionOutcome::Completed) => aggregate,
    }
}

/// Lease TTL assumed when a lease's `expires_at` cannot be parsed. It matches the
/// service's minimum configurable TTL so cancellation still fires early rather
/// than late when the timestamp is unexpectedly malformed.
const FALLBACK_LEASE_TTL: Duration = Duration::from_secs(30);

/// Remaining lifetime of a lease, derived from its `expires_at`. `None` when the
/// timestamp cannot be parsed or is already in the past, in which case the caller
/// uses a conservative fallback (and an already-expired lease cancels at once).
fn lease_remaining_ttl(lease: &LeaseDto) -> Option<Duration> {
    let expires = time::OffsetDateTime::parse(
        &lease.expires_at,
        &time::format_description::well_known::Rfc3339,
    )
    .ok()?;
    Duration::try_from(expires - time::OffsetDateTime::now_utc()).ok()
}

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
    let lease_ttl = lease_remaining_ttl(&lease).unwrap_or(FALLBACK_LEASE_TTL);
    let heartbeat_attempt_timeout = (heartbeat_interval / 2).max(Duration::from_millis(1));
    let heartbeat = tokio::spawn(async move {
        // Cancel cooperatively once too much wall-time has elapsed since the last
        // successful heartbeat relative to the lease TTL, keeping a safety margin
        // before the lease actually expires. Driving cancellation from elapsed
        // time (not a fixed failure count) guarantees we stop before the service
        // reaper could re-lease this job to another runner — even when a hung
        // service makes each attempt slow rather than failing outright.
        let cancel_after = lease_ttl.saturating_sub(lease_ttl / 4);
        // Bound every attempt well below the heartbeat cadence so a single hung
        // request cannot consume the whole budget and push cancellation late.
        let mut last_success = tokio::time::Instant::now();
        let mut consecutive_failures: u32 = 0;
        loop {
            if last_success.elapsed() >= cancel_after {
                heartbeat_cancelled.store(true, Ordering::SeqCst);
                break;
            }
            match tokio::time::timeout(
                heartbeat_attempt_timeout,
                heartbeat_api.heartbeat(&heartbeat_lease),
            )
            .await
            {
                Ok(Ok(current)) => {
                    consecutive_failures = 0;
                    last_success = tokio::time::Instant::now();
                    if matches!(
                        current.job.state,
                        JobStateDto::CancelRequested | JobStateDto::Cancelled
                    ) {
                        heartbeat_cancelled.store(true, Ordering::SeqCst);
                    }
                }
                // A failed or timed-out attempt keeps the lease unrenewed; retry
                // with backoff until the TTL-based deadline above forces a cancel.
                Ok(Err(_)) | Err(_) => {
                    consecutive_failures += 1;
                }
            }
            let base_wait = if consecutive_failures == 0 {
                heartbeat_interval
            } else {
                heartbeat_retry_backoff(consecutive_failures, heartbeat_interval)
            };
            // Never sleep past the point where we must cancel, so a long backoff
            // cannot delay cancellation beyond the lease deadline.
            let remaining = cancel_after.saturating_sub(last_success.elapsed());
            let wait = base_wait.min(remaining);
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
    control
        .stage_started(EXECUTION_TASK_ORDINAL, "execution", EXECUTION_STAGE_ORDINAL)
        .await?;
    let mut outcome = executor.execute(&lease, &control).await;
    if !control.is_cancelled()
        && let Ok(Ok(current)) =
            tokio::time::timeout(heartbeat_attempt_timeout, api.heartbeat(&lease)).await
        && matches!(
            current.job.state,
            JobStateDto::CancelRequested | JobStateDto::Cancelled
        )
    {
        control.cancelled.store(true, Ordering::SeqCst);
    }
    if control.is_cancelled() {
        if executor.cleanup_cancelled(&lease).await.is_err() {
            tracing::warn!("failed to clean up a cancelled job");
        }
        outcome = Ok(ExecutionOutcome::Cancelled);
    }
    // Wake the heartbeat task immediately instead of waiting out its sleep.
    finished.notify_one();
    let _ = heartbeat.await;
    let outcome = match outcome {
        Ok(outcome) => {
            if !matches!(outcome, ExecutionOutcome::Cancelled) {
                control
                    .stage_completed(EXECUTION_TASK_ORDINAL, "execution", EXECUTION_STAGE_ORDINAL)
                    .await?;
            }
            outcome
        }
        Err(error) => {
            let (retryable, error_code) = error.stage_failure();
            let (task_ordinal, stage_name, stage_ordinal) = error.failure_stage();
            let job = control
                .stage_failed(
                    task_ordinal,
                    stage_name,
                    stage_ordinal,
                    retryable,
                    error_code,
                )
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
        ExecutionOutcome::BlockedStorage { .. } => vec![(JobStateDto::BlockedStorage, None)],
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
        ExecutionOutcome::NeedsActionIdentityAmbiguous => vec![(
            JobStateDto::NeedsAction,
            Some(NeedsActionReasonDto::IdentityAmbiguous),
        )],
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
    exit_after_job: bool,
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
                // The service has durably gated the next lease until the watcher
                // replaces the VPN session. Exit cleanly so the watcher can act.
                Err(RunnerError::RotationRequired) => return Ok(()),
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
        if worked && exit_after_job {
            return Ok(());
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
        let response = self
            .request(reqwest::Method::POST, "v1/runner/leases")?
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|_| RunnerError::Service)?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            let rotation_required = response
                .json::<ApiError>()
                .await
                .is_ok_and(|error| error.code == ApiErrorCode::VpnRotationRequired);
            return Err(if rotation_required {
                RunnerError::RotationRequired
            } else {
                RunnerError::Service
            });
        }
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(RunnerError::Service);
        }
        response
            .json::<LeaseDto>()
            .await
            .map(Some)
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
        ExecutionOutcome, ProgressCheckpointGate, artifact_checkpoint, canonical_movie_name,
        combine_episode_outcome, ensure_plex_match, expected_source_duration_seconds,
        highest_standard_variant, matching_episode_videos, parse_episode_coordinates,
        plex_match_matches, resolve_existing_series_path_title, rezka_audio_language,
        rezka_final_video_path, rezka_physical_title, safe_name, tmdb_display_title_from_path,
        torrent_series_work_items,
    };

    #[test]
    fn safe_names_normalize_unicode_before_building_library_paths() {
        assert_eq!(safe_name("Cafe\u{301}"), safe_name("Café"));
        assert_eq!(safe_name("и\u{306}ога"), safe_name("йога"));
        assert_eq!(
            safe_name("Магия и мускулы {tmdb-94997}"),
            "Магия и мускулы {tmdb-94997}"
        );
        assert_eq!(safe_name("Fake {tmdb-not-id}"), "Fake tmdb-not-id");
    }

    #[test]
    fn rezka_path_identity_wins_over_display_and_episode_mapping_titles() {
        assert_eq!(
            rezka_physical_title(
                Some("rezka-90825"),
                Some("Mapped Alias"),
                Some("Localized Alias"),
                "Provider Alias",
            ),
            "rezka-90825",
        );
    }

    #[tokio::test]
    async fn plex_match_handoff_is_human_readable_and_first_writer_stable() {
        let root = tempfile::tempdir().unwrap();
        let video = root
            .path()
            .join("rezka-90825/Season 01/rezka-90825 - S01E01.mkv");

        ensure_plex_match(&video, "Cafe\u{301}", None)
            .await
            .unwrap();
        ensure_plex_match(&video, "Cafe\u{301}", None)
            .await
            .unwrap();
        assert_eq!(
            ensure_plex_match(&video, "Different Alias", None).await,
            Err(super::RunnerError::Execution),
        );

        assert_eq!(
            tokio::fs::read_to_string(root.path().join("rezka-90825/.plexmatch"))
                .await
                .unwrap(),
            "# PlexMatch\nTitle: Café\n",
        );
    }

    #[test]
    fn plex_match_parser_accepts_plex_comments_case_and_whitespace_but_rejects_conflicts() {
        assert!(plex_match_matches(
            b"# user comment\n TITLE : Cafe\xcc\x81 \n TmDbId: 94997\n# PlexMatch\n",
            "Café",
            Some(94997),
        ));
        assert!(!plex_match_matches(
            b"Title: Cafe\ntmdbid: 94997\ntmdbid: 42\n",
            "Cafe",
            Some(94997),
        ));
        assert!(plex_match_matches(
            b"Title: Cafe\ntvdbid: 100\n",
            "Cafe",
            None,
        ));
        assert!(!plex_match_matches(b"Title: Cafe\n", "Cafe", Some(94997),));
    }

    #[test]
    fn plex_match_tmdb_identity_survives_localized_title_drift_and_neutral_hints() {
        assert!(plex_match_matches(
            b"# PlexMatch\nTitle: Old Localized Title\nYear: 2023\nGuid: tmdb://94997\nTVDBID: 777\nIMDbID: tt1234567\nTmDbId: 94997\n",
            "New Localized Title",
            Some(94997),
        ));
        assert!(!plex_match_matches(
            b"Title: Old Localized Title\nGuid: tmdb://42\ntmdbid: 94997\n",
            "New Localized Title",
            Some(94997),
        ));
        assert!(!plex_match_matches(
            b"Title: Old Localized Title\ntmdbid: 94997\ntmdbid: 42\n",
            "New Localized Title",
            Some(94997),
        ));
        assert!(!plex_match_matches(
            b"Title: Old Localized Title\ntmdbid: 94997\ntvdbid: 777\nguid: tvdb://888\n",
            "New Localized Title",
            Some(94997),
        ));
        assert!(!plex_match_matches(
            b"Title: Old Localized Title\ntmdbid: 94997\nguid: unsupported\n",
            "New Localized Title",
            Some(94997),
        ));
    }

    #[test]
    fn plex_match_accepts_show_alias_and_authoritative_tmdb_guid_only() {
        assert!(plex_match_matches(
            b"Show: Old Localized Title\nGuid: tmdb://94997\n",
            "New Localized Title",
            Some(94997),
        ));
        assert!(plex_match_matches(
            b"Guid: tmdb://94997\n",
            "New Localized Title",
            Some(94997),
        ));
        assert!(!plex_match_matches(
            b"Title: One\nShow: Two\nGuid: tmdb://94997\n",
            "Two",
            Some(94997),
        ));
        assert!(!plex_match_matches(
            b"Guid: tmdb://94997\ntmdbid: 94998\n",
            "New Localized Title",
            Some(94997),
        ));
    }

    #[tokio::test]
    async fn existing_legacy_series_directory_wins_before_new_publication() {
        let root = tempfile::tempdir().unwrap();
        let shows = root.path().join("shows");
        tokio::fs::create_dir_all(shows.join("rezka-series-tmdb-94997"))
            .await
            .unwrap();

        let selected = resolve_existing_series_path_title(
            &shows,
            "Магия и мускулы {tmdb-94997}",
            &["rezka-series-tmdb-94997".to_owned()],
        )
        .await
        .unwrap();

        assert_eq!(selected, "rezka-series-tmdb-94997");
    }

    #[tokio::test]
    async fn existing_unbound_and_historical_tmdb_roots_are_reused_without_cross_identity_merge() {
        let root = tempfile::tempdir().unwrap();
        let shows = root.path().join("shows");
        tokio::fs::create_dir_all(shows.join("rezka-90825"))
            .await
            .unwrap();
        let selected = resolve_existing_series_path_title(
            &shows,
            "New Localized Title {tmdb-94997}",
            &["rezka-90825".to_owned()],
        )
        .await
        .unwrap();
        assert_eq!(selected, "rezka-90825");

        tokio::fs::remove_dir(shows.join("rezka-90825"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(shows.join("Old Localized Title tmdb-94997"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(shows.join("Unrelated Show tmdb-94998"))
            .await
            .unwrap();
        let selected =
            resolve_existing_series_path_title(&shows, "New Localized Title {tmdb-94997}", &[])
                .await
                .unwrap();
        assert_eq!(selected, "Old Localized Title tmdb-94997");
        assert_eq!(
            tmdb_display_title_from_path(&selected, 94997),
            Some("Old Localized Title"),
        );

        tokio::fs::remove_dir(shows.join("Old Localized Title tmdb-94997"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(shows.join("О моём перерождении в слизь ТВ-4"))
            .await
            .unwrap();
        let selected = resolve_existing_series_path_title(
            &shows,
            "О моём перерождении в слизь {tmdb-82684}",
            &["О моём перерождении в слизь ТВ-4".to_owned()],
        )
        .await
        .unwrap();
        assert_eq!(selected, "О моём перерождении в слизь ТВ-4");
    }

    #[test]
    fn non_premium_accounts_choose_the_highest_standard_quality() {
        let variants = rezka_client::parse_stream_variants(
            "[<span class='premium'>1080p Ultra</span>]https://cdn.example/u.m3u8,[1080p]https://cdn.example/1080.m3u8,[720p]https://cdn.example/720.m3u8",
        )
        .expect("quality listing is valid");

        let selected = highest_standard_variant(&variants).expect("standard quality exists");

        assert_eq!(selected.advertised_quality().label(), "1080p");
        assert_eq!(
            selected.advertised_quality().tier(),
            rezka_client::QualityTier::Standard
        );
    }

    #[test]
    fn non_premium_accounts_fail_closed_when_only_premium_quality_exists() {
        let variants = rezka_client::parse_stream_variants(
            "[<span class='premium'>1080p Ultra</span>]https://cdn.example/u.m3u8",
        )
        .expect("quality listing is valid");

        assert!(highest_standard_variant(&variants).is_none());
    }

    #[test]
    fn rezka_translation_names_produce_conservative_audio_language_tags() {
        assert_eq!(rezka_audio_language("DEEP"), Some("rus"));
        assert_eq!(rezka_audio_language("Український дубляж"), Some("ukr"));
        assert_eq!(rezka_audio_language("English"), Some("eng"));
        assert_eq!(rezka_audio_language("Оригинал (+субтитры)"), None);
    }

    #[test]
    fn source_duration_is_only_strict_for_movies() {
        assert_eq!(
            expected_source_duration_seconds(media_contract::MediaKindDto::Movie, Some(90)),
            Some(5_400.0)
        );
        assert_eq!(
            expected_source_duration_seconds(media_contract::MediaKindDto::Series, Some(24)),
            None
        );
    }

    #[test]
    fn movie_names_include_the_release_year_when_known() {
        assert_eq!(canonical_movie_name("WALL E", Some(2008)), "WALL E (2008)");
        assert_eq!(canonical_movie_name("WALL E", None), "WALL E");
    }

    #[test]
    fn mapped_special_uses_plex_specials_path_and_zero_season_coordinate() {
        let roots = media_runner::StorageRoots::new("/staging", "/plex/tv", "/plex/movies")
            .expect("test roots are disjoint");

        assert_eq!(
            rezka_final_video_path(&roots, "Attack on Titan", None, Some(0), Some(1)),
            std::path::PathBuf::from(
                "/plex/tv/Attack on Titan/Specials/Attack on Titan - S00E01.mkv"
            )
        );
    }

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
            combine_episode_outcome(
                ExecutionOutcome::Partial,
                ExecutionOutcome::BlockedStorage {
                    available_bytes: 1,
                    required_bytes: 2,
                },
            ),
            ExecutionOutcome::BlockedStorage {
                available_bytes: 1,
                required_bytes: 2,
            },
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

    #[test]
    fn selectively_downloaded_episode_uses_the_requested_coordinates() {
        let matched =
            torrent_series_work_items(vec!["Example Show/Season 2/07.mkv".into()], 2, Some(7));

        assert_eq!(
            matched,
            vec![(
                std::path::PathBuf::from("Example Show/Season 2/07.mkv"),
                (2, 7)
            )]
        );
    }

    #[test]
    fn progress_gate_reports_first_interval_state_change_and_final_observations() {
        let started = tokio::time::Instant::now();
        let mut gate = ProgressCheckpointGate::default();

        assert!(gate.should_report(started, "downloading", false));
        assert!(!gate.should_report(
            started + std::time::Duration::from_secs(4),
            "downloading",
            false
        ));
        assert!(gate.should_report(
            started + std::time::Duration::from_secs(4),
            "stalled",
            false
        ));
        assert!(gate.should_report(
            started + std::time::Duration::from_secs(9),
            "stalled",
            false
        ));
        assert!(gate.should_report(
            started + std::time::Duration::from_secs(10),
            "complete",
            true
        ));
    }

    #[test]
    fn published_artifact_checkpoint_contains_only_bounded_media_facts() {
        let artifact = media_runner::PublishedArtifact {
            probe: media_runner::MediaProbe {
                codec: "hevc".to_owned(),
                width: 1_920,
                height: 1_080,
                duration_seconds: 1_421.9,
                bitrate: Some(2_000_000),
                video_profile: Some("Main".to_owned()),
                audio_language: Some("rus".to_owned()),
                audio_title: Some(format!("Ani\u{0000}Libria {}", "Я".repeat(64))),
                audio_codec: Some("aac".to_owned()),
                audio_channels: Some(2),
                audio_channel_layout: Some("stereo".to_owned()),
                timeline: media_runner::MediaTimeline {
                    video_packet_count: 34_126,
                    audio_packet_count: Some(66_655),
                    max_video_gap_seconds: 0.04,
                    max_audio_gap_seconds: Some(0.02),
                    video_end_seconds: 1_421.88,
                    audio_end_seconds: Some(1_421.89),
                },
            },
            file_size_bytes: 440_401_920,
            subtitles_downloaded: 2,
            subtitles_missing: 0,
            processing: Some(media_runner::MediaProcessing {
                mode: media_runner::ProcessingMode::VaapiUpscale,
                elapsed_seconds: 252,
            }),
        };

        let checkpoint = artifact_checkpoint(&artifact);

        assert_eq!(
            checkpoint["artifact_video_codec"],
            media_contract::CheckpointValueDto::String("hevc".to_owned())
        );
        assert_eq!(
            checkpoint["artifact_video_profile"],
            media_contract::CheckpointValueDto::String("Main".to_owned())
        );
        assert_eq!(
            checkpoint["artifact_width"],
            media_contract::CheckpointValueDto::Unsigned(1_920)
        );
        assert_eq!(
            checkpoint["artifact_height"],
            media_contract::CheckpointValueDto::Unsigned(1_080)
        );
        assert_eq!(
            checkpoint["artifact_duration_seconds"],
            media_contract::CheckpointValueDto::Unsigned(1_421)
        );
        assert_eq!(
            checkpoint["artifact_file_size_bytes"],
            media_contract::CheckpointValueDto::Unsigned(440_401_920)
        );
        assert_eq!(
            checkpoint["artifact_audio_language"],
            media_contract::CheckpointValueDto::String("rus".to_owned())
        );
        assert_eq!(
            checkpoint["artifact_audio_codec"],
            media_contract::CheckpointValueDto::String("aac".to_owned())
        );
        assert_eq!(
            checkpoint["artifact_audio_channels"],
            media_contract::CheckpointValueDto::Unsigned(2)
        );
        assert_eq!(
            checkpoint["artifact_audio_channel_layout"],
            media_contract::CheckpointValueDto::String("stereo".to_owned())
        );
        assert_eq!(
            checkpoint["artifact_subtitles_downloaded"],
            media_contract::CheckpointValueDto::Unsigned(2)
        );
        assert_eq!(
            checkpoint["artifact_subtitles_missing"],
            media_contract::CheckpointValueDto::Unsigned(0)
        );
        assert_eq!(
            checkpoint["artifact_processing_mode"],
            media_contract::CheckpointValueDto::String("vaapi-upscale".to_owned())
        );
        assert_eq!(
            checkpoint["artifact_processing_seconds"],
            media_contract::CheckpointValueDto::Unsigned(252)
        );
        let media_contract::CheckpointValueDto::String(audio_title) =
            &checkpoint["artifact_audio_title"]
        else {
            panic!("audio title must be a string");
        };
        assert!(!audio_title.chars().any(char::is_control));
        assert!(audio_title.len() <= 64);
        assert!(checkpoint.keys().all(|key| {
            !["path", "url", "cookie", "token"]
                .iter()
                .any(|forbidden| key.contains(forbidden))
        }));
    }
}
