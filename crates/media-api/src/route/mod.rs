mod health;

use axum::Router;

use crate::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    health::routes()
}
