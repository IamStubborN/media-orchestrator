#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::AtomicBool,
        atomic::{AtomicUsize, Ordering},
    },
};

use media_api::{
    ApiState, IdempotencyError, IdempotencyRequest, IdempotencyStore, Reservation,
    StoredHttpResponse,
};
use media_core::{
    Actor, ClientId, ClientStore, CredentialDigest, Job, JobApplication, JobId, JobLease, JobState,
    JobStore, LeaseApplication, LeaseId, LeaseStore, NewJob, PortError, QueueStatus, ReadinessPort,
    UserId,
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
    async fn create(&self, _: NewJob) -> Result<Job, PortError> {
        unreachable!("foundation tests do not create jobs")
    }

    async fn find_for_owner(&self, _: JobId, _: UserId) -> Result<Option<Job>, PortError> {
        unreachable!("foundation tests do not read jobs")
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
    creates: Arc<AtomicUsize>,
    failures_remaining: Arc<AtomicUsize>,
    blocked: Arc<AtomicBool>,
    status: Arc<Mutex<QueueStatus>>,
}

impl Default for FakeJobStore {
    fn default() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
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
    async fn create(&self, job: NewJob) -> Result<Job, PortError> {
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

    async fn queue_status(&self) -> Result<QueueStatus, PortError> {
        Ok(*self.status.lock().unwrap())
    }
}

struct NoopLeaseStore;

#[async_trait::async_trait]
impl LeaseStore for NoopLeaseStore {
    async fn lease_next(
        &self,
        _: media_core::ClientId,
        _: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        Ok(None)
    }

    async fn heartbeat(
        &self,
        _: LeaseId,
        _: media_core::ClientId,
        _: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        Ok(None)
    }
}

#[derive(Clone, Default)]
pub struct FakeLeaseStore {
    lease: Arc<Mutex<Option<JobLease>>>,
}

impl FakeLeaseStore {
    pub fn with_lease(lease: JobLease) -> Self {
        Self {
            lease: Arc::new(Mutex::new(Some(lease))),
        }
    }
}

#[async_trait::async_trait]
impl LeaseStore for FakeLeaseStore {
    async fn lease_next(
        &self,
        runner: ClientId,
        _: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        Ok(self
            .lease
            .lock()
            .unwrap()
            .as_ref()
            .filter(|lease| lease.runner_client_id() == runner)
            .cloned())
    }

    async fn heartbeat(
        &self,
        lease_id: LeaseId,
        runner: ClientId,
        _: time::Duration,
    ) -> Result<Option<JobLease>, PortError> {
        Ok(self
            .lease
            .lock()
            .unwrap()
            .as_ref()
            .filter(|lease| lease.lease_id() == lease_id && lease.runner_client_id() == runner)
            .cloned())
    }
}

struct NoopIdempotencyStore;

#[async_trait::async_trait]
impl IdempotencyStore for NoopIdempotencyStore {
    async fn reserve(&self, _: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        Ok(Reservation::Reserved)
    }

    async fn complete(
        &self,
        _: IdempotencyRequest,
        _: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        Ok(())
    }

    async fn abort(&self, _: IdempotencyRequest) -> Result<(), IdempotencyError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct MemoryIdempotencyStore {
    entries: Arc<Mutex<HashMap<(ClientId, String), MemoryIdempotencyEntry>>>,
}

#[derive(Clone)]
struct MemoryIdempotencyEntry {
    fingerprint: [u8; 32],
    response: Option<StoredHttpResponse>,
}

#[async_trait::async_trait]
impl IdempotencyStore for MemoryIdempotencyStore {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        let key = (request.client_id(), request.key().to_owned());
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = entries.get(&key) else {
            entries.insert(
                key,
                MemoryIdempotencyEntry {
                    fingerprint: *request.fingerprint(),
                    response: None,
                },
            );
            return Ok(Reservation::Reserved);
        };
        if entry.fingerprint != *request.fingerprint() {
            return Ok(Reservation::Conflict);
        }

        Ok(entry
            .response
            .clone()
            .map_or(Reservation::InProgress, Reservation::Replay))
    }

    async fn complete(
        &self,
        request: IdempotencyRequest,
        response: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        let key = (request.client_id(), request.key().to_owned());
        let mut entries = self.entries.lock().unwrap();
        let entry = entries
            .get_mut(&key)
            .ok_or(IdempotencyError::Infrastructure)?;
        if entry.fingerprint != *request.fingerprint() {
            return Err(IdempotencyError::Infrastructure);
        }
        entry.response = Some(response);
        Ok(())
    }

    async fn abort(&self, request: IdempotencyRequest) -> Result<(), IdempotencyError> {
        let key = (request.client_id(), request.key().to_owned());
        let mut entries = self.entries.lock().unwrap();
        if entries
            .get(&key)
            .is_some_and(|entry| entry.fingerprint == *request.fingerprint())
        {
            entries.remove(&key);
        }
        Ok(())
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
    async fn reserve(&self, _: IdempotencyRequest) -> Result<Reservation, IdempotencyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Reservation::Reserved)
    }

    async fn complete(
        &self,
        _: IdempotencyRequest,
        _: StoredHttpResponse,
    ) -> Result<(), IdempotencyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn abort(&self, _: IdempotencyRequest) -> Result<(), IdempotencyError> {
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
        Arc::new(readiness),
    )
}

pub fn state_with_stores(
    clients: FakeClientStore,
    jobs: Arc<dyn JobStore>,
    leases: Arc<dyn LeaseStore>,
    idempotency: Arc<dyn IdempotencyStore>,
) -> ApiState {
    ApiState::new(
        Arc::new(JobApplication::new(jobs)),
        Arc::new(LeaseApplication::new(leases, time::Duration::seconds(60)).unwrap()),
        Arc::new(clients),
        idempotency,
        Arc::new(FakeReadiness::ready()),
    )
}
