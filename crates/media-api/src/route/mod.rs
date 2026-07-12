mod health;
mod jobs;
mod lifecycle;
mod metrics;
mod queue;
mod release;
mod runner;
mod search;
mod tracking;

use axum::Router;

use crate::ApiState;

pub(crate) fn public_routes() -> Router<ApiState> {
    health::routes().merge(metrics::routes())
}

pub(crate) fn protected_routes() -> Router<ApiState> {
    jobs::routes()
        .merge(queue::routes())
        .merge(lifecycle::routes())
        .merge(runner::routes())
        .merge(tracking::routes())
        .merge(release::routes())
        .merge(search::routes())
}
