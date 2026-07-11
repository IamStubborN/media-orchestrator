use axum::{
    Json, Router,
    extract::{Extension, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use media_contract::{RunnerEventRequest, RunnerEventResponse};
use media_core::{Actor, ApplicationError, LeaseId};
use serde::Deserialize;

use crate::{ApiError, ApiState, RequestId, convert, idempotency};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyRequest {}

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/runner/leases", post(lease_next))
        .route("/v1/runner/leases/{lease_id}/heartbeat", post(heartbeat))
        .route("/v1/runner/leases/{lease_id}/events", post(report_event))
}

async fn lease_next(
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
            if !valid_empty_body(&body) {
                return ApiError::invalid_request(&request_id, "request JSON is invalid")
                    .into_response();
            }
            match state.leases().lease_next(&actor, operation).await {
                Ok(Some(lease)) => match convert::lease(&lease) {
                    Ok(dto) => Json(dto).into_response(),
                    Err(_) => ApiError::internal(&request_id).into_response(),
                },
                Ok(None) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
}

async fn heartbeat(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Path(lease_id): Path<String>,
    request: Request,
) -> Response {
    idempotency::execute(
        state,
        actor,
        request_id,
        request,
        move |state, actor, request_id, operation, body| async move {
            if !valid_empty_body(&body) {
                return ApiError::invalid_request(&request_id, "request JSON is invalid")
                    .into_response();
            }
            let Ok(lease_id) = lease_id.parse::<LeaseId>() else {
                return ApiError::invalid_request(&request_id, "lease ID is invalid")
                    .into_response();
            };
            match state.leases().heartbeat(&actor, operation, lease_id).await {
                Ok(lease) => match convert::lease(&lease) {
                    Ok(dto) => Json(dto).into_response(),
                    Err(_) => ApiError::internal(&request_id).into_response(),
                },
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
}

async fn report_event(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Path(lease_id): Path<String>,
    request: Request,
) -> Response {
    idempotency::execute(
        state,
        actor,
        request_id,
        request,
        move |state, actor, request_id, operation, body| async move {
            let Ok(lease_id) = lease_id.parse::<LeaseId>() else {
                return ApiError::invalid_request(&request_id, "lease ID is invalid")
                    .into_response();
            };
            let event = serde_json::from_slice::<RunnerEventRequest>(&body)
                .ok()
                .and_then(|event| convert::runner_event(event).ok());
            let Some(event) = event else {
                return ApiError::invalid_request(&request_id, "runner event is invalid")
                    .into_response();
            };
            match state
                .leases()
                .report_event(&actor, operation, lease_id, event)
                .await
            {
                Ok(job) => Json(RunnerEventResponse {
                    job: convert::job(&job),
                })
                .into_response(),
                Err(error) => application_error(error, &request_id),
            }
        },
    )
    .await
}

fn valid_empty_body(body: &[u8]) -> bool {
    body.is_empty() || serde_json::from_slice::<EmptyRequest>(body).is_ok()
}

fn application_error(error: ApplicationError, request_id: &RequestId) -> Response {
    match error {
        ApplicationError::Forbidden => {
            ApiError::forbidden(request_id, "operation is forbidden").into_response()
        }
        ApplicationError::NotFound => ApiError::lease_not_found(request_id).into_response(),
        ApplicationError::Conflict => ApiError::conflict(request_id).into_response(),
        ApplicationError::InvalidInput(_) => {
            ApiError::invalid_request(request_id, "lease request is invalid").into_response()
        }
        ApplicationError::Infrastructure => ApiError::internal(request_id).into_response(),
    }
}
