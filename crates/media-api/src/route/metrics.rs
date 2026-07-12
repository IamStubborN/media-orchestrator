use axum::{
    Router,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};

use crate::{ApiState, metrics};

pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/metrics", get(scrape))
}

/// Unauthenticated Prometheus scrape endpoint.
///
/// Like the health and readiness endpoints, this lives on the private network
/// and is intentionally outside the bearer-auth layer. A failed database
/// snapshot degrades to process-only metrics rather than failing the scrape.
async fn scrape(State(state): State<ApiState>) -> Response {
    let snapshot = match state.metrics_source() {
        Some(source) => source.snapshot().await.ok(),
        None => None,
    };
    let body = metrics::render(snapshot.as_ref(), &state.metrics);
    ([(header::CONTENT_TYPE, metrics::CONTENT_TYPE)], body).into_response()
}
