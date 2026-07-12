use std::sync::Arc;

use crate::{
    PRIMARY_USER_ID, Actor, OperationKey, PortError, Provider, TrackingId, UserId, SECONDARY_USER_ID,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum TrackingScope {
    Personal,
    Family,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum TrackingState {
    ChoiceNeeded,
    Active,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct EpisodeSnapshot {
    season: u32,
    episode: u32,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum EpisodeSnapshotError {
    #[error("season and episode numbers must be greater than zero")]
    ZeroNumber,
}

impl EpisodeSnapshot {
    pub const fn new(season: u32, episode: u32) -> Result<Self, EpisodeSnapshotError> {
        if season == 0 || episode == 0 {
            return Err(EpisodeSnapshotError::ZeroNumber);
        }
        Ok(Self { season, episode })
    }

    #[must_use]
    pub const fn season(self) -> u32 {
        self.season
    }

    #[must_use]
    pub const fn episode(self) -> u32 {
        self.episode
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NewTrackingCommand {
    pub provider: Provider,
    pub title: String,
    pub translation: String,
    pub known_episodes: Vec<EpisodeSnapshot>,
    pub scope: TrackingScope,
    pub series_ongoing: bool,
}

impl NewTrackingCommand {
    #[must_use]
    pub const fn action_state(&self) -> TrackingState {
        if self.series_ongoing {
            TrackingState::ChoiceNeeded
        } else {
            TrackingState::Active
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NewTrackingSubscription {
    id: TrackingId,
    owner_id: UserId,
    provider: Provider,
    title: String,
    translation: String,
    known_episodes: Vec<EpisodeSnapshot>,
    scope: TrackingScope,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TrackingSubscription {
    id: TrackingId,
    owner_id: UserId,
    provider: Provider,
    title: String,
    translation: String,
    known_episodes: Vec<EpisodeSnapshot>,
    scope: TrackingScope,
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum TrackingValidationError {
    #[error("title cannot be empty")]
    EmptyTitle,
    #[error("translation cannot be empty")]
    EmptyTranslation,
    #[error("known episode snapshot cannot be empty")]
    EmptyEpisodeSnapshot,
    #[error("known episode snapshot contains duplicates")]
    DuplicateEpisode,
    #[error("only ongoing series can be tracked")]
    SeriesNotOngoing,
}

impl NewTrackingSubscription {
    pub fn new(
        id: TrackingId,
        owner_id: UserId,
        command: NewTrackingCommand,
    ) -> Result<Self, TrackingValidationError> {
        validate(
            &command.title,
            &command.translation,
            &command.known_episodes,
        )?;
        if !command.series_ongoing {
            return Err(TrackingValidationError::SeriesNotOngoing);
        }
        Ok(Self {
            id,
            owner_id,
            provider: command.provider,
            title: command.title,
            translation: command.translation,
            known_episodes: command.known_episodes,
            scope: command.scope,
        })
    }

    #[must_use]
    pub fn into_persisted(self) -> TrackingSubscription {
        TrackingSubscription {
            id: self.id,
            owner_id: self.owner_id,
            provider: self.provider,
            title: self.title,
            translation: self.translation,
            known_episodes: self.known_episodes,
            scope: self.scope,
        }
    }

    #[must_use]
    pub const fn id(&self) -> TrackingId {
        self.id
    }
    #[must_use]
    pub const fn owner_id(&self) -> UserId {
        self.owner_id
    }
    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn translation(&self) -> &str {
        &self.translation
    }
    #[must_use]
    pub fn known_episodes(&self) -> &[EpisodeSnapshot] {
        &self.known_episodes
    }
    #[must_use]
    pub const fn scope(&self) -> TrackingScope {
        self.scope
    }
}

impl TrackingSubscription {
    pub fn rehydrate(
        id: TrackingId,
        owner_id: UserId,
        provider: Provider,
        title: String,
        translation: String,
        known_episodes: Vec<EpisodeSnapshot>,
        scope: TrackingScope,
    ) -> Result<Self, TrackingValidationError> {
        validate(&title, &translation, &known_episodes)?;
        Ok(Self {
            id,
            owner_id,
            provider,
            title,
            translation,
            known_episodes,
            scope,
        })
    }

    #[must_use]
    pub const fn id(&self) -> TrackingId {
        self.id
    }
    #[must_use]
    pub const fn owner_id(&self) -> UserId {
        self.owner_id
    }
    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn translation(&self) -> &str {
        &self.translation
    }
    #[must_use]
    pub fn known_episodes(&self) -> &[EpisodeSnapshot] {
        &self.known_episodes
    }
    #[must_use]
    pub const fn scope(&self) -> TrackingScope {
        self.scope
    }

    #[must_use]
    pub fn is_visible_to(&self, user: UserId) -> bool {
        self.owner_id == user
            || (matches!(self.scope, TrackingScope::Family)
                && (user == PRIMARY_USER_ID || user == SECONDARY_USER_ID))
    }
}

fn validate(
    title: &str,
    translation: &str,
    known_episodes: &[EpisodeSnapshot],
) -> Result<(), TrackingValidationError> {
    if title.trim().is_empty() {
        return Err(TrackingValidationError::EmptyTitle);
    }
    if translation.trim().is_empty() {
        return Err(TrackingValidationError::EmptyTranslation);
    }
    if known_episodes.is_empty() {
        return Err(TrackingValidationError::EmptyEpisodeSnapshot);
    }
    let mut sorted = known_episodes.to_vec();
    sorted.sort_unstable();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(TrackingValidationError::DuplicateEpisode);
    }
    Ok(())
}

#[async_trait::async_trait]
pub trait TrackingStore: Send + Sync {
    async fn add(
        &self,
        operation: OperationKey,
        value: NewTrackingSubscription,
    ) -> Result<TrackingSubscription, PortError>;
    async fn list_visible(&self, user: UserId) -> Result<Vec<TrackingSubscription>, PortError>;
    async fn remove_visible(
        &self,
        operation: OperationKey,
        id: TrackingId,
        user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError>;
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum TrackingApplicationError {
    #[error("operation is forbidden")]
    Forbidden,
    #[error("invalid tracking request: {0}")]
    InvalidInput(#[source] TrackingValidationError),
    #[error("tracking subscription was not found")]
    NotFound,
    #[error("operation conflicts with current state")]
    Conflict,
    #[error("infrastructure operation failed")]
    Infrastructure,
}

pub struct TrackingApplication {
    store: Arc<dyn TrackingStore>,
}

impl TrackingApplication {
    #[must_use]
    pub fn new(store: Arc<dyn TrackingStore>) -> Self {
        Self { store }
    }

    pub async fn add(
        &self,
        actor: &Actor,
        operation: OperationKey,
        command: NewTrackingCommand,
    ) -> Result<TrackingSubscription, TrackingApplicationError> {
        let owner = actor
            .require_user()
            .map_err(|_| TrackingApplicationError::Forbidden)?;
        let value = NewTrackingSubscription::new(TrackingId::new(), owner, command)
            .map_err(TrackingApplicationError::InvalidInput)?;
        self.store
            .add(operation, value)
            .await
            .map_err(map_port_error)
    }

    pub async fn list(
        &self,
        actor: &Actor,
    ) -> Result<Vec<TrackingSubscription>, TrackingApplicationError> {
        let user = actor
            .require_user()
            .map_err(|_| TrackingApplicationError::Forbidden)?;
        self.store.list_visible(user).await.map_err(map_port_error)
    }

    pub async fn remove(
        &self,
        actor: &Actor,
        operation: OperationKey,
        id: TrackingId,
    ) -> Result<TrackingSubscription, TrackingApplicationError> {
        let user = actor
            .require_user()
            .map_err(|_| TrackingApplicationError::Forbidden)?;
        self.store
            .remove_visible(operation, id, user)
            .await
            .map_err(map_port_error)?
            .ok_or(TrackingApplicationError::NotFound)
    }
}

const fn map_port_error(error: PortError) -> TrackingApplicationError {
    match error {
        PortError::Conflict => TrackingApplicationError::Conflict,
        PortError::Infrastructure => TrackingApplicationError::Infrastructure,
    }
}
