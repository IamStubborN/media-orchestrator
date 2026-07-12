use std::sync::Arc;

use crate::{NotificationId, PortError};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum NotificationRecipient {
    Primary,
    Secondary,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum NotificationEventType {
    Started,
    ChoiceNeeded,
    Downloaded,
    EncodingComplete,
    PlexAdded,
    Partial,
    BlockedStorage,
    Failed,
    FutureEpisodeFound,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NotificationDelivery {
    id: NotificationId,
    recipient: NotificationRecipient,
    event_type: NotificationEventType,
    message: String,
    attempt_count: u32,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum NotificationValidationError {
    #[error("notification message cannot be empty")]
    EmptyMessage,
    #[error("notification message cannot contain a URL")]
    UrlNotAllowed,
}

impl NotificationDelivery {
    pub fn rehydrate(
        id: NotificationId,
        recipient: NotificationRecipient,
        event_type: NotificationEventType,
        message: String,
        attempt_count: u32,
    ) -> Result<Self, NotificationValidationError> {
        if message.trim().is_empty() {
            return Err(NotificationValidationError::EmptyMessage);
        }
        if message.contains("://") {
            return Err(NotificationValidationError::UrlNotAllowed);
        }
        Ok(Self {
            id,
            recipient,
            event_type,
            message,
            attempt_count,
        })
    }

    #[must_use]
    pub const fn id(&self) -> NotificationId {
        self.id
    }
    #[must_use]
    pub const fn recipient(&self) -> NotificationRecipient {
        self.recipient
    }
    #[must_use]
    pub const fn event_type(&self) -> NotificationEventType {
        self.event_type
    }
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
    #[must_use]
    pub const fn attempt_count(&self) -> u32 {
        self.attempt_count
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
    async fn mark_delivered(
        &self,
        id: NotificationId,
        worker: NotificationId,
    ) -> Result<(), PortError>;
    async fn mark_failed(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        error_code: &str,
    ) -> Result<(), PortError>;
}

#[async_trait::async_trait]
pub trait NotificationSink: Send + Sync {
    async fn deliver(&self, delivery: &NotificationDelivery) -> Result<(), &'static str>;
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct NotificationDispatchResult {
    pub delivered: u32,
    pub failed: u32,
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
        let mut result = NotificationDispatchResult {
            delivered: 0,
            failed: 0,
        };
        for delivery in deliveries {
            match self.sink.deliver(&delivery).await {
                Ok(()) => {
                    self.outbox.mark_delivered(delivery.id(), worker).await?;
                    result.delivered += 1;
                }
                Err(code) => {
                    self.outbox
                        .mark_failed(delivery.id(), worker, now, code)
                        .await?;
                    result.failed += 1;
                }
            }
        }
        Ok(result)
    }
}
