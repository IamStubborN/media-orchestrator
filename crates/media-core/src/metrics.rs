use crate::{JobState, PortError};

/// Point-in-time operational counts gathered for monitoring exposure.
///
/// This is a read-only observability model, not a domain type: it exists so a
/// delivery boundary can publish gauges without reaching into persistence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetricsSnapshot {
    /// Job counts grouped by state. States with no rows may be omitted; the
    /// consumer is responsible for emitting a complete label set when required.
    pub jobs_by_state: Vec<(JobState, u64)>,
    /// Notification outbox entries awaiting delivery.
    pub notifications_pending: u64,
    /// Notification outbox entries that reached the dead-letter terminal state.
    pub notifications_dead: u64,
}

/// Read-only port that gathers monitoring gauges at scrape time.
///
/// It is intentionally narrow: it exposes only aggregate counts, never
/// per-entity data, so a scrape cannot leak identifiers or secrets.
#[async_trait::async_trait]
pub trait MetricsSource: Send + Sync {
    async fn snapshot(&self) -> Result<MetricsSnapshot, PortError>;
}

#[cfg(test)]
mod tests {
    use super::MetricsSource;

    #[test]
    fn metrics_source_is_object_safe() {
        fn accept(_: Option<&dyn MetricsSource>) {}
        accept(None);
    }
}
