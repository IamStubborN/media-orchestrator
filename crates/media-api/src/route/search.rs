use axum::{
    Json, Router,
    extract::{Extension, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use media_contract::{
    ContinueSearchRequest, RezkaSessionRefreshRequest, SelectResultRequest, StartSearchRequest,
};
use media_core::Actor;

use crate::{ApiError, ApiState, RequestId, SearchError, idempotency};

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/searches", post(start))
        .route("/v1/searches/continue", post(continue_search))
        .route("/v1/selections", post(select))
        .route("/v1/rezka/session/refresh", post(refresh_rezka_session))
}

async fn refresh_rezka_session(
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
            let Ok(owner) = actor.require_user() else {
                return ApiError::forbidden(&request_id, "operation is forbidden").into_response();
            };
            let Ok(request) = serde_json::from_slice::<RezkaSessionRefreshRequest>(&body) else {
                return ApiError::invalid_request(
                    &request_id,
                    "session refresh request is invalid",
                )
                .into_response();
            };
            match state
                .search()
                .refresh_rezka_session(owner, operation, request)
                .await
            {
                Ok(job) => (StatusCode::CREATED, Json(job)).into_response(),
                Err(error) => search_error(error, &request_id),
            }
        },
    )
    .await
}

async fn start(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
    request: Request,
) -> Response {
    execute::<StartSearchRequest>(state, actor, request_id, request, true).await
}

async fn continue_search(
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
        |state, actor, request_id, _, body| async move {
            let Ok(owner) = actor.require_user() else {
                return ApiError::forbidden(&request_id, "operation is forbidden").into_response();
            };
            let Ok(request) = serde_json::from_slice::<ContinueSearchRequest>(&body) else {
                return ApiError::invalid_request(&request_id, "search request is invalid")
                    .into_response();
            };
            match state.search().continue_search(owner, request).await {
                Ok(page) => Json(page).into_response(),
                Err(error) => search_error(error, &request_id),
            }
        },
    )
    .await
}

async fn select(
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
            let Ok(owner) = actor.require_user() else {
                return ApiError::forbidden(&request_id, "operation is forbidden").into_response();
            };
            let Ok(request) = serde_json::from_slice::<SelectResultRequest>(&body) else {
                return ApiError::invalid_request(&request_id, "selection request is invalid")
                    .into_response();
            };
            match state.search().select(owner, operation, request).await {
                Ok(job) => (StatusCode::CREATED, Json(job)).into_response(),
                Err(error) => search_error(error, &request_id),
            }
        },
    )
    .await
}

async fn execute<T>(
    state: ApiState,
    actor: Actor,
    request_id: RequestId,
    request: Request,
    created: bool,
) -> Response
where
    T: serde::de::DeserializeOwned + Into<StartSearchRequest> + Send + 'static,
{
    idempotency::execute(
        state,
        actor,
        request_id,
        request,
        move |state, actor, request_id, _, body| async move {
            let Ok(owner) = actor.require_user() else {
                return ApiError::forbidden(&request_id, "operation is forbidden").into_response();
            };
            let Ok(request) = serde_json::from_slice::<T>(&body) else {
                return ApiError::invalid_request(&request_id, "search request is invalid")
                    .into_response();
            };
            match state.search().start(owner, request.into()).await {
                Ok(page) if created => (StatusCode::CREATED, Json(page)).into_response(),
                Ok(page) => Json(page).into_response(),
                Err(error) => search_error(error, &request_id),
            }
        },
    )
    .await
}

fn search_error(error: SearchError, request_id: &RequestId) -> Response {
    match error {
        SearchError::InvalidRequest => {
            ApiError::invalid_request(request_id, "search request is invalid").into_response()
        }
        SearchError::Forbidden => {
            ApiError::forbidden(request_id, "operation is forbidden").into_response()
        }
        SearchError::NotFound => ApiError::not_found(request_id).into_response(),
        SearchError::Conflict => ApiError::conflict(request_id).into_response(),
        SearchError::Provider | SearchError::Infrastructure => {
            ApiError::internal(request_id).into_response()
        }
    }
}
