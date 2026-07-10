use axum::{
    Json, Router,
    extract::{Extension, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use media_contract::CreateJobRequest;
use media_core::{Actor, ApplicationError, JobId};

use crate::{ApiError, ApiState, RequestId, convert, idempotency};

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/jobs", post(create))
        .route("/v1/jobs/{job_id}", get(get_job))
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
        |state, actor, request_id, body| async move {
            let request = match serde_json::from_slice::<CreateJobRequest>(&body) {
                Ok(request) => request,
                Err(_) => {
                    return ApiError::invalid_request(&request_id, "request JSON is invalid")
                        .into_response();
                }
            };
            match state
                .jobs()
                .create_job(&actor, convert::new_job_command(request))
                .await
            {
                Ok(job) => (StatusCode::CREATED, Json(convert::job(&job))).into_response(),
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
}

async fn get_job(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Path(job_id): Path<String>,
) -> Response {
    let Ok(job_id) = job_id.parse::<JobId>() else {
        return ApiError::invalid_request(&request_id, "job ID is invalid").into_response();
    };
    match state.jobs().get_job(&actor, job_id).await {
        Ok(job) => Json(convert::job(&job)).into_response(),
        Err(error) => application_error(error, &request_id),
    }
}

pub(super) fn application_error(error: ApplicationError, request_id: &RequestId) -> Response {
    match error {
        ApplicationError::Forbidden => {
            ApiError::forbidden(request_id, "operation is forbidden").into_response()
        }
        ApplicationError::InvalidInput(_) => {
            ApiError::invalid_request(request_id, "job request is invalid").into_response()
        }
        ApplicationError::NotFound => ApiError::not_found(request_id).into_response(),
        ApplicationError::Conflict => ApiError::conflict(request_id).into_response(),
        ApplicationError::Infrastructure => ApiError::internal(request_id).into_response(),
    }
}
