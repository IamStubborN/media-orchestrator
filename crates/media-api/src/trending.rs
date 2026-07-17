use media_contract::{TrendingCategoryDto, TrendingPageDto};

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum TrendingServiceError {
    #[error("trending request is invalid")]
    InvalidRequest,
    #[error("trending integration is unavailable")]
    Unavailable,
    #[error("trending provider failed")]
    Provider,
}

#[async_trait::async_trait]
pub trait TrendingService: Send + Sync {
    async fn trending(
        &self,
        category: TrendingCategoryDto,
        page: u32,
    ) -> Result<TrendingPageDto, TrendingServiceError>;
}

pub(crate) struct UnavailableTrendingService;

#[async_trait::async_trait]
impl TrendingService for UnavailableTrendingService {
    async fn trending(
        &self,
        _: TrendingCategoryDto,
        _: u32,
    ) -> Result<TrendingPageDto, TrendingServiceError> {
        Err(TrendingServiceError::Unavailable)
    }
}
