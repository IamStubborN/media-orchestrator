use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use media::runner::{
    ExecutionOutcome, JobExecutor, RunnerApi, RunnerControl, run_loop, run_single_iteration,
};
use media_contract::{
    CheckpointValueDto, ExecutionSelectionDto, JobDto, JobStateDto, LeaseDto, NotifyScopeDto,
    ProviderDto, RunnerEventDto,
};

struct FakeApi {
    lease: Mutex<Option<LeaseDto>>,
    events: Mutex<Vec<RunnerEventDto>>,
    heartbeats: AtomicUsize,
}

#[async_trait::async_trait]
impl RunnerApi for FakeApi {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, media::runner::RunnerError> {
        Ok(self.lease.lock().unwrap().take())
    }
    async fn heartbeat(&self, lease: &LeaseDto) -> Result<LeaseDto, media::runner::RunnerError> {
        self.heartbeats.fetch_add(1, Ordering::SeqCst);
        Ok(lease.clone())
    }
    async fn report(
        &self,
        lease: &LeaseDto,
        event: RunnerEventDto,
    ) -> Result<media_contract::JobDto, media::runner::RunnerError> {
        let mut job = lease.job.clone();
        match &event {
            RunnerEventDto::StageFailed { retryable, .. } if *retryable => {
                job.state = JobStateDto::Queued;
            }
            RunnerEventDto::StageFailed { .. } => job.state = JobStateDto::Failed,
            RunnerEventDto::JobTransition { state, .. } => job.state = *state,
            _ => {}
        }
        self.events.lock().unwrap().push(event);
        Ok(job)
    }
}

struct RecordingExecutor {
    active: AtomicUsize,
    max_active: AtomicUsize,
}

struct FailingExecutor;

struct RotationRequiredApi;

#[async_trait::async_trait]
impl RunnerApi for RotationRequiredApi {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, media::runner::RunnerError> {
        Err(media::runner::RunnerError::RotationRequired)
    }

    async fn heartbeat(&self, _: &LeaseDto) -> Result<LeaseDto, media::runner::RunnerError> {
        unreachable!("a rotation decision cannot own a lease")
    }

    async fn report(
        &self,
        _: &LeaseDto,
        _: RunnerEventDto,
    ) -> Result<JobDto, media::runner::RunnerError> {
        unreachable!("a rotation decision cannot report job events")
    }
}

struct TypedFailingExecutor(media::runner::RunnerError);

struct ExpireOnceExecutor {
    attempts: AtomicUsize,
}

#[async_trait::async_trait]
impl JobExecutor for FailingExecutor {
    async fn execute(
        &self,
        _: &LeaseDto,
        _: &RunnerControl,
    ) -> Result<ExecutionOutcome, media::runner::RunnerError> {
        Err(media::runner::RunnerError::Execution)
    }
}

#[async_trait::async_trait]
impl JobExecutor for TypedFailingExecutor {
    async fn execute(
        &self,
        _: &LeaseDto,
        _: &RunnerControl,
    ) -> Result<ExecutionOutcome, media::runner::RunnerError> {
        Err(self.0)
    }
}

#[async_trait::async_trait]
impl JobExecutor for ExpireOnceExecutor {
    async fn execute(
        &self,
        _: &LeaseDto,
        _: &RunnerControl,
    ) -> Result<ExecutionOutcome, media::runner::RunnerError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(media::runner::RunnerError::SourceExpired)
        } else {
            Ok(ExecutionOutcome::Completed)
        }
    }
}

struct ReleasingApi {
    leases: Mutex<VecDeque<LeaseDto>>,
    events: Mutex<Vec<RunnerEventDto>>,
}

#[async_trait::async_trait]
impl RunnerApi for ReleasingApi {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, media::runner::RunnerError> {
        Ok(self.leases.lock().unwrap().pop_front())
    }

    async fn heartbeat(&self, lease: &LeaseDto) -> Result<LeaseDto, media::runner::RunnerError> {
        Ok(lease.clone())
    }

