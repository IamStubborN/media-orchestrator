//! Hand-rolled Prometheus text-format exposition for the media API.
//!
//! Only four metric families are exposed, so a dedicated metrics crate would add
//! more dependency surface than value. The exposition follows the Prometheus
//! text format version 0.0.4. Labels are deliberately low cardinality: HTTP
//! series use the matched route pattern (never the raw path), the method
//! collapsed to a bounded allowlist, and the status code, none of which carry
//! identifiers or secrets or grow without bound.

use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};
use media_core::{JobState, MetricsSnapshot};

use crate::ApiState;

/// Middleware that records one counter and histogram observation per request.
///
/// The route label is the matched route pattern from [`MatchedPath`], never the
/// raw URI, so path parameters cannot inflate label cardinality. Unmatched
/// requests (the router fallback) collapse to a single `unmatched` label.
pub(crate) async fn record_http_metrics(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = normalize_method(request.method().as_str());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |path| path.as_str().to_owned());
    let response = next.run(request).await;
    state.metrics.record(
        &route,
        method,
        response.status().as_u16(),
        started.elapsed(),
    );
    response
}

/// Collapses an HTTP method token to a bounded allowlist of the standard
/// methods, mapping anything else to `other`. The metrics middleware runs before
/// authentication resolves, so an unauthenticated client could otherwise emit
/// arbitrary method tokens and grow the label cardinality of the series map
/// without bound.
fn normalize_method(method: &str) -> &'static str {
    match method {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "OPTIONS" => "OPTIONS",
        "TRACE" => "TRACE",
        "CONNECT" => "CONNECT",
        _ => "other",
    }
}

/// Content type for the Prometheus text exposition format.
pub(crate) const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Every job state, paired with its stable exposition label. Emitting the full
/// set keeps each series present even at a count of zero.
const JOB_STATES: [(JobState, &str); 12] = [
    (JobState::Queued, "queued"),
    (JobState::Leased, "leased"),
    (JobState::Running, "running"),
    (JobState::CancelRequested, "cancel_requested"),
    (JobState::BlockedStorage, "blocked_storage"),
    (JobState::Publishing, "publishing"),
    (JobState::PlexPending, "plex_pending"),
    (JobState::NeedsAction, "needs_action"),
    (JobState::Partial, "partial"),
    (JobState::Completed, "completed"),
    (JobState::Failed, "failed"),
    (JobState::Cancelled, "cancelled"),
];

/// Upper bounds, in seconds, for the request-duration histogram buckets.
const DURATION_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// In-process accumulator for HTTP request counters and duration histograms.
///
/// A single mutex is adequate: this is a private, low-traffic service and every
/// update is a handful of integer additions under bounded-cardinality keys.
#[derive(Default)]
pub(crate) struct MetricsRecorder {
    series: Mutex<HashMap<SeriesKey, SeriesValue>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct SeriesKey {
    route: String,
    method: String,
    status: u16,
}

struct SeriesValue {
    count: u64,
    sum_seconds: f64,
    buckets: [u64; DURATION_BUCKETS.len()],
}

impl Default for SeriesValue {
    fn default() -> Self {
        Self {
            count: 0,
            sum_seconds: 0.0,
            buckets: [0; DURATION_BUCKETS.len()],
        }
    }
}

impl MetricsRecorder {
    /// Records one completed HTTP request.
    pub(crate) fn record(&self, route: &str, method: &str, status: u16, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        let mut series = self
            .series
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let value = series
            .entry(SeriesKey {
                route: route.to_owned(),
                method: method.to_owned(),
                status,
            })
            .or_default();
        value.count += 1;
        value.sum_seconds += seconds;
        for (index, bound) in DURATION_BUCKETS.iter().enumerate() {
            if seconds <= *bound {
                value.buckets[index] += 1;
            }
        }
    }

