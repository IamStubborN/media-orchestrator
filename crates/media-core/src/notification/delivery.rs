use crate::NotificationId;

use super::{
    MediaNotification, NotificationContent, NotificationDelivery, NotificationEventType,
    NotificationRecipient, NotificationValidationError, SourceChoiceNotification,
};

impl NotificationDelivery {
    pub fn rehydrate(
        id: NotificationId,
        recipient: NotificationRecipient,
        event_type: NotificationEventType,
        status_key: Option<String>,
        message: String,
        generation: u64,
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
        if generation == 0 || generation > i64::MAX as u64 {
            return Err(NotificationValidationError::InvalidGeneration);
        }
        Ok(Self {
            id,
            recipient,
            event_type,
            status_key,
            content: NotificationContent::LegacyMessage(message.clone()),
            message,
            generation,
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
    pub const fn content(&self) -> &NotificationContent {
        &self.content
    }

    #[must_use]
    pub fn card_key(&self) -> Option<&str> {
        match &self.content {
            NotificationContent::LegacyMessage(_) => self.status_key(),
            NotificationContent::Media(notification) => Some(notification.card_key()),
            NotificationContent::SourceChoice(notification) => Some(notification.card_key()),
        }
    }

    #[must_use]
    pub fn lifecycle_cycle(&self) -> Option<u64> {
        match &self.content {
            NotificationContent::LegacyMessage(_) => None,
            NotificationContent::Media(notification) => Some(notification.lifecycle_cycle()),
            NotificationContent::SourceChoice(_) => None,
        }
    }

    pub fn rehydrate_media(
        id: NotificationId,
        recipient: NotificationRecipient,
        event_type: NotificationEventType,
        notification: MediaNotification,
        generation: u64,
        attempt_count: u32,
    ) -> Result<Self, NotificationValidationError> {
        if generation == 0 || generation > i64::MAX as u64 {
            return Err(NotificationValidationError::InvalidGeneration);
        }
        Ok(Self {
            id,
            recipient,
            event_type,
            status_key: Some(notification.card_key().to_owned()),
            message: String::new(),
            content: NotificationContent::Media(Box::new(notification)),
            generation,
            attempt_count,
        })
    }

    pub fn rehydrate_source_choice(
        id: NotificationId,
        recipient: NotificationRecipient,
        event_type: NotificationEventType,
        notification: SourceChoiceNotification,
        generation: u64,
        attempt_count: u32,
    ) -> Result<Self, NotificationValidationError> {
        if event_type != NotificationEventType::FutureEpisodeFound {
            return Err(NotificationValidationError::InvalidDisplayField);
        }
        if generation == 0 || generation > i64::MAX as u64 {
            return Err(NotificationValidationError::InvalidGeneration);
        }
        Ok(Self {
            id,
            recipient,
            event_type,
            status_key: Some(notification.card_key().to_owned()),
            message: String::new(),
            content: NotificationContent::SourceChoice(notification),
            generation,
            attempt_count,
        })
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn attempt_count(&self) -> u32 {
        self.attempt_count
    }
}
