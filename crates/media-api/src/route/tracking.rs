use axum::{
    Json, Router,
    extract::{Extension, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use media_contract::{CreateTrackingRequest, PatchTrackingRequest, TrackingListDto};
use media_core::{Actor, TrackingApplicationError, TrackingId};

use crate::{ApiError, ApiState, RequestId, convert, idempotency};

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/tracking", get(list).post(create))
        .route("/v1/tracking/{tracking_id}", delete(remove).patch(patch))
}

async fn patch(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Path(tracking_id): Path<String>,
    request: Request,
) -> Response {
    idempotency::execute(
        state,
        actor,
        request_id,
        request,
        move |state, actor, request_id, _operation, body| async move {
            let Some(tracking) = state.tracking() else {
                return ApiError::internal(&request_id).into_response();
            };
            let Ok(id) = tracking_id.parse::<TrackingId>() else {
                return ApiError::invalid_request(&request_id, "tracking ID is invalid")
                    .into_response();
            };
            let request = match serde_json::from_slice::<PatchTrackingRequest>(&body) {
                Ok(request) => request,
                Err(_) => {
                    return ApiError::invalid_request(&request_id, "request JSON is invalid")
                        .into_response();
                }
            };
            let patch = match convert::tracking_download_patch(request) {
                Ok(patch) => patch,
                Err(()) => {
                    return ApiError::invalid_request(&request_id, "tracking request is invalid")
                        .into_response();
                }
            };
            match tracking.patch_download(&actor, id, patch).await {
                Ok(value) => Json(convert::tracking(&value)).into_response(),
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
}

async fn list(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let Some(tracking) = state.tracking() else {
        return ApiError::internal(&request_id).into_response();
    };
    match tracking.list(&actor).await {
        Ok(values) => Json(TrackingListDto {
            tracking: values.iter().map(convert::tracking).collect(),
        })
        .into_response(),
        Err(error) => application_error(error, &request_id),
    }
}

async fn create(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    request: Request,
) -> Response {
    idempotency::execute(
        state,
        actor,
        request_id,
        request,
        |state, actor, request_id, operation, body| async move {
            let Some(tracking) = state.tracking() else {
                return ApiError::internal(&request_id).into_response();
            };
            let request = match serde_json::from_slice::<CreateTrackingRequest>(&body) {
                Ok(request) => request,
                Err(_) => {
                    return ApiError::invalid_request(&request_id, "request JSON is invalid")
                        .into_response();
                }
            };
            let command = match convert::new_tracking_command(request) {
                Ok(command) => command,
                Err(()) => {
                    return ApiError::invalid_request(&request_id, "tracking request is invalid")
                        .into_response();
                }
            };
            match tracking.add(&actor, operation, command).await {
                Ok(value) => (StatusCode::CREATED, Json(convert::tracking(&value))).into_response(),
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
}

async fn remove(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Path(tracking_id): Path<String>,
    request: Request,
) -> Response {
    idempotency::execute(
        state,
        actor,
        request_id,
        request,
        move |state, actor, request_id, operation, body| async move {
            if !body.is_empty() {
                return ApiError::invalid_request(&request_id, "request body must be empty")
                    .into_response();
            }
            let Some(tracking) = state.tracking() else {
                return ApiError::internal(&request_id).into_response();
            };
            let Ok(id) = tracking_id.parse::<TrackingId>() else {
                return ApiError::invalid_request(&request_id, "tracking ID is invalid")
                    .into_response();
            };
            match tracking.remove(&actor, operation, id).await {
                Ok(value) => {
                    let mut value = convert::tracking(&value);
                    value.state = media_contract::TrackingStateDto::Removed;
                    Json(value).into_response()
                }
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
}

fn application_error(error: TrackingApplicationError, request_id: &RequestId) -> Response {
    match error {
        TrackingApplicationError::Forbidden => {
            ApiError::forbidden(request_id, "operation is forbidden").into_response()
        }
        TrackingApplicationError::InvalidInput(_) => {
            ApiError::invalid_request(request_id, "tracking request is invalid").into_response()
        }
        TrackingApplicationError::NotFound => ApiError::not_found(request_id).into_response(),
        TrackingApplicationError::Conflict => ApiError::conflict(request_id).into_response(),
        TrackingApplicationError::Infrastructure => ApiError::internal(request_id).into_response(),
    }
}
