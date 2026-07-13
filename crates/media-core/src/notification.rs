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
    DownloadingStarted,
    Downloaded,
    TranscodingStarted,
    EncodingComplete,
    PlexAdded,
    Completed,
    SessionRefreshed,
    Partial,
    BlockedStorage,
    Failed,
    FutureEpisodeFound,
}

impl NotificationEventType {
    /// Parses the persisted wire tag for a notification event type, returning
    /// `None` for an unknown tag. The tags are the stable strings written to the
    /// notification outbox `event_type` column.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "started" => Self::Started,
            "choice-needed" => Self::ChoiceNeeded,
            "downloading-started" => Self::DownloadingStarted,
            "downloaded" => Self::Downloaded,
            "transcoding-started" => Self::TranscodingStarted,
            "encoding-complete" => Self::EncodingComplete,
            "plex-added" => Self::PlexAdded,
            "completed" => Self::Completed,
            "session-refreshed" => Self::SessionRefreshed,
            "partial" => Self::Partial,
            "blocked-storage" => Self::BlockedStorage,
            "failed" => Self::Failed,
            "future-episode-found" => Self::FutureEpisodeFound,
            _ => return None,
        })
    }

    /// Whether this event is an intermediate progress milestone ("downloading
    /// started", "transcoding started"). Progress milestones are noise for a
    /// co-owner who did not initiate the job, so recipient selection routes them
    /// to the initiator only, even under a `Family` notify scope. Terminal and
    /// action-required events keep the job's configured scope routing.
    #[must_use]
    pub const fn is_progress_milestone(self) -> bool {
        matches!(self, Self::DownloadingStarted | Self::TranscodingStarted)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NotificationDelivery {
    id: NotificationId,
    recipient: NotificationRecipient,
    event_type: NotificationEventType,
    status_key: Option<String>,
    message: String,
    attempt_count: u32,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum NotificationValidationError {
    #[error("notification message cannot be empty")]
    EmptyMessage,
    #[error("notification message cannot contain a URL")]
    UrlNotAllowed,
    #[error("notification status key is invalid")]
    InvalidStatusKey,
}

impl NotificationDelivery {
    pub fn rehydrate(
        id: NotificationId,
        recipient: NotificationRecipient,
        event_type: NotificationEventType,
        status_key: Option<String>,
        message: String,
        attempt_count: u32,
    ) -> Result<Self, NotificationValidationError> {
        if message.trim().is_empty() {
            return Err(NotificationValidationError::EmptyMessage);
        }
        if message.contains("://") {
            return Err(NotificationValidationError::UrlNotAllowed);
        }
        if status_key.as_ref().is_some_and(|key| {
            key.is_empty()
                || key.len() > 96
                || !key.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, ':' | '-')
                })
        }) {
            return Err(NotificationValidationError::InvalidStatusKey);
        }
        Ok(Self {
            id,
            recipient,
            event_type,
            status_key,
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
    pub fn status_key(&self) -> Option<&str> {
        self.status_key.as_deref()
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
    /// Records a terminal failure so the delivery is never leased again.
    async fn mark_dead(
        &self,
        id: NotificationId,
        worker: NotificationId,
        now: time::OffsetDateTime,
        error_code: &str,
    ) -> Result<(), PortError>;
}

#[async_trait::async_trait]
pub trait NotificationSink: Send + Sync {
    async fn deliver(
        &self,
        delivery: &NotificationDelivery,
    ) -> Result<(), NotificationDeliveryFailure>;
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
            match self.sink.deliver(&delivery).await {
                Ok(()) => {
                    self.outbox.mark_delivered(delivery.id(), worker).await?;
                    result.delivered += 1;
                }
                Err(failure) if failure.is_retryable() => {
                    self.outbox
                        .mark_failed(delivery.id(), worker, now, failure.code())
                        .await?;
                    result.failed += 1;
                }
                Err(failure) => {
                    self.outbox
                        .mark_dead(delivery.id(), worker, now, failure.code())
                        .await?;
                    result.dead += 1;
                }
            }
        }
        Ok(result)
    }
}
