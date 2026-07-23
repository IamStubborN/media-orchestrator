use media_contract::{
    AlternativeSearchRequest, ContinueSearchRequest, EpisodeMappingActionDto,
    ExecutionSelectionDto, JobDto, ResolveEpisodeMappingRequest, RezkaSessionRefreshRequest,
    SearchPageDto, SelectResultRequest, StartSearchRequest,
};
use media_core::{JobId, OperationKey, UserId};

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum SearchError {
    #[error("search request is invalid")]
    InvalidRequest,
    #[error("search operation is forbidden")]
    Forbidden,
    #[error("search resource was not found")]
    NotFound,
    #[error("search operation conflicts with current state")]
    Conflict,
    #[error("media provider failed")]
    Provider,
    #[error("search infrastructure failed")]
    Infrastructure,
}

#[async_trait::async_trait]
pub trait SearchService: Send + Sync {
    async fn refresh_rezka_session(
        &self,
        owner: UserId,
        operation: OperationKey,
        request: RezkaSessionRefreshRequest,
    ) -> Result<JobDto, SearchError>;

    async fn start(
        &self,
        owner: UserId,
        request: StartSearchRequest,
    ) -> Result<SearchPageDto, SearchError>;

    async fn continue_search(
        &self,
        owner: UserId,
        request: ContinueSearchRequest,
    ) -> Result<SearchPageDto, SearchError>;

    async fn start_alternative(
        &self,
        _owner: UserId,
        _job_id: JobId,
        _request: AlternativeSearchRequest,
    ) -> Result<SearchPageDto, SearchError> {
        Err(SearchError::NotFound)
    }

    async fn select(
        &self,
        owner: UserId,
        operation: OperationKey,
        request: SelectResultRequest,
    ) -> Result<JobDto, SearchError>;

    async fn execution_for(&self, result_ref: &str) -> Result<ExecutionSelectionDto, SearchError>;

    async fn episode_mapping_action(
        &self,
        owner: UserId,
        job_id: JobId,
    ) -> Result<EpisodeMappingActionDto, SearchError>;

    async fn resolve_episode_mapping(
        &self,
        owner: UserId,
        operation: OperationKey,
        job_id: JobId,
        request: ResolveEpisodeMappingRequest,
    ) -> Result<JobDto, SearchError>;
}

pub(crate) struct UnavailableSearchService;

#[async_trait::async_trait]
impl SearchService for UnavailableSearchService {
    async fn refresh_rezka_session(
        &self,
        _: UserId,
        _: OperationKey,
        _: RezkaSessionRefreshRequest,
    ) -> Result<JobDto, SearchError> {
        Err(SearchError::Infrastructure)
    }

    async fn start(&self, _: UserId, _: StartSearchRequest) -> Result<SearchPageDto, SearchError> {
        Err(SearchError::Infrastructure)
    }

    async fn continue_search(
        &self,
        _: UserId,
        _: ContinueSearchRequest,
    ) -> Result<SearchPageDto, SearchError> {
        Err(SearchError::Infrastructure)
    }

    async fn select(
        &self,
        _: UserId,
        _: OperationKey,
        _: SelectResultRequest,
    ) -> Result<JobDto, SearchError> {
        Err(SearchError::Infrastructure)
    }

    async fn execution_for(&self, _: &str) -> Result<ExecutionSelectionDto, SearchError> {
        Err(SearchError::NotFound)
    }

    async fn episode_mapping_action(
        &self,
        _: UserId,
        _: JobId,
    ) -> Result<EpisodeMappingActionDto, SearchError> {
        Err(SearchError::NotFound)
    }

    async fn resolve_episode_mapping(
        &self,
        _: UserId,
        _: OperationKey,
        _: JobId,
        _: ResolveEpisodeMappingRequest,
    ) -> Result<JobDto, SearchError> {
        Err(SearchError::NotFound)
    }
}