    fn write_http(&self, out: &mut String) {
        let series = self
            .series
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        out.push_str("# HELP media_http_requests_total Total HTTP requests handled.\n");
        out.push_str("# TYPE media_http_requests_total counter\n");
        for (key, value) in series.iter() {
            out.push_str("media_http_requests_total");
            write_request_labels(out, key, None);
            let _ = writeln!(out, " {}", value.count);
        }

        out.push_str(
            "# HELP media_http_request_duration_seconds HTTP request duration in seconds.\n",
        );
        out.push_str("# TYPE media_http_request_duration_seconds histogram\n");
        for (key, value) in series.iter() {
            for (index, bound) in DURATION_BUCKETS.iter().enumerate() {
                out.push_str("media_http_request_duration_seconds_bucket");
                write_request_labels(out, key, Some(&format_float(*bound)));
                let _ = writeln!(out, " {}", value.buckets[index]);
            }
            out.push_str("media_http_request_duration_seconds_bucket");
            write_request_labels(out, key, Some("+Inf"));
            let _ = writeln!(out, " {}", value.count);
            out.push_str("media_http_request_duration_seconds_sum");
            write_request_labels(out, key, None);
            let _ = writeln!(out, " {}", format_float(value.sum_seconds));
            out.push_str("media_http_request_duration_seconds_count");
            write_request_labels(out, key, None);
            let _ = writeln!(out, " {}", value.count);
        }
    }
}

/// Renders the full exposition document. `snapshot` is absent when the
/// database-backed gauges could not be gathered; the endpoint still returns the
/// in-process and build metrics rather than failing the whole scrape.
pub(crate) fn render(snapshot: Option<&MetricsSnapshot>, recorder: &MetricsRecorder) -> String {
    let mut out = String::new();

    out.push_str("# HELP media_build_info Build information as a constant gauge.\n");
    out.push_str("# TYPE media_build_info gauge\n");
    let _ = writeln!(
        out,
        "media_build_info{{version=\"{}\"}} 1",
        escape_label(BUILD_VERSION)
    );

    if let Some(snapshot) = snapshot {
        let counts: HashMap<JobState, u64> = snapshot.jobs_by_state.iter().copied().collect();
        out.push_str("# HELP media_jobs_total Current number of jobs by state.\n");
        out.push_str("# TYPE media_jobs_total gauge\n");
        for (state, label) in JOB_STATES {
            let count = counts.get(&state).copied().unwrap_or(0);
            let _ = writeln!(out, "media_jobs_total{{state=\"{label}\"}} {count}");
        }

        out.push_str("# HELP media_notifications_outbox Notification outbox entries by status.\n");
        out.push_str("# TYPE media_notifications_outbox gauge\n");
        let _ = writeln!(
            out,
            "media_notifications_outbox{{status=\"pending\"}} {}",
            snapshot.notifications_pending
        );
        let _ = writeln!(
            out,
            "media_notifications_outbox{{status=\"dead\"}} {}",
            snapshot.notifications_dead
        );
    }

    recorder.write_http(&mut out);
    out
}

fn write_request_labels(out: &mut String, key: &SeriesKey, le: Option<&str>) {
    out.push_str("{route=\"");
    out.push_str(&escape_label(&key.route));
    out.push_str("\",method=\"");
    out.push_str(&escape_label(&key.method));
    let _ = write!(out, "\",status=\"{}\"", key.status);
    if let Some(le) = le {
        out.push_str(",le=\"");
        out.push_str(&escape_label(le));
        out.push('"');
    }
    out.push('}');
}

/// Escapes a Prometheus label value: backslash, double quote, and newline.
fn escape_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Formats a float without a trailing decimal point for whole numbers, matching
/// how Prometheus renders bucket boundaries (e.g. `10`, `0.005`).
fn format_float(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::{JobState, MetricsSnapshot};

    use super::{MetricsRecorder, normalize_method, render};

    #[test]
    fn unusual_method_token_collapses_to_a_bounded_other_series() {
        // Standard methods pass through; anything else becomes the single
        // `other` label so an unauthenticated client cannot inflate cardinality.
        assert_eq!(normalize_method("GET"), "GET");
        assert_eq!(normalize_method("DELETE"), "DELETE");
        assert_eq!(normalize_method("BREW"), "other");
        assert_eq!(normalize_method("\u{1}garbage"), "other");

        let recorder = MetricsRecorder::default();
        recorder.record(
            "/v1/health",
            normalize_method("BREW"),
            405,
            Duration::from_millis(1),
        );
        let body = render(None, &recorder);

        assert!(body.contains(
            "media_http_requests_total{route=\"/v1/health\",method=\"other\",status=\"405\"} 1"
        ));
        assert!(!body.contains("BREW"));
    }

    #[test]
    fn render_includes_all_families_and_zero_filled_states() {
        let recorder = MetricsRecorder::default();
        recorder.record("/v1/jobs/{id}", "GET", 200, Duration::from_millis(3));
        let snapshot = MetricsSnapshot {
            jobs_by_state: vec![(JobState::Queued, 4), (JobState::Running, 1)],
            notifications_pending: 2,
            notifications_dead: 1,
        };

        let body = render(Some(&snapshot), &recorder);

        assert!(body.contains("media_build_info{version="));
        assert!(body.contains("media_jobs_total{state=\"queued\"} 4"));
        assert!(body.contains("media_jobs_total{state=\"running\"} 1"));
        // A state with no rows is still emitted at zero.
        assert!(body.contains("media_jobs_total{state=\"failed\"} 0"));
        assert!(body.contains("media_notifications_outbox{status=\"pending\"} 2"));
        assert!(body.contains("media_notifications_outbox{status=\"dead\"} 1"));
        assert!(body.contains(
            "media_http_requests_total{route=\"/v1/jobs/{id}\",method=\"GET\",status=\"200\"} 1"
        ));
        assert!(body.contains("media_http_request_duration_seconds_bucket"));
        assert!(body.contains("le=\"+Inf\""));
        assert!(body.contains("media_http_request_duration_seconds_count"));
    }

    #[test]
    fn render_without_snapshot_still_emits_process_metrics() {
        let recorder = MetricsRecorder::default();
        recorder.record("/v1/health", "GET", 200, Duration::from_millis(1));

        let body = render(None, &recorder);

        assert!(body.contains("media_build_info"));
        assert!(body.contains("media_http_requests_total"));
        assert!(!body.contains("media_jobs_total"));
        assert!(!body.contains("media_notifications_outbox"));
    }

    #[test]
    fn histogram_buckets_are_cumulative() {
        let recorder = MetricsRecorder::default();
        recorder.record("/v1/ready", "GET", 200, Duration::from_millis(1));
        recorder.record("/v1/ready", "GET", 200, Duration::from_millis(200));

        let body = render(None, &recorder);

        // 1ms falls in the 0.005 bucket; 200ms only from 0.25 upward.
        assert!(body.contains("le=\"0.005\"} 1"));
        assert!(body.contains("le=\"0.25\"} 2"));
        assert!(body.contains("le=\"+Inf\"} 2"));
        assert!(body.contains("media_http_request_duration_seconds_count{route=\"/v1/ready\",method=\"GET\",status=\"200\"} 2"));
    }
}
