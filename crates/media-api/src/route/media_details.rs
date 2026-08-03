use axum::{
    Json, Router,
    extract::{Extension, Query, State},
    response::{IntoResponse, Response},
    routing::get,
};
use media_contract::TrendingMediaTypeDto;
use serde::Deserialize;

use crate::{ApiError, ApiState, MediaDetailsServiceError, RequestId};

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/media/details", get(details))
        .route("/v1/media/similar", get(similar))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetailsQuery {
    tmdb_id: u64,
    media_type: TrendingMediaTypeDto,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SimilarQuery {
    tmdb_id: u64,
    media_type: TrendingMediaTypeDto,
    #[serde(default = "default_page")]
    page: u32,
}

const fn default_page() -> u32 {
    1
}

async fn details(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    Query(query): Query<DetailsQuery>,
) -> Response {
    if query.tmdb_id == 0 {
        return ApiError::invalid_request(&request_id, "tmdb ID must be positive").into_response();
    }
    match state
        .media_details()
        .details(query.tmdb_id, query.media_type)
        .await
    {
        Ok(details) => Json(details).into_response(),
        Err(error) => media_details_error(error, &request_id),
    }
}

async fn similar(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    Query(query): Query<SimilarQuery>,
) -> Response {
    if query.tmdb_id == 0 {
        return ApiError::invalid_request(&request_id, "tmdb ID must be positive").into_response();
    }
    if query.page == 0 {
        return ApiError::invalid_request(&request_id, "similar page must be positive")
            .into_response();
    }
    match state
        .media_details()
        .similar(query.tmdb_id, query.media_type, query.page)
        .await
    {
        Ok(page) => Json(page).into_response(),
        Err(error) => media_details_error(error, &request_id),
    }
}

fn media_details_error(error: MediaDetailsServiceError, request_id: &RequestId) -> Response {
    match error {
        MediaDetailsServiceError::InvalidRequest => {
            ApiError::invalid_request(request_id, "media details request is invalid")
                .into_response()
        }
        MediaDetailsServiceError::Unavailable => {
            ApiError::integration_unavailable(request_id).into_response()
        }
        MediaDetailsServiceError::Provider => {
            ApiError::upstream_failure(request_id).into_response()
        }
    }
}
