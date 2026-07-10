use std::sync::Arc;

use crate::{
    Actor, Job, JobId, JobLease, JobStore, JobValidationError, LeaseId, LeaseStore, NewJob,
    NotifyScope, PortError, Provider, QueueStatus,
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NewJobCommand {
    pub provider: Provider,
    pub result_ref: String,
    pub notify_scope: NotifyScope,
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
        command: NewJobCommand,
    ) -> Result<Job, ApplicationError> {
        let owner_id = actor
            .require_user()
            .map_err(|_| ApplicationError::Forbidden)?;
        let job = NewJob::new(
            JobId::new(),
            owner_id,
            command.provider,
            command.result_ref,
            command.notify_scope,
        )
        .map_err(ApplicationError::InvalidInput)?;

        self.store.create(job).await.map_err(Into::into)
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

impl LeaseApplication {
    #[must_use]
    pub fn new(store: Arc<dyn LeaseStore>, ttl: time::Duration) -> Self {
        Self { store, ttl }
    }

    pub async fn lease_next(&self, actor: &Actor) -> Result<Option<JobLease>, ApplicationError> {
        let runner = actor
            .require_runner()
            .map_err(|_| ApplicationError::Forbidden)?;

        self.store
            .lease_next(runner, self.ttl)
            .await
            .map_err(Into::into)
    }

    pub async fn heartbeat(
        &self,
        actor: &Actor,
        lease: LeaseId,
    ) -> Result<JobLease, ApplicationError> {
        let runner = actor
            .require_runner()
            .map_err(|_| ApplicationError::Forbidden)?;

        self.store
            .heartbeat(lease, runner, self.ttl)
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

    use super::{ApplicationError, JobApplication, LeaseApplication, NewJobCommand};
    use crate::{
        PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientId, ClientRole, Job, JobId, JobLease,
        JobState, JobStore, JobValidationError, LeaseId, LeaseStore, NewJob, NotifyScope,
        PortError, Provider, QueueStatus, RUNNER_CLIENT_ID, UserId,
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

    fn persisted_job(id: JobId, owner_id: UserId) -> Job {
        Job {
            id,
            owner_id,
            provider: Provider::Rezka,
            result_ref: "rezka-selection".to_owned(),
            state: JobState::Queued,
            needs_action_reason: None,
            notify_scope: NotifyScope::Initiator,
        }
    }

    struct FakeJobStore {
        created: Mutex<Vec<NewJob>>,
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
        async fn create(&self, job: NewJob) -> Result<Job, PortError> {
            if let Some(error) = self.failure {
                return Err(error);
            }

            self.created.lock().unwrap().push(job.clone());
            Ok(Job {
                id: job.id,
                owner_id: job.owner_id,
                provider: job.provider,
                result_ref: job.result_ref,
                state: JobState::Queued,
                needs_action_reason: None,
                notify_scope: job.notify_scope,
            })
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
                .find(|job| job.id == id && job.owner_id == owner)
                .cloned())
        }

        async fn queue_status(&self) -> Result<QueueStatus, PortError> {
            self.failure.map_or(Ok(self.status), Err)
        }
    }

    #[derive(Debug, Copy, Clone, Eq, PartialEq)]
    enum LeaseCall {
        Next {
            runner: ClientId,
            ttl: time::Duration,
        },
        Heartbeat {
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
            runner: ClientId,
            ttl: time::Duration,
        ) -> Result<Option<JobLease>, PortError> {
            if let Some(error) = self.failure {
                return Err(error);
            }
            self.calls
                .lock()
                .unwrap()
                .push(LeaseCall::Next { runner, ttl });
            Ok(self.lease.clone())
        }

        async fn heartbeat(
            &self,
            lease: LeaseId,
            runner: ClientId,
            ttl: time::Duration,
        ) -> Result<Option<JobLease>, PortError> {
            if let Some(error) = self.failure {
                return Err(error);
            }
            self.calls
                .lock()
                .unwrap()
                .push(LeaseCall::Heartbeat { lease, runner, ttl });
            Ok(self.lease.clone())
        }
    }

    #[test]
    fn create_job_uses_the_authenticated_users_identity() {
        let store = Arc::new(FakeJobStore::empty());
        let application = JobApplication::new(store.clone());

        let job = block_on(application.create_job(
            &hermes_actor(),
            NewJobCommand {
                provider: Provider::Rezka,
                result_ref: "selection-1".to_owned(),
                notify_scope: NotifyScope::Family,
            },
        ))
        .unwrap();

        assert_eq!(job.owner_id, PRIMARY_USER_ID);
        let created = store.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].owner_id, PRIMARY_USER_ID);
    }

    #[test]
    fn runner_actor_cannot_create_or_read_user_jobs() {
        let store = Arc::new(FakeJobStore::empty());
        let application = JobApplication::new(store.clone());

        let create_error = block_on(application.create_job(
            &runner_actor(),
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
        store.jobs = Mutex::new(vec![persisted_job(id, PRIMARY_USER_ID)]);
        let application = JobApplication::new(Arc::new(store));

        let job = block_on(application.get_job(&hermes_actor(), id)).unwrap();

        assert_eq!(job.id, id);
        assert_eq!(job.owner_id, PRIMARY_USER_ID);
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
            block_on(conflict.create_job(&hermes_actor(), command())).unwrap_err(),
            ApplicationError::Conflict,
        );
        assert_eq!(
            block_on(infrastructure.create_job(&hermes_actor(), command())).unwrap_err(),
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
        let application = LeaseApplication::new(store.clone(), ttl);

        assert!(
            block_on(application.lease_next(&runner_actor()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            *store.calls.lock().unwrap(),
            vec![LeaseCall::Next {
                runner: RUNNER_CLIENT_ID,
                ttl,
            }],
        );
    }

    #[test]
    fn hermes_actor_cannot_lease_jobs() {
        let store = Arc::new(FakeLeaseStore {
            calls: Mutex::new(Vec::new()),
            lease: None,
            failure: None,
        });
        let application = LeaseApplication::new(store.clone(), time::Duration::seconds(60));

        let error = block_on(application.lease_next(&hermes_actor())).unwrap_err();

        assert_eq!(error, ApplicationError::Forbidden);
        assert!(store.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn heartbeat_uses_the_runner_identity_and_server_ttl() {
        let lease_id = LeaseId::new();
        let ttl = time::Duration::seconds(90);
        let lease = JobLease {
            lease_id,
            job: persisted_job(JobId::new(), PRIMARY_USER_ID),
            runner_client_id: RUNNER_CLIENT_ID,
            expires_at: time::OffsetDateTime::UNIX_EPOCH + ttl,
        };
        let store = Arc::new(FakeLeaseStore {
            calls: Mutex::new(Vec::new()),
            lease: Some(lease.clone()),
            failure: None,
        });
        let application = LeaseApplication::new(store.clone(), ttl);

        assert_eq!(
            block_on(application.heartbeat(&runner_actor(), lease_id)).unwrap(),
            lease,
        );
        assert_eq!(
            *store.calls.lock().unwrap(),
            vec![LeaseCall::Heartbeat {
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
        );

        let error = block_on(application.heartbeat(&runner_actor(), LeaseId::new())).unwrap_err();

        assert_eq!(error, ApplicationError::NotFound);
    }
}
