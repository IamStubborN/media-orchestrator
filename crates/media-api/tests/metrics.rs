mod support;

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use media_api::build_router;
use media_core::{JobState, MetricsSnapshot, MetricsSource, PortError};
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, state};

struct FakeMetricsSource {
    snapshot: Result<MetricsSnapshot, PortError>,
}

impl FakeMetricsSource {
    fn ok(snapshot: MetricsSnapshot) -> Self {
        Self {
            snapshot: Ok(snapshot),
        }
    }

    fn failing() -> Self {
        Self {
            snapshot: Err(PortError::Infrastructure),
        }
    }
}

#[async_trait::async_trait]
impl MetricsSource for FakeMetricsSource {
    async fn snapshot(&self) -> Result<MetricsSnapshot, PortError> {
        self.snapshot.clone()
    }
}

fn app_with_source(source: Arc<dyn MetricsSource>) -> Router {
    // The metrics endpoint is unauthenticated, so the client store is never
    // consulted; `failing()` simply provides one without seeding actors.
    let state =
        state(FakeClientStore::failing(), FakeReadiness::ready()).with_metrics_source(source);
    build_router(state, Router::new())
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_text_with_expected_families() {
    let snapshot = MetricsSnapshot {
        jobs_by_state: vec![(JobState::Queued, 3), (JobState::Running, 1)],
        notifications_pending: 5,
        notifications_dead: 2,
    };
    let response = app_with_source(Arc::new(FakeMetricsSource::ok(snapshot)))
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        content_type.starts_with("text/plain"),
        "unexpected content type: {content_type}"
    );

    let body = body_text(response).await;
    assert!(body.contains("media_build_info{version="));
    assert!(body.contains("media_jobs_total{state=\"queued\"} 3"));
    assert!(body.contains("media_jobs_total{state=\"running\"} 1"));
    assert!(body.contains("media_jobs_total{state=\"failed\"} 0"));
    assert!(body.contains("media_notifications_outbox{status=\"pending\"} 5"));
    assert!(body.contains("media_notifications_outbox{status=\"dead\"} 2"));
    assert!(body.contains("# TYPE media_http_request_duration_seconds histogram"));
}

#[tokio::test]
async fn metrics_endpoint_bypasses_bearer_authentication() {
    // No Authorization header: a protected route would return 401, but /metrics
    // lives on the public router outside the bearer-auth layer.
    let response = app_with_source(Arc::new(FakeMetricsSource::failing()))
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    // A failed database snapshot still yields process and build metrics.
    assert!(body.contains("media_build_info"));
    assert!(!body.contains("media_jobs_total"));
}

#[tokio::test]
async fn http_requests_are_counted_with_the_matched_route_label() {
    let app = app_with_source(Arc::new(FakeMetricsSource::ok(MetricsSnapshot::default())));

    // Exercise a matched route so the middleware records a sample.
    let health = app
        .clone()
        .oneshot(Request::get("/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let scrape = app
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(scrape.status(), StatusCode::OK);
    let body = body_text(scrape).await;
    assert!(
        body.contains(
            "media_http_requests_total{route=\"/v1/health\",method=\"GET\",status=\"200\"} 1"
        ),
        "missing matched-route counter series in:\n{body}"
    );
}

#[tokio::test]
async fn unusual_request_method_is_recorded_as_a_bounded_other_label() {
    let app = app_with_source(Arc::new(FakeMetricsSource::ok(MetricsSnapshot::default())));

    // The metrics middleware wraps the whole router before auth resolves, so an
    // unauthenticated client can pick the method token. An arbitrary token must
    // collapse to `other` rather than becoming its own unbounded series.
    let odd = app
        .clone()
        .oneshot(
            Request::builder()
                .method("BREW")
                .uri("/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(odd.status(), StatusCode::OK);

    let scrape = app
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = body_text(scrape).await;
    assert!(
        body.contains("method=\"other\""),
        "unusual method should collapse to the `other` label in:\n{body}"
    );
    assert!(
        !body.contains("BREW"),
        "raw method token must never become a label in:\n{body}"
    );
}
