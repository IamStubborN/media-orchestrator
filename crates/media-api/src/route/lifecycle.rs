use axum::{
    Json, Router,
    extract::{Extension, State},
    response::{IntoResponse, Response},
    routing::get,
};
use media_contract::{RunnerLifecycleDto, RunnerLifecycleStateDto, UpdateRunnerLifecycleRequest};
use media_core::{
    Actor, ApplicationError, RunnerLifecycle, RunnerLifecycleState, RunnerLifecycleUpdate,
};

use crate::{ApiError, ApiState, RequestId};

pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/v1/runner/lifecycle", get(get_state).put(update_state))
}

async fn get_state(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let Some(application) = state.lifecycle() else {
        return ApiError::internal(&request_id).into_response();
    };
    match application.get(&actor).await {
        Ok(value) => Json(dto(value)).into_response(),
        Err(error) => app_error(error, &request_id),
    }
}

async fn update_state(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<UpdateRunnerLifecycleRequest>,
) -> Response {
    if !valid(&request) {
        return ApiError::invalid_request(&request_id, "runner lifecycle is invalid")
            .into_response();
    }
    let Some(application) = state.lifecycle() else {
        return ApiError::internal(&request_id).into_response();
    };
    let update = RunnerLifecycleUpdate {
        state: state_from_dto(request.state),
        reason: request.reason,
        previous_ip: request.previous_ip,
        current_ip: request.current_ip,
    };
    match application.update(&actor, update).await {
        Ok(value) => Json(dto(value)).into_response(),
        Err(error) => app_error(error, &request_id),
    }
}

fn valid(request: &UpdateRunnerLifecycleRequest) -> bool {
    let reason_present = request
        .reason
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    matches!(
        (request.state, reason_present),
        (RunnerLifecycleStateDto::Blocked, true)
            | (
                RunnerLifecycleStateDto::Ready | RunnerLifecycleStateDto::Rotating,
                false
            )
    )
}

const fn state_from_dto(value: RunnerLifecycleStateDto) -> RunnerLifecycleState {
    match value {
        RunnerLifecycleStateDto::Ready => RunnerLifecycleState::Ready,
        RunnerLifecycleStateDto::Rotating => RunnerLifecycleState::Rotating,
        RunnerLifecycleStateDto::Blocked => RunnerLifecycleState::Blocked,
    }
}

fn dto(value: RunnerLifecycle) -> RunnerLifecycleDto {
    RunnerLifecycleDto {
        state: match value.state {
            RunnerLifecycleState::Ready => RunnerLifecycleStateDto::Ready,
            RunnerLifecycleState::Rotating => RunnerLifecycleStateDto::Rotating,
            RunnerLifecycleState::Blocked => RunnerLifecycleStateDto::Blocked,
        },
        reason: value.reason,
        previous_ip: value.previous_ip,
        current_ip: value.current_ip,
        updated_at: value
            .updated_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    }
}

fn app_error(error: ApplicationError, request_id: &RequestId) -> Response {
    match error {
        ApplicationError::Forbidden => {
            ApiError::forbidden(request_id, "operation is forbidden").into_response()
        }
        ApplicationError::Conflict => ApiError::conflict(request_id).into_response(),
        ApplicationError::InvalidInput(_) => {
            ApiError::invalid_request(request_id, "runner lifecycle is invalid").into_response()
        }
        ApplicationError::NotFound | ApplicationError::Infrastructure => {
            ApiError::internal(request_id).into_response()
        }
    }
}