    async fn report(
        &self,
        lease: &LeaseDto,
        event: RunnerEventDto,
    ) -> Result<JobDto, media::runner::RunnerError> {
        let mut job = lease.job.clone();
        match &event {
            RunnerEventDto::StageFailed { retryable, .. } if *retryable => {
                job.state = JobStateDto::Queued;
            }
            RunnerEventDto::JobTransition { state, .. } => job.state = *state,
            _ => {}
        }
        self.events.lock().unwrap().push(event);
        Ok(job)
    }
}

#[tokio::test]
async fn expired_source_reports_a_stable_retryable_error_code() {
    let api = Arc::new(FakeApi {
        lease: Mutex::new(Some(lease())),
        events: Mutex::default(),
        heartbeats: AtomicUsize::new(0),
    });

    assert!(
        run_single_iteration(
            api.clone(),
            Arc::new(TypedFailingExecutor(
                media::runner::RunnerError::SourceExpired,
            )),
            Duration::from_millis(1),
        )
        .await
        .unwrap()
    );

    assert!(api.events.lock().unwrap().iter().any(|event| matches!(
        event,
        RunnerEventDto::StageFailed {
            retryable: true,
            error_code,
            ..
        } if error_code == "stream_expired"
    )));
}

#[tokio::test]
async fn expired_source_is_released_and_a_fresh_lease_completes() {
    let first = lease();
    let mut second = lease();
    second.lease_id =
        media_contract::PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb114").unwrap();
    let api = Arc::new(ReleasingApi {
        leases: Mutex::new(VecDeque::from([first, second])),
        events: Mutex::default(),
    });
    let executor = Arc::new(ExpireOnceExecutor {
        attempts: AtomicUsize::new(0),
    });

    assert!(
        run_single_iteration(api.clone(), executor.clone(), Duration::from_millis(1))
            .await
            .unwrap()
    );
    assert!(
        run_single_iteration(api.clone(), executor.clone(), Duration::from_millis(1))
            .await
            .unwrap()
    );

    assert_eq!(executor.attempts.load(Ordering::SeqCst), 2);
    let events = api.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        RunnerEventDto::StageFailed {
            retryable: true,
            error_code,
            ..
        } if error_code == "stream_expired"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        RunnerEventDto::JobTransition {
            state: JobStateDto::Completed,
            ..
        }
    )));
}

#[async_trait::async_trait]
impl JobExecutor for RecordingExecutor {
    async fn execute(
        &self,
        _: &LeaseDto,
        control: &RunnerControl,
    ) -> Result<ExecutionOutcome, media::runner::RunnerError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        control.stage_started(0, "resolve", 0).await?;
        control
            .stage_checkpoint(
                0,
                "resolve",
                0,
                BTreeMap::from([(
                    "downloaded_bytes".to_owned(),
                    CheckpointValueDto::Unsigned(1024),
                )]),
            )
            .await?;
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ExecutionOutcome::Completed)
    }
}

fn lease() -> LeaseDto {
    LeaseDto {
        lease_id: media_contract::PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb112").unwrap(),
        job: JobDto {
            id: media_contract::PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb111").unwrap(),
            provider: ProviderDto::Prowlarr,
            result_ref: "selection:one".to_owned(),
            state: JobStateDto::Leased,
            needs_action_reason: None,
            notify_scope: NotifyScopeDto::Initiator,
        },
        execution: Some(ExecutionSelectionDto::Prowlarr {
            source_identity: "mock:1".to_owned(),
            info_hash: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            uri: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".to_owned(),
            media_kind: media_contract::MediaKindDto::Movie,
            season: None,
            episode: None,
            library_title: Some("Movie".to_owned()),
            title: "Movie".to_owned(),
        }),
        completed_task_ordinals: Vec::new(),
        expires_at: "2026-07-13T12:00:00Z".to_owned(),
    }
}

fn session_refresh_lease() -> LeaseDto {
    let mut lease = lease();
    lease.job.provider = ProviderDto::Rezka;
    lease.job.result_ref =
        "selection:session-refresh:018f3f86-7b4c-7b4f-9b6a-6d62f45bb113".to_owned();
    lease.execution = Some(ExecutionSelectionDto::RezkaSessionRefresh {
        credential_request_id: "one-shot-request".to_owned(),
    });
    lease
}

