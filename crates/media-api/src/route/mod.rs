mod health;
mod jobs;
mod queue;
mod runner;
mod search;

use axum::Router;

use crate::ApiState;

pub(crate) fn public_routes() -> Router<ApiState> {
    health::routes()
}

pub(crate) fn protected_routes() -> Router<ApiState> {
    jobs::routes()
        .merge(queue::routes())
        .merge(runner::routes())
        .merge(search::routes())
}
