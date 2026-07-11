use axum::{
    Json, Router,
    extract::{Extension, State},
    routing::get,
};
use serde::Serialize;

use crate::{ApiError, ApiState, RequestId};

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/ready", get(ready))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn ready(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<HealthResponse>, ApiError> {
    match state.readiness.is_ready().await {
        Ok(true) => Ok(Json(HealthResponse { status: "ok" })),
        Ok(false) | Err(_) => Err(ApiError::not_ready(&request_id)),
    }
}
