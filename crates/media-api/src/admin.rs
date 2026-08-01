use media_core::Actor;
use serde_json::Value;

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum MediaAdminError {
    #[error("media administration is not configured")]
    Unavailable,
    #[error("media administration request is invalid")]
    InvalidRequest,
    #[error("media administration operation is forbidden")]
    Forbidden,
    #[error("media administration resource was not found")]
    NotFound,
    #[error("media administration confirmation is invalid or expired")]
    InvalidConfirmation,
    #[error("media administration provider failed")]
    Provider,
}

#[async_trait::async_trait]
pub trait MediaAdminService: Send + Sync {
    async fn plex_search(
        &self,
        actor: &Actor,
        query: &str,
        limit: u16,
    ) -> Result<Value, MediaAdminError>;
    async fn plex_recent(&self, actor: &Actor, limit: u16) -> Result<Value, MediaAdminError>;
    async fn plex_now_playing(&self, actor: &Actor) -> Result<Value, MediaAdminError>;
    async fn plex_item(&self, actor: &Actor, rating_key: u64) -> Result<Value, MediaAdminError>;
    async fn plex_refresh(&self, actor: &Actor, section_key: u32)
    -> Result<Value, MediaAdminError>;
    async fn qbittorrent_list(
        &self,
        actor: &Actor,
        filter: Option<&str>,
    ) -> Result<Value, MediaAdminError>;
    async fn qbittorrent_details(
        &self,
        actor: &Actor,
        hash: &str,
    ) -> Result<Value, MediaAdminError>;
    async fn qbittorrent_control(
        &self,
        actor: &Actor,
        hash: &str,
        action: &str,
    ) -> Result<Value, MediaAdminError>;
    async fn file_inspect(&self, actor: &Actor, path: &str) -> Result<Value, MediaAdminError>;
    async fn infrastructure_status(&self, actor: &Actor) -> Result<Value, MediaAdminError>;
    async fn prepare_destructive(
        &self,
        actor: &Actor,
        action: &str,
        target: &str,
        delete_files: bool,
    ) -> Result<Value, MediaAdminError>;
    async fn confirm_destructive(
        &self,
        actor: &Actor,
        confirmation_token: &str,
    ) -> Result<Value, MediaAdminError>;
}

pub(crate) struct UnavailableMediaAdminService;

#[async_trait::async_trait]
impl MediaAdminService for UnavailableMediaAdminService {
    async fn plex_search(&self, _: &Actor, _: &str, _: u16) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn plex_recent(&self, _: &Actor, _: u16) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn plex_now_playing(&self, _: &Actor) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn plex_item(&self, _: &Actor, _: u64) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn plex_refresh(&self, _: &Actor, _: u32) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn qbittorrent_list(&self, _: &Actor, _: Option<&str>) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn qbittorrent_details(&self, _: &Actor, _: &str) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn qbittorrent_control(
        &self,
        _: &Actor,
        _: &str,
        _: &str,
    ) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn file_inspect(&self, _: &Actor, _: &str) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn infrastructure_status(&self, _: &Actor) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn prepare_destructive(
        &self,
        _: &Actor,
        _: &str,
        _: &str,
        _: bool,
    ) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
    async fn confirm_destructive(&self, _: &Actor, _: &str) -> Result<Value, MediaAdminError> {
        Err(MediaAdminError::Unavailable)
    }
}
