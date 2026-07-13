#![allow(dead_code)]

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::AtomicBool,
        atomic::{AtomicUsize, Ordering},
    },
};

use media_api::{
    ApiState, IdempotencyError, IdempotencyGeneration, IdempotencyHandle, IdempotencyRequest,
    IdempotencyStore, OperationCompletionStore, Reservation, StoredHttpResponse,
};
use media_core::{
    Actor, ClientId, ClientStore, CredentialDigest, Job, JobApplication, JobEvent, JobEventKind,
    JobId, JobLease, JobState, JobStore, LeaseApplication, LeaseId, LeaseStore, NewJob,
    OperationKey, PortError, QueueStatus, ReadinessPort, UserId,
};

pub const VALID_TOKEN: &str = "primary-token";
pub const RUNNER_TOKEN: &str = "runner-token";
pub const SECONDARY_TOKEN: &str = "secondary-token";
pub const OTHER_RUNNER_TOKEN: &str = "other-runner-token";
pub const DISABLED_TOKEN: &str = "disabled-token";

pub struct FakeClientStore {
    actors: HashMap<[u8; 32], Actor>,
    fail: bool,
}

impl FakeClientStore {
    pub fn new(entries: impl IntoIterator<Item = (&'static str, Actor)>) -> Self {
        Self {
            actors: entries
                .into_iter()
                .map(|(token, actor)| (digest(token.as_bytes()), actor))
                .collect(),
            fail: false,
        }
    }

    pub fn failing() -> Self {
        Self {
            actors: HashMap::new(),
            fail: true,
        }
    }
}

#[async_trait::async_trait]
impl ClientStore for FakeClientStore {
    async fn find_by_digest(&self, digest: CredentialDigest) -> Result<Option<Actor>, PortError> {
        if self.fail {
            return Err(PortError::Infrastructure);
        }

        Ok(self.actors.get(digest.as_bytes()).cloned())
    }

    async fn upsert_client(&self, _: media_core::BootstrapClient) -> Result<(), PortError> {
        unreachable!("API tests do not bootstrap clients")
    }
}

fn digest(token: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    Sha256::digest(token).into()
}

struct NoopJobStore;

#[async_trait::async_trait]
impl JobStore for NoopJobStore {
    async fn create(&self, _: OperationKey, _: NewJob) -> Result<Job, PortError> {
        unreachable!("foundation tests do not create jobs")
    }

    async fn find_for_owner(&self, _: JobId, _: UserId) -> Result<Option<Job>, PortError> {
        unreachable!("foundation tests do not read jobs")
    }

    async fn list_for_owner(&self, _: UserId) -> Result<Vec<Job>, PortError> {
        Ok(Vec::new())
    }

    async fn cancel(&self, _: OperationKey, _: JobId, _: UserId) -> Result<Option<Job>, PortError> {
        Ok(None)
    }

    async fn queue_status(&self) -> Result<QueueStatus, PortError> {
        Ok(QueueStatus {
            queued: 0,
            active: false,
        })
    }
}

#[derive(Clone)]
pub struct FakeJobStore {
    jobs: Arc<Mutex<Vec<Job>>>,
    operations: Arc<Mutex<HashMap<OperationKey, FakeJobOperation>>>,
    creates: Arc<AtomicUsize>,
    failures_remaining: Arc<AtomicUsize>,
    blocked: Arc<AtomicBool>,
    status: Arc<Mutex<QueueStatus>>,
}

#[derive(Clone)]
enum FakeJobOperation {
    Pending,
    Completed(Job),
}

impl Default for FakeJobStore {
    fn default() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            operations: Arc::new(Mutex::new(HashMap::new())),
            creates: Arc::new(AtomicUsize::new(0)),
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            blocked: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new(QueueStatus {
                queued: 0,
                active: false,
            })),
        }
    }
}

impl FakeJobStore {
    pub fn with_job(job: Job) -> Self {
        let store = Self::default();
        store.jobs.lock().unwrap().push(job);
        store
    }

    pub fn with_jobs(jobs: impl IntoIterator<Item = Job>) -> Self {
        let store = Self::default();
        store.jobs.lock().unwrap().extend(jobs);
        store
    }

    pub fn with_status(status: QueueStatus) -> Self {
        let store = Self::default();
        *store.status.lock().unwrap() = status;
        store
    }

    pub fn fail_creates(&self, count: usize) {
        self.failures_remaining.store(count, Ordering::SeqCst);
    }

    pub fn block_creates(&self) {
        self.blocked.store(true, Ordering::SeqCst);
    }

    pub fn unblock_creates(&self) {
        self.blocked.store(false, Ordering::SeqCst);
    }

