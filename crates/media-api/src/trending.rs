use media_contract::{
    BestPageDto, BestRankingDto, DiscoverPageDto, GenreListDto, PremiereFeedDto, PremieresPageDto,
    TrendingCategoryDto, TrendingMediaTypeDto, TrendingPageDto,
};

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

    async fn best(
        &self,
        _: TrendingMediaTypeDto,
        _: BestRankingDto,
        _: u32,
    ) -> Result<BestPageDto, TrendingServiceError> {
        Err(TrendingServiceError::Unavailable)
    }

    async fn premieres(
        &self,
        _: TrendingMediaTypeDto,
        _: PremiereFeedDto,
        _: u32,
    ) -> Result<PremieresPageDto, TrendingServiceError> {
        Err(TrendingServiceError::Unavailable)
    }

    async fn genres(&self, _: TrendingMediaTypeDto) -> Result<GenreListDto, TrendingServiceError> {
        Err(TrendingServiceError::Unavailable)
    }

    async fn discover(
        &self,
        _: TrendingMediaTypeDto,
        _: u64,
        _: u32,
    ) -> Result<DiscoverPageDto, TrendingServiceError> {
        Err(TrendingServiceError::Unavailable)
    }
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
