use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Extension, Request, State},
    response::{IntoResponse, Response},
    routing::post,
};
use media_contract::ReleaseQueryRequest;
use media_core::{ReleaseQuery, ReleaseQueryError};

use crate::{ApiError, ApiState, MAX_REQUEST_BODY_BYTES, RequestId, convert};

pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/v1/releases/query", post(query))
}

async fn query(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    request: Request,
) -> Response {
    let Some(service) = state.release_metadata() else {
        return ApiError::internal(&request_id).into_response();
    };
    let body = match to_bytes(request.into_body(), MAX_REQUEST_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return ApiError::invalid_body(&request_id).into_response(),
    };
    let request = match serde_json::from_slice::<ReleaseQueryRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return ApiError::invalid_request(&request_id, "release query JSON is invalid")
                .into_response();
        }
    };
    let query = match ReleaseQuery::new(request.title, request.original_title, request.year) {
        Ok(query) => query,
        Err(_) => {
            return ApiError::invalid_request(&request_id, "release query is invalid")
                .into_response();
        }
    };
    match service.query(query).await {
        Ok(result) => Json(convert::release_result(result)).into_response(),
        Err(ReleaseQueryError::EmptyTitle | ReleaseQueryError::EmptyOriginalTitle) => {
            ApiError::invalid_request(&request_id, "release query is invalid").into_response()
        }
        Err(ReleaseQueryError::Provider) => ApiError::internal(&request_id).into_response(),
    }
}
