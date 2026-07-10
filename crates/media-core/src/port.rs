use crate::{
    Actor, BootstrapClient, CanonicalEpisode, CanonicalMedia, CanonicalSeason, ClientId,
    CredentialDigest, EpisodeProviderMapping, ExternalNamespace, Job, JobId, JobLease, LeaseId,
    MediaExternalReference, NewJob, QueueStatus, UserId,
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

#[async_trait::async_trait]
pub trait IdentityStore: Send + Sync {
    async fn create_media(&self, media: CanonicalMedia) -> Result<CanonicalMedia, PortError>;

    async fn add_external_reference(
        &self,
        reference: MediaExternalReference,
    ) -> Result<MediaExternalReference, PortError>;

    async fn find_media_by_external_reference(
        &self,
        namespace: ExternalNamespace,
        value: &str,
    ) -> Result<Option<CanonicalMedia>, PortError>;

    async fn create_season(&self, season: CanonicalSeason) -> Result<CanonicalSeason, PortError>;

    async fn create_episode(
        &self,
        episode: CanonicalEpisode,
    ) -> Result<CanonicalEpisode, PortError>;

    async fn save_episode_mapping(
        &self,
        mapping: EpisodeProviderMapping,
    ) -> Result<EpisodeProviderMapping, PortError>;
}

#[cfg(test)]
mod tests {
    use super::{ClientStore, IdentityStore, JobStore, LeaseStore, ReadinessPort};

    #[test]
    fn persistence_ports_are_object_safe() {
        fn accept_client_store(_: Option<&dyn ClientStore>) {}
        fn accept_job_store(_: Option<&dyn JobStore>) {}
        fn accept_lease_store(_: Option<&dyn LeaseStore>) {}
        fn accept_readiness_port(_: Option<&dyn ReadinessPort>) {}
        fn accept_identity_store(_: Option<&dyn IdentityStore>) {}

        accept_client_store(None);
        accept_job_store(None);
        accept_lease_store(None);
        accept_readiness_port(None);
        accept_identity_store(None);
    }
}
