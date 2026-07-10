//! Private HTTP API for the media orchestrator.

mod auth;
mod error;
mod idempotency;
mod request_id;
mod route;

use std::sync::Arc;

use axum::{Router, extract::DefaultBodyLimit, middleware};
use media_core::{ClientStore, JobApplication, LeaseApplication, ReadinessPort};

pub use error::ApiError;
pub use idempotency::{
    IdempotencyError, IdempotencyRequest, IdempotencyStore, Reservation, StoredHttpResponse,
};
pub use request_id::RequestId;

const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;

/// Dependencies required by HTTP delivery. Concrete adapters are composed outside this crate.
#[derive(Clone)]
pub struct ApiState {
    pub(crate) jobs: Arc<JobApplication>,
    pub(crate) leases: Arc<LeaseApplication>,
    pub(crate) clients: Arc<dyn ClientStore>,
    pub(crate) idempotency: Arc<dyn IdempotencyStore>,
    pub(crate) readiness: Arc<dyn ReadinessPort>,
}

impl ApiState {
    #[must_use]
    pub fn new(
        jobs: Arc<JobApplication>,
        leases: Arc<LeaseApplication>,
        clients: Arc<dyn ClientStore>,
        idempotency: Arc<dyn IdempotencyStore>,
        readiness: Arc<dyn ReadinessPort>,
    ) -> Self {
        Self {
            jobs,
            leases,
            clients,
            idempotency,
            readiness,
        }
    }

    #[must_use]
    pub fn jobs(&self) -> &JobApplication {
        &self.jobs
    }

    #[must_use]
    pub fn leases(&self) -> &LeaseApplication {
        &self.leases
    }

    #[must_use]
    pub fn idempotency(&self) -> &dyn IdempotencyStore {
        self.idempotency.as_ref()
    }
}

/// Builds the public health endpoints and caller-supplied authenticated routes.
///
/// The request-ID layer is deliberately outermost so every downstream response,
/// including authentication failures, carries the same correlation ID.
pub fn build_router(state: ApiState, protected: Router<ApiState>) -> Router {
    let protected = if protected.has_routes() {
        protected.route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ))
    } else {
        protected
    };

    route::routes()
        .merge(protected)
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn(error::enforce_request_limits))
        .layer(middleware::from_fn(request_id::assign))
}

/// Builds an API with only the public liveness and readiness endpoints.
pub fn router(state: ApiState) -> Router {
    build_router(state, Router::new())
}
