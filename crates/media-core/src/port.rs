use crate::{
    Actor, BootstrapClient, ClientId, CredentialDigest, Job, JobId, JobLease, LeaseId, NewJob,
    QueueStatus, UserId,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum PortError {
    #[error("persistence conflict")]
    Conflict,
    #[error("infrastructure operation failed")]
    Infrastructure,
}

#[async_trait::async_trait]
pub trait ClientStore: Send + Sync {
    async fn find_by_digest(&self, digest: CredentialDigest) -> Result<Option<Actor>, PortError>;

    async fn upsert_client(&self, client: BootstrapClient) -> Result<(), PortError>;
}

#[async_trait::async_trait]
pub trait JobStore: Send + Sync {
    async fn create(&self, job: NewJob) -> Result<Job, PortError>;

    async fn find_for_owner(&self, id: JobId, owner: UserId) -> Result<Option<Job>, PortError>;

    async fn queue_status(&self) -> Result<QueueStatus, PortError>;
}

#[async_trait::async_trait]
pub trait LeaseStore: Send + Sync {
    async fn lease_next(
        &self,
        runner: ClientId,
        ttl: time::Duration,
    ) -> Result<Option<JobLease>, PortError>;

    async fn heartbeat(
        &self,
        lease: LeaseId,
        runner: ClientId,
        ttl: time::Duration,
    ) -> Result<Option<JobLease>, PortError>;
}

#[async_trait::async_trait]
pub trait ReadinessPort: Send + Sync {
    async fn is_ready(&self) -> Result<bool, PortError>;
}

#[cfg(test)]
mod tests {
    use super::{ClientStore, JobStore, LeaseStore, ReadinessPort};

    #[test]
    fn persistence_ports_are_object_safe() {
        fn accept_client_store(_: Option<&dyn ClientStore>) {}
        fn accept_job_store(_: Option<&dyn JobStore>) {}
        fn accept_lease_store(_: Option<&dyn LeaseStore>) {}
        fn accept_readiness_port(_: Option<&dyn ReadinessPort>) {}

        accept_client_store(None);
        accept_job_store(None);
        accept_lease_store(None);
        accept_readiness_port(None);
    }
}
