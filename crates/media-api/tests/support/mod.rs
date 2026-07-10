#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use media_api::{
    ApiState, IdempotencyError, IdempotencyRequest, IdempotencyStore, Reservation,
    StoredHttpResponse,
};
use media_core::{
    Actor, ClientStore, CredentialDigest, Job, JobApplication, JobId, JobLease, JobStore,
    LeaseApplication, LeaseId, LeaseStore, NewJob, PortError, QueueStatus, ReadinessPort, UserId,
};

pub const VALID_TOKEN: &str = "primary-token";
pub const RUNNER_TOKEN: &str = "runner-token";
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
