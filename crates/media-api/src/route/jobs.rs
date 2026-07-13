use axum::{
    Json, Router,
    extract::{Extension, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use media_contract::{CreateJobRequest, JobListDto};
use media_core::{Actor, ApplicationError, JobId};

use crate::{ApiError, ApiState, RequestId, convert, idempotency};

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/jobs", get(list).post(create))
        .route("/v1/jobs/{job_id}", get(get_job))
        .route("/v1/jobs/{job_id}/cancel", post(cancel))
        .route("/v1/jobs/{job_id}/retry", post(retry))
}

async fn list(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.jobs().list_jobs(&actor).await {
        Ok(jobs) => Json(JobListDto {
            jobs: jobs.iter().map(convert::job).collect(),
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
            let request = match serde_json::from_slice::<CreateJobRequest>(&body) {
                Ok(request) => request,
                Err(_) => {
                    return ApiError::invalid_request(&request_id, "request JSON is invalid")
                        .into_response();
                }
            };
            match state
                .jobs()
                .create_job(&actor, operation, convert::new_job_command(request))
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
    match state.jobs().get_job_detail(&actor, job_id).await {
        Ok(detail) => Json(convert::job_detail(&detail)).into_response(),
        Err(error) => application_error(error, &request_id),
    }
}

async fn cancel(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Path(job_id): Path<String>,
    request: Request,
) -> Response {
    idempotency::execute(
        state,
        actor,
        request_id,
        request,
        move |state, actor, request_id, operation, body| async move {
            let valid_body = body.is_empty()
                || serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&body)
                    .is_ok_and(|body| body.is_empty());
            if !valid_body {
                return ApiError::invalid_request(&request_id, "request JSON is invalid")
                    .into_response();
            }
            let Ok(job_id) = job_id.parse::<JobId>() else {
                return ApiError::invalid_request(&request_id, "job ID is invalid").into_response();
            };
            match state.jobs().cancel_job(&actor, operation, job_id).await {
                Ok(job) => Json(convert::job(&job)).into_response(),
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
}

async fn retry(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Path(job_id): Path<String>,
    request: Request,
) -> Response {
    idempotency::execute(
        state,
        actor,
        request_id,
        request,
        move |state, actor, request_id, operation, body| async move {
            let valid_body = body.is_empty()
                || serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&body)
                    .is_ok_and(|body| body.is_empty());
            if !valid_body {
                return ApiError::invalid_request(&request_id, "request JSON is invalid")
                    .into_response();
            }
            let Ok(job_id) = job_id.parse::<JobId>() else {
                return ApiError::invalid_request(&request_id, "job ID is invalid").into_response();
            };
            match state.jobs().retry_job(&actor, operation, job_id).await {
                Ok(job) => Json(convert::job(&job)).into_response(),
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
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
