use std::sync::Arc;

use crate::{
    Actor, Job, JobEvent, JobId, JobLease, JobStore, JobValidationError, LeaseId, LeaseStore,
    NewJob, NotifyScope, OperationKey, PortError, Provider, QueueStatus, RunnerLifecycle,
    RunnerLifecycleStore, RunnerLifecycleUpdate,
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NewJobCommand {
    pub provider: Provider,
    pub result_ref: String,
    pub notify_scope: NotifyScope,
}

pub struct RunnerLifecycleApplication {
    store: Arc<dyn RunnerLifecycleStore>,
}

impl RunnerLifecycleApplication {
    #[must_use]
    pub fn new(store: Arc<dyn RunnerLifecycleStore>) -> Self {
        Self { store }
    }

    pub async fn get(&self, actor: &Actor) -> Result<RunnerLifecycle, ApplicationError> {
        actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;
        self.store.get().await.map_err(Into::into)
    }

    pub async fn update(
        &self,
        actor: &Actor,
        update: RunnerLifecycleUpdate,
    ) -> Result<RunnerLifecycle, ApplicationError> {
        actor
            .require_lifecycle()
            .map_err(|_| ApplicationError::Forbidden)?;
        self.store.update(update).await.map_err(Into::into)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ApplicationError {
    #[error("operation is forbidden")]
    Forbidden,
    #[error("invalid input: {0}")]
    InvalidInput(#[source] JobValidationError),
    #[error("resource was not found")]
    NotFound,
    #[error("operation conflicts with current state")]
    Conflict,
    #[error("infrastructure operation failed")]
    Infrastructure,
}

impl From<PortError> for ApplicationError {
    fn from(error: PortError) -> Self {
        match error {
            PortError::Conflict => Self::Conflict,
            PortError::Infrastructure => Self::Infrastructure,
        }
    }
}

pub struct JobApplication {
    store: Arc<dyn JobStore>,
}

impl JobApplication {
    #[must_use]
    pub fn new(store: Arc<dyn JobStore>) -> Self {
        Self { store }
    }

    pub async fn create_job(
        &self,
        actor: &Actor,
        operation: OperationKey,
        command: NewJobCommand,
    ) -> Result<Job, ApplicationError> {
        let owner_id = actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;
        self.create_job_for_owner(owner_id, operation, command)
            .await
    }

    pub async fn create_job_for_owner(
        &self,
        owner_id: crate::UserId,
        operation: OperationKey,
        command: NewJobCommand,
    ) -> Result<Job, ApplicationError> {
        let job = NewJob::new(
            JobId::new(),
            owner_id,
            command.provider,
            command.result_ref,
            command.notify_scope,
        )
        .map_err(ApplicationError::InvalidInput)?;

        self.store.create(operation, job).await.map_err(Into::into)
    }

    pub async fn get_job(&self, actor: &Actor, id: JobId) -> Result<Job, ApplicationError> {
        let owner_id = actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;

        self.store
            .find_for_owner(id, owner_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn get_job_detail(
        &self,
        actor: &Actor,
        id: JobId,
    ) -> Result<crate::JobDetail, ApplicationError> {
        let owner_id = actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;

        self.store
            .find_detail_for_owner(id, owner_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn list_jobs(&self, actor: &Actor) -> Result<Vec<Job>, ApplicationError> {
        let owner_id = actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;
        self.store
            .list_for_owner(owner_id)
            .await
            .map_err(Into::into)
    }

    pub async fn cancel_job(
        &self,
        actor: &Actor,
        operation: OperationKey,
        id: JobId,
    ) -> Result<Job, ApplicationError> {
        let owner_id = actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;
        self.store
            .cancel(operation, id, owner_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn retry_job(
        &self,
        actor: &Actor,
        operation: OperationKey,
        id: JobId,
    ) -> Result<Job, ApplicationError> {
        let owner_id = actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;
        self.store
            .retry(operation, id, owner_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn queue_status(&self, actor: &Actor) -> Result<QueueStatus, ApplicationError> {
        actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;

        self.store.queue_status().await.map_err(Into::into)
    }
}

pub struct LeaseApplication {
    store: Arc<dyn LeaseStore>,
    ttl: time::Duration,
}

const MIN_LEASE_TTL: time::Duration = time::Duration::seconds(30);
const MAX_LEASE_TTL: time::Duration = time::Duration::seconds(300);

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum LeaseTtlError {
    #[error("lease TTL {actual:?} is shorter than 30 seconds")]
    TooShort { actual: time::Duration },
    #[error("lease TTL {actual:?} is longer than 300 seconds")]
    TooLong { actual: time::Duration },
}

impl LeaseApplication {
    pub fn new(store: Arc<dyn LeaseStore>, ttl: time::Duration) -> Result<Self, LeaseTtlError> {
        if ttl < MIN_LEASE_TTL {
            return Err(LeaseTtlError::TooShort { actual: ttl });
        }
        if ttl > MAX_LEASE_TTL {
            return Err(LeaseTtlError::TooLong { actual: ttl });
        }

        Ok(Self { store, ttl })
    }

    pub async fn lease_next(
        &self,
        actor: &Actor,
        operation: OperationKey,
    ) -> Result<Option<JobLease>, ApplicationError> {
        let runner = actor
            .require_runner()
            .map_err(|_| ApplicationError::Forbidden)?;

        self.store
            .lease_next(operation, runner, self.ttl)
            .await
            .map_err(Into::into)
    }

    pub async fn heartbeat(
        &self,
        actor: &Actor,
        operation: OperationKey,
        lease: LeaseId,
    ) -> Result<JobLease, ApplicationError> {
        let runner = actor
            .require_runner()
            .map_err(|_| ApplicationError::Forbidden)?;

        self.store
            .heartbeat(operation, lease, runner, self.ttl)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn report_event(
        &self,
        actor: &Actor,
        operation: OperationKey,
        lease: LeaseId,
        event: JobEvent,
    ) -> Result<Job, ApplicationError> {
        let runner = actor
            .require_runner()
            .map_err(|_| ApplicationError::Forbidden)?;
        self.store
            .report_event(operation, lease, runner, event)
            .await?
            .ok_or(ApplicationError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        sync::{Arc, Mutex},
        task::{Context, Poll, Waker},
    };

    use super::{ApplicationError, JobApplication, LeaseApplication, LeaseTtlError, NewJobCommand};
    use crate::{
        PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientId, ClientRole, Job, JobId, JobLease,
        JobState, JobStore, JobValidationError, LeaseId, LeaseStore, NewJob, NotifyScope,
        OperationKey, PortError, Provider, QueueStatus, RUNNER_CLIENT_ID, UserId,
    };

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);

        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn hermes_actor() -> Actor {
        Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap()
    }

    fn runner_actor() -> Actor {
        Actor::new(RUNNER_CLIENT_ID, None, ClientRole::Runner).unwrap()
    }

    const fn operation_key(marker: u8) -> OperationKey {
        OperationKey::from_bytes([marker; 32])
    }

    fn persisted_job(id: JobId, owner_id: UserId, state: JobState) -> Job {
        Job::rehydrate(
            id,
            owner_id,
            Provider::Rezka,
            "rezka-selection".to_owned(),
            state,
            None,
            NotifyScope::Initiator,
        )
        .unwrap()
    }

    struct FakeJobStore {
        created: Mutex<Vec<(OperationKey, NewJob)>>,
        jobs: Mutex<Vec<Job>>,
        status: QueueStatus,
        failure: Option<PortError>,
    }

    impl FakeJobStore {
        fn empty() -> Self {
            Self {
                created: Mutex::new(Vec::new()),
                jobs: Mutex::new(Vec::new()),
                status: QueueStatus {
                    queued: 0,
                    active: false,
                },
                failure: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl JobStore for FakeJobStore {
        async fn create(&self, operation: OperationKey, job: NewJob) -> Result<Job, PortError> {
            if let Some(error) = self.failure {
                return Err(error);
            }

            self.created.lock().unwrap().push((operation, job.clone()));
            Job::rehydrate(
                job.id(),
                job.owner_id(),
                job.provider(),
                job.result_ref().to_owned(),
                JobState::Queued,
                None,
                job.notify_scope(),
            )
            .map_err(|_| PortError::Infrastructure)
        }

        async fn find_for_owner(&self, id: JobId, owner: UserId) -> Result<Option<Job>, PortError> {
            if let Some(error) = self.failure {
                return Err(error);
            }

            Ok(self
                .jobs
                .lock()
                .unwrap()
                .iter()
                .find(|job| job.id() == id && job.owner_id() == owner)
                .cloned())
        }

        async fn list_for_owner(&self, owner: UserId) -> Result<Vec<Job>, PortError> {
            if let Some(error) = self.failure {
                return Err(error);
            }
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
            self.find_for_owner(id, owner).await
        }

        async fn queue_status(&self) -> Result<QueueStatus, PortError> {
            self.failure.map_or(Ok(self.status), Err)
        }
    }

    #[derive(Debug, Copy, Clone, Eq, PartialEq)]
    enum LeaseCall {
        Next {
            operation: OperationKey,
            runner: ClientId,
            ttl: time::Duration,
        },
        Heartbeat {
            operation: OperationKey,
            lease: LeaseId,
            runner: ClientId,
            ttl: time::Duration,
        },
    }

    struct FakeLeaseStore {
        calls: Mutex<Vec<LeaseCall>>,
        lease: Option<JobLease>,
        failure: Option<PortError>,
    }

    #[async_trait::async_trait]
    impl LeaseStore for FakeLeaseStore {
        async fn lease_next(
            &self,
            operation: OperationKey,
            runner: ClientId,
            ttl: time::Duration,
        ) -> Result<Option<JobLease>, PortError> {
            if let Some(error) = self.failure {
                return Err(error);
            }
            self.calls.lock().unwrap().push(LeaseCall::Next {
                operation,
                runner,
                ttl,
            });
            Ok(self.lease.clone())
        }

        async fn heartbeat(
            &self,
            operation: OperationKey,
            lease: LeaseId,
            runner: ClientId,
            ttl: time::Duration,
        ) -> Result<Option<JobLease>, PortError> {
            if let Some(error) = self.failure {
                return Err(error);
            }
            self.calls.lock().unwrap().push(LeaseCall::Heartbeat {
                operation,
                lease,
                runner,
                ttl,
            });
            Ok(self.lease.clone())
        }

        async fn report_event(
            &self,
            _: OperationKey,
            _: LeaseId,
            _: ClientId,
            _: crate::JobEvent,
        ) -> Result<Option<Job>, PortError> {
            Ok(self.lease.as_ref().map(|lease| lease.job().clone()))
        }
    }

    #[test]
    fn create_job_uses_the_authenticated_users_identity() {
        let store = Arc::new(FakeJobStore::empty());
        let application = JobApplication::new(store.clone());

        let job = block_on(application.create_job(
            &hermes_actor(),
            operation_key(1),
            NewJobCommand {
                provider: Provider::Rezka,
                result_ref: "selection-1".to_owned(),
                notify_scope: NotifyScope::Family,
            },
        ))
        .unwrap();

        assert_eq!(job.owner_id(), PRIMARY_USER_ID);
        let created = store.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, operation_key(1));
        assert_eq!(created[0].1.owner_id(), PRIMARY_USER_ID);
    }

    #[test]
    fn runner_actor_cannot_create_or_read_user_jobs() {
        let store = Arc::new(FakeJobStore::empty());
        let application = JobApplication::new(store.clone());

        let create_error = block_on(application.create_job(
            &runner_actor(),
            operation_key(2),
            NewJobCommand {
                provider: Provider::Rezka,
                result_ref: "selection-1".to_owned(),
                notify_scope: NotifyScope::Initiator,
            },
        ))
        .unwrap_err();
        let read_error = block_on(application.get_job(&runner_actor(), JobId::new())).unwrap_err();

        assert_eq!(create_error, ApplicationError::Forbidden);
        assert_eq!(read_error, ApplicationError::Forbidden);
        assert!(store.created.lock().unwrap().is_empty());
    }

    #[test]
    fn create_job_rejects_an_empty_result_reference_before_storage() {
        let store = Arc::new(FakeJobStore::empty());
        let application = JobApplication::new(store.clone());

        let error = block_on(application.create_job(
            &hermes_actor(),
            operation_key(3),
            NewJobCommand {
                provider: Provider::Prowlarr,
                result_ref: "   ".to_owned(),
                notify_scope: NotifyScope::Initiator,
            },
        ))
        .unwrap_err();

        assert_eq!(
            error,
            ApplicationError::InvalidInput(JobValidationError::EmptyResultReference),
        );
        assert!(store.created.lock().unwrap().is_empty());
    }

    #[test]
    fn get_job_is_scoped_to_the_authenticated_owner() {
        let id = JobId::new();
        let mut store = FakeJobStore::empty();
        store.jobs = Mutex::new(vec![persisted_job(id, PRIMARY_USER_ID, JobState::Queued)]);
        let application = JobApplication::new(Arc::new(store));

        let job = block_on(application.get_job(&hermes_actor(), id)).unwrap();

        assert_eq!(job.id(), id);
        assert_eq!(job.owner_id(), PRIMARY_USER_ID);
    }

    #[test]
    fn missing_job_maps_to_not_found() {
        let application = JobApplication::new(Arc::new(FakeJobStore::empty()));

        let error = block_on(application.get_job(&hermes_actor(), JobId::new())).unwrap_err();

        assert_eq!(error, ApplicationError::NotFound);
    }

    #[test]
    fn queue_status_is_available_only_to_user_actors() {
        let store = FakeJobStore {
            status: QueueStatus {
                queued: 7,
                active: true,
            },
            ..FakeJobStore::empty()
        };
        let application = JobApplication::new(Arc::new(store));

        assert_eq!(
            block_on(application.queue_status(&hermes_actor())).unwrap(),
            QueueStatus {
                queued: 7,
                active: true,
            },
        );
        assert_eq!(
            block_on(application.queue_status(&runner_actor())).unwrap_err(),
            ApplicationError::Forbidden,
        );
    }

    #[test]
    fn job_port_errors_map_to_application_errors() {
        let conflict = JobApplication::new(Arc::new(FakeJobStore {
            failure: Some(PortError::Conflict),
            ..FakeJobStore::empty()
        }));
        let infrastructure = JobApplication::new(Arc::new(FakeJobStore {
            failure: Some(PortError::Infrastructure),
            ..FakeJobStore::empty()
        }));
        let command = || NewJobCommand {
            provider: Provider::Rezka,
            result_ref: "selection".to_owned(),
            notify_scope: NotifyScope::Initiator,
        };

        assert_eq!(
            block_on(conflict.create_job(&hermes_actor(), operation_key(4), command()))
                .unwrap_err(),
            ApplicationError::Conflict,
        );
        assert_eq!(
            block_on(infrastructure.create_job(&hermes_actor(), operation_key(5), command()))
                .unwrap_err(),
            ApplicationError::Infrastructure,
        );
    }

    #[test]
    fn lease_next_uses_the_runner_identity_and_server_ttl() {
        let ttl = time::Duration::seconds(60);
        let store = Arc::new(FakeLeaseStore {
            calls: Mutex::new(Vec::new()),
            lease: None,
            failure: None,
        });
        let application = LeaseApplication::new(store.clone(), ttl).unwrap();

        assert!(
            block_on(application.lease_next(&runner_actor(), operation_key(6)))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            *store.calls.lock().unwrap(),
            vec![LeaseCall::Next {
                operation: operation_key(6),
                runner: RUNNER_CLIENT_ID,
                ttl,
            }],
        );
    }

    #[test]
    fn lease_ttl_accepts_inclusive_boundaries() {
        let store = || {
            Arc::new(FakeLeaseStore {
                calls: Mutex::new(Vec::new()),
                lease: None,
                failure: None,
            })
        };

        assert!(LeaseApplication::new(store(), time::Duration::seconds(30)).is_ok());
        assert!(LeaseApplication::new(store(), time::Duration::seconds(300)).is_ok());
    }

    #[test]
    fn lease_ttl_rejects_values_outside_server_boundaries() {
        let store = || {
            Arc::new(FakeLeaseStore {
                calls: Mutex::new(Vec::new()),
                lease: None,
                failure: None,
            })
        };

        assert_eq!(
            LeaseApplication::new(store(), time::Duration::seconds(29)).err(),
            Some(LeaseTtlError::TooShort {
                actual: time::Duration::seconds(29),
            }),
        );
        assert_eq!(
            LeaseApplication::new(store(), time::Duration::seconds(301)).err(),
            Some(LeaseTtlError::TooLong {
                actual: time::Duration::seconds(301),
            }),
        );
    }

    #[test]
    fn hermes_actor_cannot_lease_jobs() {
        let store = Arc::new(FakeLeaseStore {
            calls: Mutex::new(Vec::new()),
            lease: None,
            failure: None,
        });
        let application =
            LeaseApplication::new(store.clone(), time::Duration::seconds(60)).unwrap();

        let error =
            block_on(application.lease_next(&hermes_actor(), operation_key(7))).unwrap_err();

        assert_eq!(error, ApplicationError::Forbidden);
        assert!(store.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn heartbeat_uses_the_runner_identity_and_server_ttl() {
        let lease_id = LeaseId::new();
        let ttl = time::Duration::seconds(90);
        let lease = JobLease::new(
            lease_id,
            persisted_job(JobId::new(), PRIMARY_USER_ID, JobState::Leased),
            RUNNER_CLIENT_ID,
            time::OffsetDateTime::UNIX_EPOCH + ttl,
        );
        let store = Arc::new(FakeLeaseStore {
            calls: Mutex::new(Vec::new()),
            lease: Some(lease.clone()),
            failure: None,
        });
        let application = LeaseApplication::new(store.clone(), ttl).unwrap();

        assert_eq!(
            block_on(application.heartbeat(&runner_actor(), operation_key(8), lease_id)).unwrap(),
            lease,
        );
        assert_eq!(
            *store.calls.lock().unwrap(),
            vec![LeaseCall::Heartbeat {
                operation: operation_key(8),
                lease: lease_id,
                runner: RUNNER_CLIENT_ID,
                ttl,
            }],
        );
    }

    #[test]
    fn missing_heartbeat_lease_maps_to_not_found() {
        let application = LeaseApplication::new(
            Arc::new(FakeLeaseStore {
                calls: Mutex::new(Vec::new()),
                lease: None,
                failure: None,
            }),
            time::Duration::seconds(60),
        )
        .unwrap();

        let error =
            block_on(application.heartbeat(&runner_actor(), operation_key(9), LeaseId::new()))
                .unwrap_err();

        assert_eq!(error, ApplicationError::NotFound);
    }
}
