//! Private HTTP API for the media orchestrator.

mod admin;
mod auth;
mod convert;
mod details;
mod error;
mod idempotency;
mod mcp;
mod metrics;
mod plex;
mod request_id;
mod route;
mod search;
mod trending;

use std::{sync::Arc, time::Duration};

use axum::{Router, extract::DefaultBodyLimit, middleware};
use media_core::{
    ClientStore, JobApplication, LeaseApplication, MetricsSource, ReadinessPort,
    ReleaseMetadataService, RunnerLifecycleApplication, TrackingApplication,
};

use crate::metrics::MetricsRecorder;

pub use admin::{MediaAdminError, MediaAdminService};
pub use details::{MediaDetailsService, MediaDetailsServiceError};
pub use error::ApiError;
pub use idempotency::{
    IdempotencyError, IdempotencyGeneration, IdempotencyHandle, IdempotencyRequest,
    IdempotencyStore, OperationCompletionStore, Reservation, StoredHttpResponse,
};
pub use plex::{PlexReconcileService, PlexServiceError};
pub use request_id::RequestId;
pub use search::{ChoiceSetSelection, SearchError, SearchService};
pub use trending::{TrendingService, TrendingServiceError};

const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
// Conservative application-level budgets, independent of proxy/server defaults.
const MAX_REQUEST_HEADER_COUNT: usize = 64;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

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
    pub(crate) tracking: Option<Arc<TrackingApplication>>,
    pub(crate) release_metadata: Option<Arc<ReleaseMetadataService>>,
    pub(crate) lifecycle: Option<Arc<RunnerLifecycleApplication>>,
    pub(crate) search: Arc<dyn SearchService>,
    pub(crate) plex: Arc<dyn PlexReconcileService>,
    pub(crate) metrics: Arc<MetricsRecorder>,
    pub(crate) metrics_source: Option<Arc<dyn MetricsSource>>,
    pub(crate) trending: Arc<dyn TrendingService>,
    pub(crate) media_details: Arc<dyn MediaDetailsService>,
    pub(crate) admin: Arc<dyn MediaAdminService>,
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
            tracking: None,
            release_metadata: None,
            lifecycle: None,
            search: Arc::new(search::UnavailableSearchService),
            plex: Arc::new(plex::UnavailablePlexService),
            metrics: Arc::new(MetricsRecorder::default()),
            metrics_source: None,
            trending: Arc::new(trending::UnavailableTrendingService),
            media_details: Arc::new(details::UnavailableMediaDetailsService),
            admin: Arc::new(admin::UnavailableMediaAdminService),
        }
    }

    #[must_use]
    pub fn with_lifecycle(mut self, lifecycle: Arc<RunnerLifecycleApplication>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    #[must_use]
    pub fn lifecycle(&self) -> Option<&RunnerLifecycleApplication> {
        self.lifecycle.as_deref()
    }

    #[must_use]
    pub fn with_tracking(mut self, tracking: Arc<TrackingApplication>) -> Self {
        self.tracking = Some(tracking);
        self
    }

    #[must_use]
    pub fn with_release_metadata(mut self, release_metadata: Arc<ReleaseMetadataService>) -> Self {
        self.release_metadata = Some(release_metadata);
        self
    }

    #[must_use]
    pub(crate) fn release_metadata(&self) -> Option<&ReleaseMetadataService> {
        self.release_metadata.as_deref()
    }

    #[must_use]
    pub fn with_metrics_source(mut self, source: Arc<dyn MetricsSource>) -> Self {
        self.metrics_source = Some(source);
        self
    }

    #[must_use]
    pub(crate) fn metrics_source(&self) -> Option<&dyn MetricsSource> {
        self.metrics_source.as_deref()
    }

    #[must_use]
    pub fn with_search(mut self, search: Arc<dyn SearchService>) -> Self {
        self.search = search;
        self
    }

    #[must_use]
    pub fn with_trending(mut self, trending: Arc<dyn TrendingService>) -> Self {
        self.trending = trending;
        self
    }

    #[must_use]
    pub fn trending(&self) -> &dyn TrendingService {
        self.trending.as_ref()
    }

    #[must_use]
    pub fn with_media_details(mut self, media_details: Arc<dyn MediaDetailsService>) -> Self {
        self.media_details = media_details;
        self
    }

    #[must_use]
    pub fn media_details(&self) -> &dyn MediaDetailsService {
        self.media_details.as_ref()
    }

    #[must_use]
    pub fn with_admin(mut self, admin: Arc<dyn MediaAdminService>) -> Self {
        self.admin = admin;
        self
    }

    #[must_use]
    pub fn admin(&self) -> &dyn MediaAdminService {
        self.admin.as_ref()
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
    pub fn tracking(&self) -> Option<&TrackingApplication> {
        self.tracking.as_deref()
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
    let protected = route::protected_routes(state.clone()).merge(protected);
    let protected = if protected.has_routes() {
        protected.route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ))
    } else {
        protected
    };

    let metrics_state = state.clone();
    route::public_routes()
        .merge(protected)
        .with_state(state)
        // Innermost custom layer: runs after routing so the matched route pattern
        // is available, and measures handler execution including authentication.
        .layer(middleware::from_fn_with_state(
            metrics_state,
            metrics::record_http_metrics,
        ))
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
