mod health;
mod jobs;
mod lifecycle;
mod media_details;
mod metrics;
mod queue;
mod release;
mod runner;
mod search;
mod tracking;
mod trending;

use axum::Router;

use crate::ApiState;

pub(crate) fn public_routes() -> Router<ApiState> {
    health::routes().merge(metrics::routes())
}

pub(crate) fn protected_routes(state: ApiState) -> Router<ApiState> {
    jobs::routes()
        .merge(queue::routes())
        .merge(lifecycle::routes())
        .merge(runner::routes())
        .merge(tracking::routes())
        .merge(release::routes())
        .merge(search::routes())
        .merge(trending::routes())
        .merge(media_details::routes())
        .merge(crate::mcp::routes(state))
}
