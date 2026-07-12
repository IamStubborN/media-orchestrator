//! Private HTTP API for the media orchestrator.

mod auth;
mod convert;
mod error;
mod idempotency;
mod plex;
mod request_id;
mod route;
mod search;

use std::{sync::Arc, time::Duration};

use axum::{Router, extract::DefaultBodyLimit, middleware};
use media_core::{ClientStore, JobApplication, LeaseApplication, ReadinessPort};

pub use error::ApiError;
pub use idempotency::{
    IdempotencyError, IdempotencyGeneration, IdempotencyHandle, IdempotencyRequest,
    IdempotencyStore, OperationCompletionStore, Reservation, StoredHttpResponse,
};
pub use plex::{PlexReconcileService, PlexServiceError};
pub use request_id::RequestId;
pub use search::{SearchError, SearchService};

const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
// Conservative application-level budgets, independent of proxy/server defaults.
const MAX_REQUEST_HEADER_COUNT: usize = 64;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Copy, Clone)]
pub(crate) struct RequestTimeout(Duration);

/// Dependencies required by HTTP delivery. Concrete adapters are composed outside this crate.
#[derive(Clone)]
pub struct ApiState {
    pub(crate) jobs: Arc<JobApplication>,
    pub(crate) leases: Arc<LeaseApplication>,
    pub(crate) clients: Arc<dyn ClientStore>,
    pub(crate) idempotency: Arc<dyn IdempotencyStore>,
    pub(crate) operations: Arc<dyn OperationCompletionStore>,
    pub(crate) readiness: Arc<dyn ReadinessPort>,
    pub(crate) search: Arc<dyn SearchService>,
    pub(crate) plex: Arc<dyn PlexReconcileService>,
}

impl ApiState {
    #[must_use]
    pub fn new(
        jobs: Arc<JobApplication>,
        leases: Arc<LeaseApplication>,
        clients: Arc<dyn ClientStore>,
        idempotency: Arc<dyn IdempotencyStore>,
        operations: Arc<dyn OperationCompletionStore>,
        readiness: Arc<dyn ReadinessPort>,
    ) -> Self {
        Self {
            jobs,
            leases,
            clients,
            idempotency,
            operations,
            readiness,
            search: Arc::new(search::UnavailableSearchService),
            plex: Arc::new(plex::UnavailablePlexService),
        }
    }

    #[must_use]
    pub fn with_search(mut self, search: Arc<dyn SearchService>) -> Self {
        self.search = search;
        self
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

    #[must_use]
    pub fn operations(&self) -> &dyn OperationCompletionStore {
        self.operations.as_ref()
    }

    #[must_use]
    pub fn search(&self) -> &dyn SearchService {
        self.search.as_ref()
    }

    #[must_use]
    pub fn with_plex(mut self, plex: Arc<dyn PlexReconcileService>) -> Self {
        self.plex = plex;
        self
    }

    #[must_use]
    pub fn plex(&self) -> &dyn PlexReconcileService {
        self.plex.as_ref()
    }
}

/// Builds the public health endpoints and caller-supplied authenticated routes.
///
/// The request-ID layer is deliberately outermost so every downstream response,
/// including authentication failures, carries the same correlation ID.
pub fn build_router(state: ApiState, protected: Router<ApiState>) -> Router {
    build_router_with_request_timeout(state, protected, DEFAULT_REQUEST_TIMEOUT)
}

/// Builds the API with an explicit deadline for focused timeout tests.
pub fn build_router_with_request_timeout(
    state: ApiState,
    protected: Router<ApiState>,
    request_timeout: Duration,
) -> Router {
    let protected = route::protected_routes().merge(protected);
    let protected = if protected.has_routes() {
        protected.route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ))
    } else {
        protected
    };

    route::public_routes()
        .merge(protected)
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn(error::enforce_request_limits))
        .layer(middleware::from_fn_with_state(
            RequestTimeout(request_timeout),
            error::enforce_request_timeout,
        ))
        .layer(middleware::from_fn(request_id::assign))
}

/// Builds an API with only the public liveness and readiness endpoints.
pub fn router(state: ApiState) -> Router {
    build_router(state, Router::new())
}