    pub fn create_calls(&self) -> usize {
        self.creates.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl JobStore for FakeJobStore {
    async fn create(&self, operation: OperationKey, job: NewJob) -> Result<Job, PortError> {
        enum Claim {
            Replay(Job),
            Wait,
            Own,
        }

        loop {
            let claim = {
                let mut operations = self.operations.lock().unwrap();
                match operations.get(&operation) {
                    Some(FakeJobOperation::Completed(job)) => Claim::Replay(job.clone()),
                    Some(FakeJobOperation::Pending) => Claim::Wait,
                    None => {
                        operations.insert(operation, FakeJobOperation::Pending);
                        Claim::Own
                    }
                }
            };
            match claim {
                Claim::Replay(job) => return Ok(job),
                Claim::Wait => tokio::task::yield_now().await,
                Claim::Own => break,
            }
        }
        self.creates.fetch_add(1, Ordering::SeqCst);
        while self.blocked.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        if self
            .failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            self.operations.lock().unwrap().remove(&operation);
            return Err(PortError::Infrastructure);
        }

        let persisted = Job::rehydrate(
            job.id(),
            job.owner_id(),
            job.provider(),
            job.result_ref().to_owned(),
            JobState::Queued,
            None,
            job.notify_scope(),
        )
        .map_err(|_| PortError::Infrastructure)?;
        self.jobs.lock().unwrap().push(persisted.clone());
        self.operations
            .lock()
            .unwrap()
            .insert(operation, FakeJobOperation::Completed(persisted.clone()));
        Ok(persisted)
    }

    async fn find_for_owner(&self, id: JobId, owner: UserId) -> Result<Option<Job>, PortError> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .find(|job| job.id() == id && job.owner_id() == owner)
            .cloned())
    }

    async fn list_for_owner(&self, owner: UserId) -> Result<Vec<Job>, PortError> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|job| job.owner_id() == owner)
            .cloned()
            .collect())
    }

    async fn cancel(
        &self,
        _: OperationKey,
        id: JobId,
        owner: UserId,
    ) -> Result<Option<Job>, PortError> {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(index) = jobs
            .iter()
            .position(|job| job.id() == id && job.owner_id() == owner)
        else {
            return Ok(None);
        };
        let current = &jobs[index];
        let state = match current.state() {
            JobState::Queued => JobState::Cancelled,
            JobState::Completed | JobState::Cancelled => current.state(),
            _ => JobState::CancelRequested,
        };
        let updated = Job::rehydrate(
            current.id(),
            current.owner_id(),
            current.provider(),
            current.result_ref().to_owned(),
            state,
            None,
            current.notify_scope(),
        )
        .map_err(|_| PortError::Infrastructure)?;
        jobs[index] = updated.clone();
        Ok(Some(updated))
    }

    async fn retry(
        &self,
        _: OperationKey,
        id: JobId,
        owner: UserId,
    ) -> Result<Option<Job>, PortError> {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(index) = jobs
            .iter()
            .position(|job| job.id() == id && job.owner_id() == owner)
        else {
            return Ok(None);
        };
        let current = &jobs[index];
        if !matches!(current.state(), JobState::Partial | JobState::Failed) {
            return Err(PortError::Conflict);
        }
        let updated = Job::rehydrate(
            current.id(),
            current.owner_id(),
            current.provider(),
            current.result_ref().to_owned(),
            JobState::Queued,
            None,
            current.notify_scope(),
        )
        .map_err(|_| PortError::Infrastructure)?;
        jobs[index] = updated.clone();
        Ok(Some(updated))
    }

    async fn queue_status(&self) -> Result<QueueStatus, PortError> {
        Ok(*self.status.lock().unwrap())
    }
}

struct NoopLeaseStore;

#[async_trait::async_trait]
impl LeaseStore for NoopLeaseStore {
    async fn lease_next(
        &self,
        _: OperationKey,
        _: media_core::ClientId,
        _: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        Ok(None)
    }

    async fn heartbeat(
        &self,
        _: OperationKey,
        _: LeaseId,
        _: media_core::ClientId,
        _: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        Ok(None)
    }

    async fn report_event(
        &self,
        _: OperationKey,
        _: LeaseId,
        _: ClientId,
        _: JobEvent,
    ) -> Result<Option<Job>, PortError> {
        Ok(None)
    }
}

#[derive(Clone, Default)]
pub struct FakeLeaseStore {
    state: Arc<Mutex<FakeLeaseState>>,
    lease_calls: Arc<AtomicUsize>,
    heartbeat_calls: Arc<AtomicUsize>,
}

