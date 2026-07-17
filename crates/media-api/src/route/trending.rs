use axum::{
    Json, Router,
    extract::{Extension, Query, State},
    response::{IntoResponse, Response},
    routing::get,
};
use media_contract::TrendingCategoryDto;
use serde::Deserialize;

use crate::{ApiError, ApiState, RequestId, TrendingServiceError};

pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/v1/trending", get(trending))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrendingQuery {
    #[serde(default = "default_category")]
    category: TrendingCategoryDto,
    #[serde(default = "default_page")]
    page: u32,
}

const fn default_category() -> TrendingCategoryDto {
    TrendingCategoryDto::All
}

const fn default_page() -> u32 {
    1
}

async fn trending(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    Query(query): Query<TrendingQuery>,
) -> Response {
    if query.page == 0 {
        return ApiError::invalid_request(&request_id, "trending page must be positive")
            .into_response();
    }
    match state.trending().trending(query.category, query.page).await {
        Ok(page) => Json(page).into_response(),
        Err(TrendingServiceError::InvalidRequest) => {
            ApiError::invalid_request(&request_id, "trending request is invalid").into_response()
        }
        Err(TrendingServiceError::Unavailable) => {
            ApiError::integration_unavailable(&request_id).into_response()
        }
        Err(TrendingServiceError::Provider) => {
            ApiError::upstream_failure(&request_id).into_response()
        }
    }
}
