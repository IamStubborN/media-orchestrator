use std::sync::Arc;

use crate::{
    PRIMARY_USER_ID, Actor, OperationKey, PortError, Provider, SourceChoiceAction, TrackingId,
    UserId, SECONDARY_USER_ID,
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TrackingDownload {
    provider_media_ref: String,
    translation_id: u64,
    season: u32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TrackingDownloadPatch {
    translation: String,
    download: TrackingDownload,
}

impl TrackingDownloadPatch {
    pub fn new(
        translation: String,
        download: TrackingDownload,
    ) -> Result<Self, TrackingValidationError> {
        validate_download(Provider::Rezka, &translation, Some(&download))?;
        if translation.trim().is_empty() {
            return Err(TrackingValidationError::EmptyTranslation);
        }
        if translation.contains("://") {
            return Err(TrackingValidationError::UrlNotAllowed);
        }
        Ok(Self {
            translation,
            download,
        })
    }

    #[must_use]
    pub fn translation(&self) -> &str {
        &self.translation
    }

    #[must_use]
    pub const fn download(&self) -> &TrackingDownload {
        &self.download
    }
}

impl TrackingDownload {
    pub fn new(
        provider_media_ref: String,
        translation_id: u64,
        season: u32,
    ) -> Result<Self, TrackingValidationError> {
        if provider_media_ref.trim().is_empty() {
            return Err(TrackingValidationError::EmptyProviderMediaReference);
        }
        if provider_media_ref.contains("://") {
            return Err(TrackingValidationError::UrlNotAllowed);
        }
        if provider_media_ref
            .parse::<u64>()
            .ok()
            .is_none_or(|value| value == 0)
            || translation_id == 0
            || season == 0
        {
            return Err(TrackingValidationError::InvalidDownloadSelection);
        }
        Ok(Self {
            provider_media_ref,
            translation_id,
            season,
        })
    }

    #[must_use]
    pub fn provider_media_ref(&self) -> &str {
        &self.provider_media_ref
    }

    #[must_use]
    pub const fn translation_id(&self) -> u64 {
        self.translation_id
    }

    #[must_use]
    pub const fn season(&self) -> u32 {
        self.season
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct EpisodeSnapshot {
    season: u32,
    episode: u32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EpisodeDiscovery {
    episodes: Vec<EpisodeSnapshot>,
    release_title: String,
    original_release_title: Option<String>,
}

impl EpisodeDiscovery {
    pub fn new(
        episodes: Vec<EpisodeSnapshot>,
        release_title: String,
        original_release_title: Option<String>,
    ) -> Result<Self, TrackingValidationError> {
        if release_title.trim().is_empty() {
            return Err(TrackingValidationError::EmptyTitle);
        }
        if original_release_title
            .as_ref()
            .is_some_and(|title| title.trim().is_empty())
        {
            return Err(TrackingValidationError::EmptyTitle);
        }
        Ok(Self {
            episodes,
            release_title,
            original_release_title,
        })
    }

    #[must_use]
    pub fn episodes(&self) -> &[EpisodeSnapshot] {
        &self.episodes
    }

    #[must_use]
    pub fn release_title(&self) -> &str {
        &self.release_title
    }

    #[must_use]
    pub fn original_release_title(&self) -> Option<&str> {
        self.original_release_title.as_deref()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProviderAvailability {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct EpisodeAvailability {
    rezka: ProviderAvailability,
    prowlarr: ProviderAvailability,
}

impl EpisodeAvailability {
    #[must_use]
    pub const fn new(rezka: ProviderAvailability, prowlarr: ProviderAvailability) -> Self {
        Self { rezka, prowlarr }
    }

    #[must_use]
    pub fn actions(self) -> Vec<SourceChoiceAction> {
        match (self.rezka, self.prowlarr) {
            (ProviderAvailability::Available, ProviderAvailability::Available) => vec![
                SourceChoiceAction::All,
                SourceChoiceAction::Rezka,
                SourceChoiceAction::Prowlarr,
            ],
            (ProviderAvailability::Available, _) => vec![SourceChoiceAction::Rezka],
            (_, ProviderAvailability::Available) => vec![SourceChoiceAction::Prowlarr],
            _ => Vec::new(),
        }
    }
}

pub struct EpisodeAvailabilityRequest<'a> {
    tracking: &'a TrackingSubscription,
    discovery: &'a EpisodeDiscovery,
    episode: EpisodeSnapshot,
}

impl<'a> EpisodeAvailabilityRequest<'a> {
    #[must_use]
    pub const fn new(
        tracking: &'a TrackingSubscription,
        discovery: &'a EpisodeDiscovery,
        episode: EpisodeSnapshot,
    ) -> Self {
        Self {
            tracking,
            discovery,
            episode,
        }
    }

    #[must_use]
    pub const fn tracking(&self) -> &TrackingSubscription {
        self.tracking
    }

    #[must_use]
    pub const fn discovery(&self) -> &EpisodeDiscovery {
        self.discovery
    }

    #[must_use]
    pub const fn episode(&self) -> EpisodeSnapshot {
        self.episode
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum EpisodeSnapshotError {
    #[error("episode number must be greater than zero")]
    ZeroNumber,
}

impl EpisodeSnapshot {
    pub const fn new(season: u32, episode: u32) -> Result<Self, EpisodeSnapshotError> {
        if episode == 0 {
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
    pub download: Option<TrackingDownload>,
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
    download: Option<TrackingDownload>,
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
    download: Option<TrackingDownload>,
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum TrackingValidationError {
    #[error("title cannot be empty")]
    EmptyTitle,
    #[error("translation cannot be empty")]
    EmptyTranslation,
    #[error("tracking metadata cannot contain a URL")]
    UrlNotAllowed,
    #[error("known episode snapshot cannot be empty")]
    EmptyEpisodeSnapshot,
    #[error("known episode snapshot contains duplicates")]
    DuplicateEpisode,
    #[error("provider media reference cannot be empty")]
    EmptyProviderMediaReference,
    #[error("tracking download selection is invalid")]
    InvalidDownloadSelection,
    #[error("automatic download is only supported for Rezka tracking")]
    UnsupportedDownloadProvider,
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
        validate_download(
            command.provider,
            &command.translation,
            command.download.as_ref(),
        )?;
        Ok(Self {
            id,
            owner_id,
            provider: command.provider,
            title: command.title,
            translation: command.translation,
            known_episodes: command.known_episodes,
            scope: command.scope,
            download: command.download,
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
            download: self.download,
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
    #[must_use]
    pub const fn download(&self) -> Option<&TrackingDownload> {
        self.download.as_ref()
    }
}

impl TrackingSubscription {
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        id: TrackingId,
        owner_id: UserId,
        provider: Provider,
        title: String,
        translation: String,
        known_episodes: Vec<EpisodeSnapshot>,
        scope: TrackingScope,
        download: Option<TrackingDownload>,
    ) -> Result<Self, TrackingValidationError> {
        validate(&title, &translation, &known_episodes)?;
        validate_download(provider, &translation, download.as_ref())?;
        Ok(Self {
            id,
            owner_id,
            provider,
            title,
            translation,
            known_episodes,
            scope,
            download,
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
    pub const fn download(&self) -> Option<&TrackingDownload> {
        self.download.as_ref()
    }

    #[must_use]
    pub fn is_visible_to(&self, user: UserId) -> bool {
        self.owner_id == user
            || (matches!(self.scope, TrackingScope::Family)
                && (user == PRIMARY_USER_ID || user == SECONDARY_USER_ID))
    }
}

fn validate_download(
    provider: Provider,
    translation: &str,
    download: Option<&TrackingDownload>,
) -> Result<(), TrackingValidationError> {
    let Some(download) = download else {
        return Ok(());
    };
    if provider != Provider::Rezka {
        return Err(TrackingValidationError::UnsupportedDownloadProvider);
    }
    if translation == "release-calendar"
        || download.provider_media_ref().trim().is_empty()
        || download.translation_id() == 0
        || download.season() == 0
    {
        return Err(TrackingValidationError::InvalidDownloadSelection);
    }
    Ok(())
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
    if title.contains("://") || translation.contains("://") {
        return Err(TrackingValidationError::UrlNotAllowed);
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
    async fn patch_download_visible(
        &self,
        id: TrackingId,
        user: UserId,
        patch: TrackingDownloadPatch,
    ) -> Result<Option<TrackingSubscription>, PortError>;
    async fn remove_visible(
        &self,
        operation: OperationKey,
        id: TrackingId,
        user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError>;
}

#[async_trait::async_trait]
pub trait TrackingScheduleStore: Send + Sync {
    async fn list_due(
        &self,
        now: time::OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError>;
    async fn record_future_episode(
        &self,
        id: TrackingId,
        episode: EpisodeSnapshot,
        next_check_at: time::OffsetDateTime,
        actions: Vec<SourceChoiceAction>,
    ) -> Result<bool, PortError>;
    async fn defer_check(
        &self,
        id: TrackingId,
        next_check_at: time::OffsetDateTime,
    ) -> Result<(), PortError>;
}

#[async_trait::async_trait]
pub trait EpisodeDiscoveryPort: Send + Sync {
    async fn available_episodes(
        &self,
        tracking: &TrackingSubscription,
    ) -> Result<EpisodeDiscovery, PortError>;
}

#[async_trait::async_trait]
pub trait EpisodeAvailabilityPort: Send + Sync {
    async fn probe(
        &self,
        request: EpisodeAvailabilityRequest<'_>,
    ) -> Result<EpisodeAvailability, PortError>;
}

#[async_trait::async_trait]
pub trait TrackedEpisodeDownloadPort: Send + Sync {
    async fn enqueue_episode(
        &self,
        tracking: &TrackingSubscription,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError>;
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct TrackingRunResult {
    pub checked: u32,
    pub discovered: u32,
    pub failed: u32,
    pub queued: u32,
}

pub struct TrackingRuntime {
    store: Arc<dyn TrackingScheduleStore>,
    discovery: Arc<dyn EpisodeDiscoveryPort>,
    availability: Option<Arc<dyn EpisodeAvailabilityPort>>,
    downloads: Option<Arc<dyn TrackedEpisodeDownloadPort>>,
}

impl TrackingRuntime {
    #[must_use]
    pub fn new(
        store: Arc<dyn TrackingScheduleStore>,
        discovery: Arc<dyn EpisodeDiscoveryPort>,
    ) -> Self {
        Self {
            store,
            discovery,
            availability: None,
            downloads: None,
        }
    }

    #[must_use]
    pub fn with_availability(mut self, availability: Arc<dyn EpisodeAvailabilityPort>) -> Self {
        self.availability = Some(availability);
        self
    }

    #[must_use]
    pub fn with_downloads(mut self, downloads: Arc<dyn TrackedEpisodeDownloadPort>) -> Self {
        self.downloads = Some(downloads);
        self
    }

    pub async fn run_once(
        &self,
        now: time::OffsetDateTime,
        limit: u32,
    ) -> Result<TrackingRunResult, PortError> {
        if limit == 0 || limit > 100 {
            return Err(PortError::Conflict);
        }
        let due = self.store.list_due(now, limit).await?;
        let mut result = TrackingRunResult {
            checked: 0,
            discovered: 0,
            failed: 0,
            queued: 0,
        };
        for tracking in due {
            let default_next_check = if tracking.download().is_some() {
                now + time::Duration::minutes(15)
            } else {
                now + time::Duration::hours(6)
            };
            result.checked += 1;
            let selected_season = tracking.download().map(TrackingDownload::season);
            let discovery = match self.discovery.available_episodes(&tracking).await {
                Ok(available) => available,
                Err(_) => {
                    self.store
                        .defer_check(tracking.id(), default_next_check)
                        .await?;
                    result.failed += 1;
                    continue;
                }
            };
            let baseline = selected_season
                .map(|season| {
                    tracking
                        .known_episodes()
                        .iter()
                        .filter(|episode| episode.season() == season)
                        .max()
                        .copied()
                })
                .flatten();
            let mut pending_availability = false;
            for episode in discovery.episodes().iter().copied() {
                let already_known = if tracking.download().is_some() {
                    baseline.is_some_and(|baseline| episode <= baseline)
                } else {
                    tracking.known_episodes().contains(&episode)
                };
                if already_known || selected_season.is_some_and(|season| episode.season() != season)
                {
                    continue;
                }
                let actions = if tracking.download().is_some() {
                    Vec::new()
                } else {
                    let Some(availability) = self.availability.as_deref() else {
                        result.failed += 1;
                        pending_availability = true;
                        continue;
                    };
                    let availability = match availability
                        .probe(EpisodeAvailabilityRequest::new(
                            &tracking, &discovery, episode,
                        ))
                        .await
                    {
                        Ok(availability) => availability,
                        Err(_) => {
                            result.failed += 1;
                            pending_availability = true;
                            continue;
                        }
                    };
                    let actions = availability.actions();
                    if actions.is_empty() {
                        pending_availability = true;
                        continue;
                    }
                    actions
                };
                if tracking.download().is_some() {
                    let Some(downloads) = self.downloads.as_deref() else {
                        result.failed += 1;
                        continue;
                    };
                    if downloads.enqueue_episode(&tracking, episode).await.is_err() {
                        result.failed += 1;
                        continue;
                    }
                    result.queued += 1;
                }
                if self
                    .store
                    .record_future_episode(tracking.id(), episode, default_next_check, actions)
                    .await?
                {
                    result.discovered += 1;
                }
            }
            let next_check = if pending_availability {
                now + time::Duration::minutes(30)
            } else {
                default_next_check
            };
            self.store.defer_check(tracking.id(), next_check).await?;
        }
        Ok(result)
    }
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

    pub async fn patch_download(
        &self,
        actor: &Actor,
        id: TrackingId,
        patch: TrackingDownloadPatch,
    ) -> Result<TrackingSubscription, TrackingApplicationError> {
        let user = actor
            .require_user()
            .map_err(|_| TrackingApplicationError::Forbidden)?;
        self.store
            .patch_download_visible(id, user, patch)
            .await
            .map_err(map_port_error)?
            .ok_or(TrackingApplicationError::NotFound)
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