#[derive(Default)]
struct FakeLeaseState {
    lease: Option<JobLease>,
    lease_operations: HashMap<OperationKey, Option<JobLease>>,
    heartbeat_operations: HashMap<OperationKey, Option<JobLease>>,
}

impl FakeLeaseStore {
    pub fn with_lease(lease: JobLease) -> Self {
        let store = Self::default();
        store.state.lock().unwrap().lease = Some(lease);
        store
    }

    pub fn set_lease(&self, lease: Option<JobLease>) {
        self.state.lock().unwrap().lease = lease;
    }

    pub fn lease_calls(&self) -> usize {
        self.lease_calls.load(Ordering::SeqCst)
    }

    pub fn heartbeat_calls(&self) -> usize {
        self.heartbeat_calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LeaseStore for FakeLeaseStore {
    async fn lease_next(
        &self,
        operation: OperationKey,
        runner: ClientId,
        _: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        let mut state = self.state.lock().unwrap();
        if let Some(result) = state.lease_operations.get(&operation).cloned() {
            return Ok(result);
        }
        self.lease_calls.fetch_add(1, Ordering::SeqCst);
        let result = state
            .lease
            .as_ref()
            .filter(|lease| lease.runner_client_id() == runner)
            .cloned();
        state.lease_operations.insert(operation, result.clone());
        Ok(result)
    }

    async fn heartbeat(
        &self,
        operation: OperationKey,
        lease_id: LeaseId,
        runner: ClientId,
        _: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        let mut state = self.state.lock().unwrap();
        if let Some(result) = state.heartbeat_operations.get(&operation).cloned() {
            return Ok(result);
        }
        self.heartbeat_calls.fetch_add(1, Ordering::SeqCst);
        let result = state
            .lease
            .as_ref()
            .filter(|lease| lease.lease_id() == lease_id && lease.runner_client_id() == runner)
            .cloned();
        state.heartbeat_operations.insert(operation, result.clone());
        Ok(result)
    }

    async fn report_event(
        &self,
        _: OperationKey,
        lease_id: LeaseId,
        runner: ClientId,
        event: JobEvent,
    ) -> Result<Option<Job>, PortError> {
        let mut state = self.state.lock().unwrap();
        let Some(lease) = state
            .lease
            .as_ref()
            .filter(|lease| lease.lease_id() == lease_id && lease.runner_client_id() == runner)
        else {
            return Ok(None);
        };
        let job_state = match event.kind() {
            JobEventKind::Started => JobState::Running,
            JobEventKind::JobTransition { state, .. } => *state,
            _ => lease.job().state(),
        };
        let job = Job::rehydrate(
            lease.job().id(),
            lease.job().owner_id(),
            lease.job().provider(),
            lease.job().result_ref().to_owned(),
            job_state,
            match event.kind() {
                JobEventKind::JobTransition {
                    needs_action_reason,
                    ..
                } => *needs_action_reason,
                _ => lease.job().needs_action_reason(),
            },
            lease.job().notify_scope(),
        )
        .map_err(|_| PortError::Infrastructure)?;
        state.lease = Some(JobLease::new(
            lease.lease_id(),
            job.clone(),
            lease.runner_client_id(),
            lease.expires_at(),
        ));
        Ok(Some(job))
    }
}

#[derive(Clone)]
pub struct ControlledIdempotencyStore {
    reservations: Arc<Mutex<VecDeque<ControlledReservation>>>,
    fail_complete: bool,
    fail_abort: bool,
    complete_calls: Arc<AtomicUsize>,
    abort_calls: Arc<AtomicUsize>,
}

#[derive(Clone)]
pub enum ControlledReservation {
    Reserved,
    Replay(StoredHttpResponse),
    Conflict,
    InProgress,
}

impl ControlledIdempotencyStore {
    pub fn new(reservations: impl IntoIterator<Item = ControlledReservation>) -> Self {
        Self {
            reservations: Arc::new(Mutex::new(reservations.into_iter().collect())),
            fail_complete: false,
            fail_abort: false,
            complete_calls: Arc::new(AtomicUsize::new(0)),
            abort_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn failing_complete(mut self) -> Self {
        self.fail_complete = true;
        self
    }

    pub fn failing_abort(mut self) -> Self {
        self.fail_abort = true;
        self
    }

    pub fn complete_calls(&self) -> usize {
        self.complete_calls.load(Ordering::SeqCst)
    }

    pub fn abort_calls(&self) -> usize {
        self.abort_calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl IdempotencyStore for ControlledIdempotencyStore {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        let reservation = self
            .reservations
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(IdempotencyError::Infrastructure)?;
        let handle = IdempotencyHandle::new(request, IdempotencyGeneration::new());
        Ok(match reservation {
            ControlledReservation::Reserved => Reservation::Reserved(handle),
            ControlledReservation::Replay(response) => Reservation::Replay { handle, response },
            ControlledReservation::Conflict => Reservation::Conflict,
            ControlledReservation::InProgress => Reservation::InProgress(handle),
        })
    }

    async fn complete(
        &self,
        _: &IdempotencyHandle,
        _: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_complete {
            Err(IdempotencyError::Infrastructure)
        } else {
            Ok(())
        }
    }

    async fn abort_in_progress(&self, _: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        self.abort_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_abort {
            Err(IdempotencyError::Infrastructure)
        } else {
            Ok(())
        }
    }
}

struct NoopIdempotencyStore;

#[async_trait::async_trait]
impl IdempotencyStore for NoopIdempotencyStore {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        Ok(Reservation::Reserved(IdempotencyHandle::new(
            request,
            IdempotencyGeneration::new(),
        )))
    }

    async fn complete(
        &self,
        _: &IdempotencyHandle,
        _: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        Ok(())
    }

    async fn abort_in_progress(&self, _: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct MemoryIdempotencyStore {
    entries: Arc<Mutex<HashMap<(ClientId, String), MemoryIdempotencyEntry>>>,
}

#[derive(Clone, Default)]
pub struct CommitThenErrorIdempotencyStore {
    entry: Arc<Mutex<Option<CommitThenErrorEntry>>>,
    abort_calls: Arc<AtomicUsize>,
}

#[derive(Clone)]
enum CommitThenErrorEntry {
    InProgress(IdempotencyHandle),
    Completed(IdempotencyHandle, StoredHttpResponse),
}

impl CommitThenErrorIdempotencyStore {
    pub fn abort_calls(&self) -> usize {
        self.abort_calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl IdempotencyStore for CommitThenErrorIdempotencyStore {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        let mut entry = self.entry.lock().unwrap();
        match entry.as_ref() {
            None => {
                let handle = IdempotencyHandle::new(request, IdempotencyGeneration::new());
                *entry = Some(CommitThenErrorEntry::InProgress(handle.clone()));
                Ok(Reservation::Reserved(handle))
            }
            Some(CommitThenErrorEntry::InProgress(handle)) => {
                if handle.fingerprint() == request.fingerprint() {
                    Ok(Reservation::InProgress(handle.clone()))
                } else {
                    Ok(Reservation::Conflict)
                }
            }
            Some(CommitThenErrorEntry::Completed(handle, response)) => {
                if handle.fingerprint() == request.fingerprint() {
                    Ok(Reservation::Replay {
                        handle: handle.clone(),
                        response: response.clone(),
                    })
                } else {
                    Ok(Reservation::Conflict)
                }
            }
        }
    }

    async fn complete(
        &self,
        handle: &IdempotencyHandle,
        response: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        let mut entry = self.entry.lock().unwrap();
        match entry.as_ref() {
            Some(CommitThenErrorEntry::InProgress(current)) if current == handle => {
                *entry = Some(CommitThenErrorEntry::Completed(handle.clone(), response));
                Err(IdempotencyError::Infrastructure)
            }
            _ => Err(IdempotencyError::Infrastructure),
        }
    }

    async fn abort_in_progress(&self, handle: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        self.abort_calls.fetch_add(1, Ordering::SeqCst);
        let mut entry = self.entry.lock().unwrap();
        match entry.as_ref() {
            Some(CommitThenErrorEntry::InProgress(current)) if current == handle => {
                *entry = None;
                Ok(())
            }
            _ => Err(IdempotencyError::Infrastructure),
        }
    }
}

#[derive(Clone)]
struct MemoryIdempotencyEntry {
    fingerprint: [u8; 32],
    generation: IdempotencyGeneration,
    response: Option<StoredHttpResponse>,
}

#[async_trait::async_trait]
impl IdempotencyStore for MemoryIdempotencyStore {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        let key = (request.client_id(), request.key().to_owned());
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = entries.get(&key) else {
            let generation = IdempotencyGeneration::new();
            entries.insert(
                key,
                MemoryIdempotencyEntry {
                    fingerprint: *request.fingerprint(),
                    generation,
                    response: None,
                },
            );
            return Ok(Reservation::Reserved(IdempotencyHandle::new(
                request, generation,
            )));
        };
        if entry.fingerprint != *request.fingerprint() {
            return Ok(Reservation::Conflict);
        }

        let handle = IdempotencyHandle::new(request, entry.generation);
        Ok(match entry.response.clone() {
            Some(response) => Reservation::Replay { handle, response },
            None => Reservation::InProgress(handle),
        })
    }

    async fn complete(
        &self,
        handle: &IdempotencyHandle,
        response: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        let key = (handle.client_id(), handle.key().to_owned());
        let mut entries = self.entries.lock().unwrap();
        let entry = entries
            .get_mut(&key)
            .ok_or(IdempotencyError::Infrastructure)?;
        if entry.fingerprint != *handle.fingerprint() || entry.generation != handle.generation() {
            return Err(IdempotencyError::Infrastructure);
        }
        match entry.response.as_ref() {
            None => {
                entry.response = Some(response);
                Ok(())
            }
            Some(existing) if existing == &response => Ok(()),
            Some(_) => Err(IdempotencyError::Infrastructure),
        }
    }

    async fn abort_in_progress(&self, handle: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        let key = (handle.client_id(), handle.key().to_owned());
        let mut entries = self.entries.lock().unwrap();
        if entries.get(&key).is_some_and(|entry| {
            entry.fingerprint == *handle.fingerprint()
                && entry.generation == handle.generation()
                && entry.response.is_none()
        }) {
            entries.remove(&key);
            return Ok(());
        }
        Err(IdempotencyError::Infrastructure)
    }
}

#[derive(Clone, Default)]
pub struct RecordingIdempotencyStore {
    calls: Arc<AtomicUsize>,
}

impl RecordingIdempotencyStore {
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl IdempotencyStore for RecordingIdempotencyStore {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Reservation::Reserved(IdempotencyHandle::new(
            request,
            IdempotencyGeneration::new(),
        )))
    }

    async fn complete(
        &self,
        _: &IdempotencyHandle,
        _: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn abort_in_progress(&self, _: &IdempotencyHandle) -> Result<(), IdempotencyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub struct FakeReadiness {
    result: Result<bool, PortError>,
}

impl FakeReadiness {
    pub const fn ready() -> Self {
        Self { result: Ok(true) }
    }

    pub const fn not_ready() -> Self {
        Self { result: Ok(false) }
    }

    pub const fn failing() -> Self {
        Self {
            result: Err(PortError::Infrastructure),
        }
    }
}

#[async_trait::async_trait]
impl ReadinessPort for FakeReadiness {
    async fn is_ready(&self) -> Result<bool, PortError> {
        self.result
    }
}

#[derive(Clone)]
pub struct FakeOperationCompletionStore {
    completed: bool,
    calls: Arc<AtomicUsize>,
}

impl FakeOperationCompletionStore {
    pub fn incomplete() -> Self {
        Self {
            completed: false,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn completed() -> Self {
        Self {
            completed: true,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl OperationCompletionStore for FakeOperationCompletionStore {
    async fn is_completed(&self, _: OperationKey) -> Result<bool, IdempotencyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.completed)
    }
}

pub fn state(clients: FakeClientStore, readiness: FakeReadiness) -> ApiState {
    state_with_idempotency(clients, readiness, Arc::new(NoopIdempotencyStore))
}

pub fn state_with_idempotency(
    clients: FakeClientStore,
    readiness: FakeReadiness,
    idempotency: Arc<dyn IdempotencyStore>,
) -> ApiState {
    ApiState::new(
        Arc::new(JobApplication::new(Arc::new(NoopJobStore))),
        Arc::new(
            LeaseApplication::new(Arc::new(NoopLeaseStore), time::Duration::seconds(60)).unwrap(),
        ),
        Arc::new(clients),
        idempotency,
        Arc::new(FakeOperationCompletionStore::incomplete()),
        Arc::new(readiness),
    )
}

pub fn state_with_stores(
    clients: FakeClientStore,
    jobs: Arc<dyn JobStore>,
    leases: Arc<dyn LeaseStore>,
    idempotency: Arc<dyn IdempotencyStore>,
) -> ApiState {
    state_with_stores_and_completion(
        clients,
        jobs,
        leases,
        idempotency,
        Arc::new(FakeOperationCompletionStore::incomplete()),
    )
}

pub fn state_with_stores_and_completion(
    clients: FakeClientStore,
    jobs: Arc<dyn JobStore>,
    leases: Arc<dyn LeaseStore>,
    idempotency: Arc<dyn IdempotencyStore>,
    operations: Arc<dyn OperationCompletionStore>,
) -> ApiState {
    ApiState::new(
        Arc::new(JobApplication::new(jobs)),
        Arc::new(LeaseApplication::new(leases, time::Duration::seconds(60)).unwrap()),
        Arc::new(clients),
        idempotency,
        operations,
        Arc::new(FakeReadiness::ready()),
    )
}
