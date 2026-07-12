use media_contract::{PlexReconcileRequest, PlexReconcileResponse};

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum PlexServiceError {
    #[error("Plex reconciliation request is invalid")]
    InvalidRequest,
    #[error("Plex reconciliation failed")]
    Infrastructure,
}

#[async_trait::async_trait]
pub trait PlexReconcileService: Send + Sync {
    async fn reconcile(
        &self,
        request: PlexReconcileRequest,
    ) -> Result<PlexReconcileResponse, PlexServiceError>;
}

pub(crate) struct UnavailablePlexService;

#[async_trait::async_trait]
impl PlexReconcileService for UnavailablePlexService {
    async fn reconcile(
        &self,
        _: PlexReconcileRequest,
    ) -> Result<PlexReconcileResponse, PlexServiceError> {
        Err(PlexServiceError::Infrastructure)
    }
}
