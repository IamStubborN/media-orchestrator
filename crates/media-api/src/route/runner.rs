use axum::{
    Json, Router,
    extract::{Extension, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use media_contract::{PlexReconcileRequest, RunnerEventRequest, RunnerEventResponse};
use media_core::{Actor, ApplicationError, LeaseId};
use serde::Deserialize;

use crate::{ApiError, ApiState, PlexServiceError, RequestId, SearchError, convert, idempotency};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyRequest {}

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/runner/leases", post(lease_next))
        .route("/v1/runner/leases/{lease_id}/heartbeat", post(heartbeat))
        .route("/v1/runner/leases/{lease_id}/events", post(report_event))
        .route("/v1/runner/plex/reconcile", post(reconcile_plex))
}

async fn reconcile_plex(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<PlexReconcileRequest>,
) -> Response {
    if actor.require_runner().is_err() {
        return ApiError::forbidden(&request_id, "operation is forbidden").into_response();
    }
    match state.plex().reconcile(request).await {
        Ok(response) => Json(response).into_response(),
        Err(PlexServiceError::InvalidRequest) => {
            ApiError::invalid_request(&request_id, "Plex request is invalid").into_response()
        }
        Err(PlexServiceError::Infrastructure) => ApiError::internal(&request_id).into_response(),
    }
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
                Ok(Some(lease)) => match lease_dto(&state, &lease).await {
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
                Ok(lease) => match lease_dto(&state, &lease).await {
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

async fn lease_dto(
    state: &ApiState,
    lease: &media_core::JobLease,
) -> Result<media_contract::LeaseDto, ()> {
    let mut dto = convert::lease(lease).map_err(|_| ())?;
    match state.search().execution_for(lease.job().result_ref()).await {
        Ok(execution) => dto.execution = Some(execution),
        Err(SearchError::NotFound) => {}
        Err(_) => return Err(()),
    }
    Ok(dto)
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
