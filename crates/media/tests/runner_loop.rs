use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use media::runner::{
    ExecutionOutcome, JobExecutor, RunnerApi, RunnerControl, run_single_iteration,
};
use media_contract::{
    ExecutionSelectionDto, JobDto, JobStateDto, LeaseDto, NotifyScopeDto, ProviderDto,
    RunnerEventDto,
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
impl JobExecutor for RecordingExecutor {
    async fn execute(
        &self,
        _: &LeaseDto,
        control: &RunnerControl,
    ) -> Result<ExecutionOutcome, media::runner::RunnerError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        control.stage_started(0, "resolve", 0).await?;
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
            title: "Movie".to_owned(),
        }),
        expires_at: "2026-07-13T12:00:00Z".to_owned(),
    }
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
            stage_name,
            retryable: true,
            error_code,
            ..
        } if stage_name == "execution" && error_code == "execution_failed"
    )));
}
