use std::sync::Arc;

use crate::{NotificationId, PortError};

use super::NotificationDelivery;

/// Keeps the persistence-side delivery fence held across the external side
/// effect. Storage adapters construct this from an owned transaction guard.
pub struct NotificationDeliveryPermit {
    _guard: Box<dyn Send>,
}

impl NotificationDeliveryPermit {
    #[doc(hidden)]
    #[must_use]
    pub fn hold<T: Send + 'static>(guard: T) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

pub enum NotificationSinkOutcome {
    Delivered(NotificationDeliveryPermit),
    Superseded,
}

/// The outcome of a failed delivery attempt. A retryable failure is rescheduled
/// with backoff; a terminal failure (for example an authentication or signature
/// rejection that will never succeed on replay) is moved to a dead state so the
/// outbox stops re-leasing it.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct NotificationDeliveryFailure {
    code: &'static str,
    retryable: bool,
}

impl NotificationDeliveryFailure {
    #[must_use]
    pub const fn retryable(code: &'static str) -> Self {
        Self {
            code,
            retryable: true,
        }
    }

    #[must_use]
    pub const fn terminal(code: &'static str) -> Self {
        Self {
            code,
            retryable: false,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.retryable
    }
}

#[async_trait::async_trait]
pub trait NotificationOutboxPort: Send + Sync {
    async fn lease_pending(
        &self,
        worker: NotificationId,
        now: time::OffsetDateTime,
        ttl: time::Duration,
        limit: u32,
    ) -> Result<Vec<NotificationDelivery>, PortError>;
    /// Acquires a process-safe fence for the exact leased generation. `None`
    /// means a newer projection won before the external side effect began.
    async fn acquire_delivery_permit(
        &self,
        id: NotificationId,
        worker: NotificationId,
        generation: u64,
    ) -> Result<Option<NotificationDeliveryPermit>, PortError>;
    async fn mark_delivered(
        &self,
        id: NotificationId,
        worker: NotificationId,
        generation: u64,
    ) -> Result<(), PortError>;
    async fn mark_failed(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        generation: u64,
        error_code: &str,
    ) -> Result<(), PortError>;
    /// Records a terminal failure so the delivery is never leased again.
    async fn mark_dead(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        generation: u64,
        error_code: &str,
    ) -> Result<(), PortError>;
}

#[async_trait::async_trait]
pub trait NotificationSink: Send + Sync {
    async fn deliver(
        &self,
        delivery: &NotificationDelivery,
        fence: &NotificationDeliveryFence,
    ) -> Result<NotificationSinkOutcome, NotificationDeliveryFailure>;
}

pub struct NotificationDeliveryFence {
    outbox: Arc<dyn NotificationOutboxPort>,
    id: NotificationId,
    worker: NotificationId,
    generation: u64,
}

impl NotificationDeliveryFence {
    pub async fn acquire(&self) -> Result<Option<NotificationDeliveryPermit>, PortError> {
        self.outbox
            .acquire_delivery_permit(self.id, self.worker, self.generation)
            .await
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct NotificationDispatchResult {
    pub delivered: u32,
    pub failed: u32,
    pub dead: u32,
}

pub struct NotificationDispatcher {
    outbox: Arc<dyn NotificationOutboxPort>,
    sink: Arc<dyn NotificationSink>,
}

impl NotificationDispatcher {
    #[must_use]
    pub fn new(outbox: Arc<dyn NotificationOutboxPort>, sink: Arc<dyn NotificationSink>) -> Self {
        Self { outbox, sink }
    }

    pub async fn run_once(
        &self,
        worker: NotificationId,
        now: time::OffsetDateTime,
        limit: u32,
    ) -> Result<NotificationDispatchResult, PortError> {
        let deliveries = self
            .outbox
            .lease_pending(worker, now, time::Duration::seconds(30), limit)
            .await?;
        let mut result = NotificationDispatchResult::default();
        for delivery in deliveries {
            let fence = NotificationDeliveryFence {
                outbox: Arc::clone(&self.outbox),
                id: delivery.id(),
                worker,
                generation: delivery.generation(),
            };
            match self.sink.deliver(&delivery, &fence).await {
                Ok(NotificationSinkOutcome::Delivered(_permit)) => {
                    self.outbox
                        .mark_delivered(delivery.id(), worker, delivery.generation())
                        .await?;
                    result.delivered += 1;
                }
                Ok(NotificationSinkOutcome::Superseded) => {}
                Err(failure) if failure.is_retryable() => {
                    self.outbox
                        .mark_failed(
                            delivery.id(),
                            worker,
                            now,
                            delivery.generation(),
                            failure.code(),
                        )
                        .await?;
                    result.failed += 1;
                }
                Err(failure) => {
                    self.outbox
                        .mark_dead(
                            delivery.id(),
                            worker,
                            now,
                            delivery.generation(),
                            failure.code(),
                        )
                        .await?;
                    result.dead += 1;
                }
            }
        }
        Ok(result)
    }
}