#[tokio::test]
async fn loop_leases_heartbeats_reports_stages_and_runs_one_active_job() {
    let api = Arc::new(FakeApi {
        lease: Mutex::new(Some(lease())),
        events: Mutex::default(),
        heartbeats: AtomicUsize::new(0),
    });
    let executor = Arc::new(RecordingExecutor {
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
    });

    assert!(
        run_single_iteration(api.clone(), executor.clone(), Duration::from_millis(1))
            .await
            .unwrap()
    );

    assert!(api.heartbeats.load(Ordering::SeqCst) > 0);
    assert_eq!(executor.max_active.load(Ordering::SeqCst), 1);
    let events = api.events.lock().unwrap();
    assert!(matches!(events.first(), Some(RunnerEventDto::Started)));
    assert!(events.iter().any(|event| matches!(event, RunnerEventDto::StageStarted { stage_name, .. } if stage_name == "resolve")));
    assert!(events.iter().any(|event| matches!(
        event,
        RunnerEventDto::StageCheckpoint { stage_name, checkpoint, .. }
            if stage_name == "resolve" && checkpoint.get("downloaded_bytes") == Some(&CheckpointValueDto::Unsigned(1024))
    )));
    let transitions = events
        .iter()
        .filter_map(|event| match event {
            RunnerEventDto::JobTransition { state, .. } => Some(*state),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        transitions,
        vec![
            JobStateDto::Publishing,
            JobStateDto::PlexPending,
            JobStateDto::Completed,
        ]
    );
    assert!(matches!(
        events.last(),
        Some(RunnerEventDto::JobTransition {
            state: JobStateDto::Completed,
            ..
        })
    ));
}

/// Fails every heartbeat while recording report events, so a job can only end by
/// cooperative cancellation once the lease is treated as lost.
struct HeartbeatFailingApi {
    lease: Mutex<Option<LeaseDto>>,
    events: Mutex<Vec<RunnerEventDto>>,
    heartbeat_attempts: AtomicUsize,
}

#[async_trait::async_trait]
impl RunnerApi for HeartbeatFailingApi {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, media::runner::RunnerError> {
        Ok(self.lease.lock().unwrap().take())
    }
    async fn heartbeat(&self, _lease: &LeaseDto) -> Result<LeaseDto, media::runner::RunnerError> {
        self.heartbeat_attempts.fetch_add(1, Ordering::SeqCst);
        Err(media::runner::RunnerError::Service)
    }
    async fn report(
        &self,
        lease: &LeaseDto,
        event: RunnerEventDto,
    ) -> Result<media_contract::JobDto, media::runner::RunnerError> {
        let mut job = lease.job.clone();
        if let RunnerEventDto::JobTransition { state, .. } = &event {
            job.state = *state;
        }
        self.events.lock().unwrap().push(event);
        Ok(job)
    }
}

/// Runs until cancelled, so the test observes when the heartbeat task gives up
/// and flips the shared cancellation flag.
struct CancelAwareExecutor;

#[async_trait::async_trait]
impl JobExecutor for CancelAwareExecutor {
    async fn execute(
        &self,
        _: &LeaseDto,
        control: &RunnerControl,
    ) -> Result<ExecutionOutcome, media::runner::RunnerError> {
        for _ in 0..10_000 {
            if control.is_cancelled() {
                return Ok(ExecutionOutcome::Cancelled);
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        Ok(ExecutionOutcome::Completed)
    }
}

struct LateCancelApi {
    lease: Mutex<Option<LeaseDto>>,
    events: Mutex<Vec<RunnerEventDto>>,
    heartbeats: AtomicUsize,
    first_heartbeat: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl RunnerApi for LateCancelApi {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, media::runner::RunnerError> {
        Ok(self.lease.lock().unwrap().take())
    }

    async fn heartbeat(&self, lease: &LeaseDto) -> Result<LeaseDto, media::runner::RunnerError> {
        let call = self.heartbeats.fetch_add(1, Ordering::SeqCst);
        let mut current = lease.clone();
        if call == 0 {
            self.first_heartbeat.notify_one();
        } else {
            current.job.state = JobStateDto::Cancelled;
        }
        Ok(current)
    }

    async fn report(
        &self,
        lease: &LeaseDto,
        event: RunnerEventDto,
    ) -> Result<JobDto, media::runner::RunnerError> {
        let mut job = lease.job.clone();
        if let RunnerEventDto::JobTransition { state, .. } = &event {
            job.state = *state;
        }
        self.events.lock().unwrap().push(event);
        Ok(job)
    }
}

struct LateCancelExecutor {
    first_heartbeat: Arc<tokio::sync::Notify>,
    cleanups: AtomicUsize,
}

#[async_trait::async_trait]
impl JobExecutor for LateCancelExecutor {
    async fn execute(
        &self,
        _: &LeaseDto,
        _: &RunnerControl,
    ) -> Result<ExecutionOutcome, media::runner::RunnerError> {
        self.first_heartbeat.notified().await;
        Ok(ExecutionOutcome::Completed)
    }

    async fn cleanup_cancelled(&self, _: &LeaseDto) -> Result<(), media::runner::RunnerError> {
        self.cleanups.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn cancellation_after_execution_still_runs_cleanup_before_terminal_transition() {
    let first_heartbeat = Arc::new(tokio::sync::Notify::new());
    let api = Arc::new(LateCancelApi {
        lease: Mutex::new(Some(lease())),
        events: Mutex::default(),
        heartbeats: AtomicUsize::new(0),
        first_heartbeat: first_heartbeat.clone(),
    });
    let executor = Arc::new(LateCancelExecutor {
        first_heartbeat,
        cleanups: AtomicUsize::new(0),
    });

    assert!(
        run_single_iteration(api.clone(), executor.clone(), Duration::from_secs(60))
            .await
            .unwrap()
    );

    assert_eq!(executor.cleanups.load(Ordering::SeqCst), 1);
    let transitions = api
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            RunnerEventDto::JobTransition { state, .. } => Some(*state),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(transitions, vec![JobStateDto::Cancelled]);
    assert!(!api.events.lock().unwrap().iter().any(|event| matches!(
        event,
        RunnerEventDto::StageCompleted {
            task_ordinal: 2_000_000_000,
            stage_name,
            ..
        } if stage_name == "execution"
    )));
}

/// Builds a lease whose TTL (via `expires_at`) is `seconds` from now, so the
/// runner derives a realistic deadline for heartbeat-driven cancellation.
fn lease_expiring_in(seconds: i64) -> LeaseDto {
    let mut lease = lease();
    lease.expires_at = (time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    lease
}

#[tokio::test]
async fn heartbeat_failures_cancel_before_the_lease_ttl_elapses() {
    // The service never answers a heartbeat and the lease has a 3s TTL. Driving
    // cancellation from elapsed time versus the TTL, the runner must cancel while
    // the lease still has margin — before the service reaper could re-lease the
    // job. The previous fixed-failure-count logic (with a realistic heartbeat
    // interval and its backoff) needs ~5.5s to give up here, outlasting the TTL;
    // this test fails against that timing.
    let api = Arc::new(HeartbeatFailingApi {
        lease: Mutex::new(Some(lease_expiring_in(3))),
        events: Mutex::default(),
        heartbeat_attempts: AtomicUsize::new(0),
    });

    let started = std::time::Instant::now();
    assert!(
        run_single_iteration(
            api.clone(),
            Arc::new(CancelAwareExecutor),
            Duration::from_secs(2),
        )
        .await
        .unwrap()
    );
    let elapsed = started.elapsed();

    // Cancellation fired strictly before the 3s lease TTL.
    assert!(
        elapsed < Duration::from_secs(3),
        "cancellation must precede lease expiry, took {elapsed:?}"
    );
    // The heartbeat was retried across the outage rather than giving up at once.
    assert!(api.heartbeat_attempts.load(Ordering::SeqCst) >= 2);
    let events = api.events.lock().unwrap();
    let transitions = events
        .iter()
        .filter_map(|event| match event {
            RunnerEventDto::JobTransition { state, .. } => Some(*state),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(transitions, vec![JobStateDto::Cancelled]);
}

/// Errors `lease_next` a bounded number of times, then leases a real job once.
struct FlakyLeaseApi {
    remaining_errors: AtomicUsize,
    lease: Mutex<Option<LeaseDto>>,
    events: Mutex<Vec<RunnerEventDto>>,
}

#[async_trait::async_trait]
impl RunnerApi for FlakyLeaseApi {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, media::runner::RunnerError> {
        if self.remaining_errors.load(Ordering::SeqCst) > 0 {
            self.remaining_errors.fetch_sub(1, Ordering::SeqCst);
            return Err(media::runner::RunnerError::Service);
        }
        Ok(self.lease.lock().unwrap().take())
    }
    async fn heartbeat(&self, lease: &LeaseDto) -> Result<LeaseDto, media::runner::RunnerError> {
        Ok(lease.clone())
    }
    async fn report(
        &self,
        lease: &LeaseDto,
        event: RunnerEventDto,
    ) -> Result<media_contract::JobDto, media::runner::RunnerError> {
        let mut job = lease.job.clone();
        if let RunnerEventDto::JobTransition { state, .. } = &event {
            job.state = *state;
        }
        self.events.lock().unwrap().push(event);
        Ok(job)
    }
}

/// Signals through a oneshot the first time it executes a job.
struct SignalingExecutor {
    done: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[async_trait::async_trait]
impl JobExecutor for SignalingExecutor {
    async fn execute(
        &self,
        _: &LeaseDto,
        _: &RunnerControl,
    ) -> Result<ExecutionOutcome, media::runner::RunnerError> {
        if let Some(sender) = self.done.lock().unwrap().take() {
            let _ = sender.send(());
        }
        Ok(ExecutionOutcome::Completed)
    }
}

#[tokio::test]
async fn run_loop_survives_a_transient_iteration_error_and_keeps_working() {
    let api = Arc::new(FlakyLeaseApi {
        remaining_errors: AtomicUsize::new(1),
        lease: Mutex::new(Some(lease())),
        events: Mutex::default(),
    });
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let executor = Arc::new(SignalingExecutor {
        done: Mutex::new(Some(sender)),
    });
    let handle = tokio::spawn(run_loop(
        api.clone(),
        executor.clone(),
        Duration::from_millis(1),
        false,
    ));

    // The first iteration errors on lease_next; the loop backs off and the
    // second iteration leases and runs the job, proving the runner did not exit.
    tokio::time::timeout(Duration::from_secs(30), receiver)
        .await
        .expect("job should run after the transient lease error")
        .expect("executor should signal completion");
    handle.abort();

    assert!(
        api.events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, RunnerEventDto::Started))
    );
}

#[tokio::test]
async fn run_loop_exits_cleanly_after_one_processed_job_when_configured() {
    let api = Arc::new(FlakyLeaseApi {
        remaining_errors: AtomicUsize::new(0),
        lease: Mutex::new(Some(lease())),
        events: Mutex::default(),
    });
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let executor = Arc::new(SignalingExecutor {
        done: Mutex::new(Some(sender)),
    });
    let handle = tokio::spawn(run_loop(api, executor, Duration::from_millis(1), true));

    tokio::time::timeout(Duration::from_secs(30), receiver)
        .await
        .expect("job should be processed")
        .expect("executor should signal completion");
    tokio::time::timeout(Duration::from_secs(30), handle)
        .await
        .expect("runner should exit after the processed job")
        .expect("runner task should join")
        .expect("runner should exit successfully");
}

#[tokio::test]
async fn run_loop_exits_cleanly_when_the_service_requires_vpn_rotation() {
    tokio::time::timeout(
        Duration::from_secs(1),
        run_loop(
            Arc::new(RotationRequiredApi),
            Arc::new(FailingExecutor),
            Duration::from_millis(1),
            true,
        ),
    )
    .await
    .expect("the runner must not retry a rotation decision")
    .expect("VPN rotation is a clean process handoff");
}

/// Fails only stage progress events, keeping terminal transitions reliable.
struct ProgressFailingApi {
    lease: Mutex<Option<LeaseDto>>,
    events: Mutex<Vec<RunnerEventDto>>,
}

#[async_trait::async_trait]
impl RunnerApi for ProgressFailingApi {
    async fn lease_next(&self) -> Result<Option<LeaseDto>, media::runner::RunnerError> {
        Ok(self.lease.lock().unwrap().take())
    }
    async fn heartbeat(&self, lease: &LeaseDto) -> Result<LeaseDto, media::runner::RunnerError> {
        Ok(lease.clone())
    }
    async fn report(
        &self,
        lease: &LeaseDto,
        event: RunnerEventDto,
    ) -> Result<media_contract::JobDto, media::runner::RunnerError> {
        self.events.lock().unwrap().push(event.clone());
        match event {
            RunnerEventDto::StageStarted { .. }
            | RunnerEventDto::StageCheckpoint { .. }
            | RunnerEventDto::StageCompleted { .. } => Err(media::runner::RunnerError::Service),
            RunnerEventDto::JobTransition { state, .. } => {
                let mut job = lease.job.clone();
                job.state = state;
                Ok(job)
            }
            _ => Ok(lease.job.clone()),
        }
    }
}

#[tokio::test]
async fn progress_report_failures_do_not_fail_the_job() {
    let api = Arc::new(ProgressFailingApi {
        lease: Mutex::new(Some(lease())),
        events: Mutex::default(),
    });
    let executor = Arc::new(RecordingExecutor {
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
    });

    assert!(
        run_single_iteration(api.clone(), executor, Duration::from_millis(1))
            .await
            .unwrap()
    );

    // Despite every stage progress report failing, the job reached its terminal
    // transitions.
    let events = api.events.lock().unwrap();
    let transitions = events
        .iter()
        .filter_map(|event| match event {
            RunnerEventDto::JobTransition { state, .. } => Some(*state),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        transitions,
        vec![
            JobStateDto::Publishing,
            JobStateDto::PlexPending,
            JobStateDto::Completed,
        ]
    );
}

#[tokio::test]
async fn execution_failure_is_reported_without_stopping_the_runner_loop() {
    let api = Arc::new(FakeApi {
        lease: Mutex::new(Some(lease())),
        events: Mutex::default(),
        heartbeats: AtomicUsize::new(0),
    });

    assert!(
        run_single_iteration(
            api.clone(),
            Arc::new(FailingExecutor),
            Duration::from_millis(1),
        )
        .await
        .unwrap()
    );
    let events = api.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        RunnerEventDto::StageFailed {
            task_ordinal: 2_000_000_000,
            stage_name,
            retryable: true,
            error_code,
            ..
        } if stage_name == "execution" && error_code == "execution_failed"
    )));
}

#[tokio::test]
async fn episode_failure_is_reported_against_the_actual_episode_stage() {
    let api = Arc::new(FakeApi {
        lease: Mutex::new(Some(lease())),
        events: Mutex::default(),
        heartbeats: AtomicUsize::new(0),
    });

    assert!(
        run_single_iteration(
            api.clone(),
            Arc::new(TypedFailingExecutor(
                media::runner::RunnerError::SourceTransferTransient.at_stage(
                    7,
                    "media_pipeline",
                    1,
                ),
            )),
            Duration::from_millis(1),
        )
        .await
        .unwrap()
    );
    let events = api.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        RunnerEventDto::StageFailed {
            task_ordinal: 7,
            stage_name,
            stage_ordinal: 1,
            retryable: true,
            error_code,
        } if stage_name == "media_pipeline" && error_code == "source_transfer_transient"
    )));
}

#[tokio::test]
async fn consumed_session_refresh_failure_is_terminal_instead_of_requeued() {
    let api = Arc::new(FakeApi {
        lease: Mutex::new(Some(session_refresh_lease())),
        events: Mutex::default(),
        heartbeats: AtomicUsize::new(0),
    });

    assert!(
        run_single_iteration(
            api.clone(),
            Arc::new(FailingExecutor),
            Duration::from_millis(1),
        )
        .await
        .unwrap()
    );
    let events = api.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        RunnerEventDto::StageFailed {
            task_ordinal: 2_000_000_000,
            stage_name,
            retryable: false,
            error_code,
            ..
        } if stage_name == "execution" && error_code == "execution_failed"
    )));
}
