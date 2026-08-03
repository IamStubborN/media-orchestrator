use media_contract::{MediaDetailsDto, SimilarPageDto, TrendingMediaTypeDto};

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum MediaDetailsServiceError {
    #[error("media details request is invalid")]
    InvalidRequest,
    #[error("media details integration is unavailable")]
    Unavailable,
    #[error("media details provider failed")]
    Provider,
}

#[async_trait::async_trait]
pub trait MediaDetailsService: Send + Sync {
    async fn details(
        &self,
        tmdb_id: u64,
        media_type: TrendingMediaTypeDto,
    ) -> Result<MediaDetailsDto, MediaDetailsServiceError>;

    async fn similar(
        &self,
        tmdb_id: u64,
        media_type: TrendingMediaTypeDto,
        page: u32,
    ) -> Result<SimilarPageDto, MediaDetailsServiceError>;
}

pub(crate) struct UnavailableMediaDetailsService;

#[async_trait::async_trait]
impl MediaDetailsService for UnavailableMediaDetailsService {
    async fn details(
        &self,
        _: u64,
        _: TrendingMediaTypeDto,
    ) -> Result<MediaDetailsDto, MediaDetailsServiceError> {
        Err(MediaDetailsServiceError::Unavailable)
    }

    async fn similar(
        &self,
        _: u64,
        _: TrendingMediaTypeDto,
        _: u32,
    ) -> Result<SimilarPageDto, MediaDetailsServiceError> {
        Err(MediaDetailsServiceError::Unavailable)
    }
}
