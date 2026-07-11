use axum::{
    Json, Router,
    extract::{Extension, State},
    response::{IntoResponse, Response},
    routing::get,
};
use media_core::Actor;

use super::jobs::application_error;
use crate::{ApiState, RequestId, convert};

pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/v1/queue/status", get(status))
}

async fn status(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.jobs().queue_status(&actor).await {
        Ok(status) => Json(convert::queue_status(status)).into_response(),
        Err(error) => application_error(error, &request_id),
    }
}
