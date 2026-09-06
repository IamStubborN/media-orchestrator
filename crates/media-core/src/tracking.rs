use std::{collections::BTreeMap, sync::Arc};

use crate::{
    PRIMARY_USER_ID, Actor, OperationKey, PortError, Provider, SourceChoiceAction,
    TrackingClaimToken, TrackingId, UserId, SECONDARY_USER_ID,
};

/// Stable opaque identifier for the provider choices discovered for one tracked
/// episode. The identifier is not sufficient for authorization on its own;
/// callers must still load the underlying session for the authenticated owner.
pub fn episode_choice_set_id(id: TrackingId, season: u32, episode: u32) -> String {
    let value = format!("tracking:{}:{}:{}", id, season, episode);
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, value.as_bytes()).to_string()
}

const NOTIFY_TRACKING_INTERVAL: time::Duration = time::Duration::hours(3);
const DOWNLOAD_TRACKING_INTERVAL: time::Duration = time::Duration::minutes(30);
const TRACKING_CLAIM_LEASE: time::Duration = time::Duration::minutes(15);

pub const ENQUEUE_FAILURE_CODE: &str = "enqueue_failed";
pub const ENQUEUE_SEARCH_FAILURE_CODE: &str = "enqueue_search_failed";
pub const ENQUEUE_VERIFY_FAILURE_CODE: &str = "enqueue_verify_failed";
pub const ENQUEUE_PERSIST_FAILURE_CODE: &str = "enqueue_persist_failed";
pub const ENQUEUE_JOB_FAILURE_CODE: &str = "enqueue_job_failed";
pub const SOURCE_PROBE_FAILURE_CODE: &str = "source_probe_failed";
pub const SOURCE_UNAVAILABLE_CODE: &str = "source_unavailable";
pub const RELEASE_INFRASTRUCTURE_FAILURE_CODE: &str = "release_infrastructure";
pub const RELEASE_CONFLICT_FAILURE_CODE: &str = "release_conflict";

/// Exponential cooldown for repeated identical tracking failures.
///
/// `failure_count` is 1-based after the current failure. Policy:
/// 1 → 15m, 2 → 30m, 3 → 60m, 4 → 120m, 5+ → 240m (cap).
#[must_use]
pub fn tracking_failure_cooldown(failure_count: u32) -> time::Duration {
    let exponent = failure_count.saturating_sub(1).min(4);
    let minutes = 15u32.saturating_mul(1u32 << exponent).min(240);
    time::Duration::minutes(i64::from(minutes))
}

/// Increments when the failure code repeats; otherwise restarts at 1.
#[must_use]
pub fn next_check_failure_count(
    previous_count: u32,
    previous_error: Option<&str>,
    new_error: &str,
) -> u32 {
    if previous_error == Some(new_error) {
        previous_count.saturating_add(1).max(1)
    } else {
        1
    }
}

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

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum TrackingCheckStatus {
    Never,
    NoNewEpisode,
    AwaitingSource,
    EpisodeFound,
    DownloadQueued,
    ReleaseError,
    SourceError,
}

/// Durable outcome written when a claimed tracking check finishes.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TrackingCheckOutcome {
    pub next_check_at: time::OffsetDateTime,
    pub status: TrackingCheckStatus,
    pub last_error: Option<String>,
    pub failure_count: u32,
}

impl TrackingCheckOutcome {
    #[must_use]
    pub fn success(next_check_at: time::OffsetDateTime, status: TrackingCheckStatus) -> Self {
        Self {
            next_check_at,
            status,
            last_error: None,
            failure_count: 0,
        }
    }

    #[must_use]
    pub fn failure(
        next_check_at: time::OffsetDateTime,
        status: TrackingCheckStatus,
        last_error: impl Into<String>,
        failure_count: u32,
    ) -> Self {
        Self {
            next_check_at,
            status,
            last_error: Some(last_error.into()),
            failure_count,
        }
    }
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
    poster_url: Option<String>,
    release_identity: Option<crate::ReleaseIdentity>,
    last_scheduled_by_season: BTreeMap<u32, u32>,
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
            poster_url: None,
            release_identity: None,
            last_scheduled_by_season: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn with_poster_url(mut self, poster_url: Option<String>) -> Self {
        self.poster_url = poster_url;
        self
    }

    #[must_use]
    pub fn with_release_identity(mut self, release_identity: crate::ReleaseIdentity) -> Self {
        self.release_identity = Some(release_identity);
        self
    }

    #[must_use]
    pub fn with_last_scheduled_by_season(
        mut self,
        last_scheduled_by_season: BTreeMap<u32, u32>,
    ) -> Self {
        self.last_scheduled_by_season = last_scheduled_by_season;
        self
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

    #[must_use]
    pub fn poster_url(&self) -> Option<&str> {
        self.poster_url.as_deref()
    }

    #[must_use]
    pub const fn release_identity(&self) -> Option<crate::ReleaseIdentity> {
        self.release_identity
    }

    #[must_use]
    pub fn last_scheduled_by_season(&self) -> &BTreeMap<u32, u32> {
        &self.last_scheduled_by_season
    }

    #[must_use]
    pub fn is_last_scheduled_episode(&self, episode: EpisodeSnapshot) -> bool {
        self.last_scheduled_by_season
            .get(&episode.season())
            .is_some_and(|&last| last == episode.episode())
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
    rezka_count: u32,
    prowlarr_count: u32,
}

impl EpisodeAvailability {
    #[must_use]
    pub const fn new(rezka: ProviderAvailability, prowlarr: ProviderAvailability) -> Self {
        Self {
            rezka,
            prowlarr,
            rezka_count: 0,
            prowlarr_count: 0,
        }
    }

    #[must_use]
    pub const fn with_counts(mut self, rezka_count: u32, prowlarr_count: u32) -> Self {
        self.rezka_count = rezka_count;
        self.prowlarr_count = prowlarr_count;
        self
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

    #[must_use]
    pub const fn prowlarr(self) -> ProviderAvailability {
        self.prowlarr
    }

    #[must_use]
    pub const fn rezka_count(self) -> u32 {
        self.rezka_count
    }

    #[must_use]
    pub const fn prowlarr_count(self) -> u32 {
        self.prowlarr_count
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
    pub poster_url: Option<String>,
    pub release_identity: Option<crate::ReleaseIdentity>,
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
    poster_url: Option<String>,
    release_identity: Option<crate::ReleaseIdentity>,
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
    poster_url: Option<String>,
    release_identity: Option<crate::ReleaseIdentity>,
    download: Option<TrackingDownload>,
    last_checked_at: Option<time::OffsetDateTime>,
    next_check_at: time::OffsetDateTime,
    check_status: TrackingCheckStatus,
    check_last_error: Option<String>,
    check_failure_count: u32,
    pending_episodes: Vec<EpisodeSnapshot>,
    pending_since: Option<time::OffsetDateTime>,
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum TrackingValidationError {
    #[error("title cannot be empty")]
    EmptyTitle,
    #[error("translation cannot be empty")]
    EmptyTranslation,
    #[error("tracking metadata cannot contain a URL")]
    UrlNotAllowed,
    #[error("tracking poster URL is invalid")]
    InvalidPosterUrl,
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
    #[error("release-calendar tracking requires a positive TVmaze release_identity")]
    MissingReleaseIdentity,
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
        validate_poster_url(command.poster_url.as_deref())?;
        if !command.series_ongoing {
            return Err(TrackingValidationError::SeriesNotOngoing);
        }
        if command.translation == "release-calendar"
            && command
                .release_identity
                .as_ref()
                .is_none_or(|identity| identity.source_id() == 0)
        {
            return Err(TrackingValidationError::MissingReleaseIdentity);
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
            poster_url: command.poster_url,
            release_identity: command.release_identity,
            download: command.download,
        })
    }

    #[must_use]
    pub fn into_persisted(self) -> TrackingSubscription {
        let now = time::OffsetDateTime::now_utc();
        TrackingSubscription {
            id: self.id,
            owner_id: self.owner_id,
            provider: self.provider,
            title: self.title,
            translation: self.translation,
            known_episodes: self.known_episodes,
            scope: self.scope,
            poster_url: self.poster_url,
            release_identity: self.release_identity,
            download: self.download,
            last_checked_at: None,
            next_check_at: now,
            check_status: TrackingCheckStatus::Never,
            check_last_error: None,
            check_failure_count: 0,
            pending_episodes: Vec::new(),
            pending_since: None,
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
    pub fn poster_url(&self) -> Option<&str> {
        self.poster_url.as_deref()
    }
    #[must_use]
    pub const fn release_identity(&self) -> Option<crate::ReleaseIdentity> {
        self.release_identity
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
        Self::rehydrate_with_identity(
            id,
            owner_id,
            provider,
            title,
            translation,
            known_episodes,
            scope,
            None,
            download,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate_with_poster(
        id: TrackingId,
        owner_id: UserId,
        provider: Provider,
        title: String,
        translation: String,
        known_episodes: Vec<EpisodeSnapshot>,
        scope: TrackingScope,
        download: Option<TrackingDownload>,
        poster_url: Option<String>,
    ) -> Result<Self, TrackingValidationError> {
        Self::rehydrate_with_check_identity_and_poster(
            id,
            owner_id,
            provider,
            title,
            translation,
            known_episodes,
            scope,
            None,
            download,
            poster_url,
            None,
            time::OffsetDateTime::now_utc(),
            TrackingCheckStatus::Never,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate_with_identity(
        id: TrackingId,
        owner_id: UserId,
        provider: Provider,
        title: String,
        translation: String,
        known_episodes: Vec<EpisodeSnapshot>,
        scope: TrackingScope,
        release_identity: Option<crate::ReleaseIdentity>,
        download: Option<TrackingDownload>,
    ) -> Result<Self, TrackingValidationError> {
        Self::rehydrate_with_check_and_identity(
            id,
            owner_id,
            provider,
            title,
            translation,
            known_episodes,
            scope,
            release_identity,
            download,
            None,
            time::OffsetDateTime::now_utc(),
            TrackingCheckStatus::Never,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate_with_check(
        id: TrackingId,
        owner_id: UserId,
        provider: Provider,
        title: String,
        translation: String,
        known_episodes: Vec<EpisodeSnapshot>,
        scope: TrackingScope,
        download: Option<TrackingDownload>,
        last_checked_at: Option<time::OffsetDateTime>,
        next_check_at: time::OffsetDateTime,
        check_status: TrackingCheckStatus,
    ) -> Result<Self, TrackingValidationError> {
        Self::rehydrate_with_check_and_identity(
            id,
            owner_id,
            provider,
            title,
            translation,
            known_episodes,
            scope,
            None,
            download,
            last_checked_at,
            next_check_at,
            check_status,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate_with_check_and_identity(
        id: TrackingId,
        owner_id: UserId,
        provider: Provider,
        title: String,
        translation: String,
        known_episodes: Vec<EpisodeSnapshot>,
        scope: TrackingScope,
        release_identity: Option<crate::ReleaseIdentity>,
        download: Option<TrackingDownload>,
        last_checked_at: Option<time::OffsetDateTime>,
        next_check_at: time::OffsetDateTime,
        check_status: TrackingCheckStatus,
    ) -> Result<Self, TrackingValidationError> {
        Self::rehydrate_with_check_identity_and_poster(
            id,
            owner_id,
            provider,
            title,
            translation,
            known_episodes,
            scope,
            release_identity,
            download,
            None,
            last_checked_at,
            next_check_at,
            check_status,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate_with_check_identity_and_poster(
        id: TrackingId,
        owner_id: UserId,
        provider: Provider,
        title: String,
        translation: String,
        known_episodes: Vec<EpisodeSnapshot>,
        scope: TrackingScope,
        release_identity: Option<crate::ReleaseIdentity>,
        download: Option<TrackingDownload>,
        poster_url: Option<String>,
        last_checked_at: Option<time::OffsetDateTime>,
        next_check_at: time::OffsetDateTime,
        check_status: TrackingCheckStatus,
    ) -> Result<Self, TrackingValidationError> {
        validate(&title, &translation, &known_episodes)?;
        validate_download(provider, &translation, download.as_ref())?;
        validate_poster_url(poster_url.as_deref())?;
        Ok(Self {
            id,
            owner_id,
            provider,
            title,
            translation,
            known_episodes,
            scope,
            poster_url,
            release_identity,
            download,
            last_checked_at,
            next_check_at,
            check_status,
            check_last_error: None,
            check_failure_count: 0,
            pending_episodes: Vec::new(),
            pending_since: None,
        })
    }

    #[must_use]
    pub fn with_check_diagnostics(
        mut self,
        check_last_error: Option<String>,
        check_failure_count: u32,
    ) -> Self {
        self.check_last_error = check_last_error;
        self.check_failure_count = check_failure_count;
        self
    }

    #[must_use]
    pub fn with_pending_diagnostics(
        mut self,
        pending_episodes: Vec<EpisodeSnapshot>,
        pending_since: Option<time::OffsetDateTime>,
    ) -> Self {
        self.pending_episodes = pending_episodes;
        self.pending_since = pending_since;
        self
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
    pub fn poster_url(&self) -> Option<&str> {
        self.poster_url.as_deref()
    }
    #[must_use]
    pub const fn release_identity(&self) -> Option<crate::ReleaseIdentity> {
        self.release_identity
    }
    #[must_use]
    pub const fn download(&self) -> Option<&TrackingDownload> {
        self.download.as_ref()
    }
    #[must_use]
    pub const fn last_checked_at(&self) -> Option<time::OffsetDateTime> {
        self.last_checked_at
    }
    #[must_use]
    pub const fn next_check_at(&self) -> time::OffsetDateTime {
        self.next_check_at
    }
    #[must_use]
    pub const fn check_status(&self) -> TrackingCheckStatus {
        self.check_status
    }
    #[must_use]
    pub fn check_last_error(&self) -> Option<&str> {
        self.check_last_error.as_deref()
    }
    #[must_use]
    pub const fn check_failure_count(&self) -> u32 {
        self.check_failure_count
    }
    #[must_use]
    pub fn pending_episodes(&self) -> &[EpisodeSnapshot] {
        &self.pending_episodes
    }
    #[must_use]
    pub const fn pending_since(&self) -> Option<time::OffsetDateTime> {
        self.pending_since
    }

    /// Human-readable reason for Hermes cards when status alone looks silent.
    #[must_use]
    pub fn status_reason(&self) -> Option<String> {
        match self.check_status {
            TrackingCheckStatus::AwaitingSource => {
                let pending = self.pending_episodes.last().or_else(|| {
                    self.known_episodes
                        .iter()
                        .max_by_key(|episode| (episode.season(), episode.episode()))
                });
                match pending {
                    Some(episode) => Some(format!(
                        "aired S{:02}E{:02}, waiting for Rezka/Prowlarr",
                        episode.season(),
                        episode.episode()
                    )),
                    None => Some("waiting for Rezka/Prowlarr".to_owned()),
                }
            }
            TrackingCheckStatus::SourceError => Some(match self.check_last_error.as_deref() {
                Some(ENQUEUE_SEARCH_FAILURE_CODE) => {
                    "auto-download search failed; backing off retries".to_owned()
                }
                Some(ENQUEUE_VERIFY_FAILURE_CODE) => {
                    "auto-download verify failed; backing off retries".to_owned()
                }
                Some(ENQUEUE_PERSIST_FAILURE_CODE) => {
                    "auto-download persist failed; backing off retries".to_owned()
                }
                Some(ENQUEUE_JOB_FAILURE_CODE) => {
                    "auto-download job create failed; backing off retries".to_owned()
                }
                Some(ENQUEUE_FAILURE_CODE) => {
                    "auto-download enqueue failed; backing off retries".to_owned()
                }
                Some(SOURCE_PROBE_FAILURE_CODE) => {
                    "source probe failed while checking availability".to_owned()
                }
                Some(SOURCE_UNAVAILABLE_CODE) => {
                    "download source is not available yet".to_owned()
                }
                Some(other) => format!("source error: {other}"),
                None => "source error".to_owned(),
            }),
            TrackingCheckStatus::ReleaseError => Some(match self.check_last_error.as_deref() {
                Some(RELEASE_CONFLICT_FAILURE_CODE) => {
                    "release calendar conflict while discovering episodes".to_owned()
                }
                Some(RELEASE_INFRASTRUCTURE_FAILURE_CODE) => {
                    "release calendar unavailable while discovering episodes".to_owned()
                }
                Some(other) => format!("release error: {other}"),
                None => "release error".to_owned(),
            }),
            _ => None,
        }
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

fn validate_poster_url(value: Option<&str>) -> Result<(), TrackingValidationError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_empty() || value.len() > 2048 {
        return Err(TrackingValidationError::InvalidPosterUrl);
    }
    let url = url::Url::parse(value).map_err(|_| TrackingValidationError::InvalidPosterUrl)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(TrackingValidationError::InvalidPosterUrl);
    }
    Ok(())
}

#[must_use]
pub fn is_valid_tracking_poster_url(value: &str) -> bool {
    validate_poster_url(Some(value)).is_ok()
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
    async fn set_baseline_visible(
        &self,
        _id: TrackingId,
        _user: UserId,
        _baseline: EpisodeSnapshot,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        Err(PortError::Conflict)
    }
    async fn request_check_visible(
        &self,
        _id: TrackingId,
        _user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        Err(PortError::Conflict)
    }
    async fn remove_visible(
        &self,
        operation: OperationKey,
        id: TrackingId,
        user: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError>;
}

/// The complete immutable input for recording one discovered episode.
///
/// Keeping this as a value object avoids widening the scheduling port every
/// time discovery metadata is added while making the provider counts explicit
/// at the call site.
#[derive(Debug, Clone)]
pub struct FutureEpisodeRecord {
    pub id: TrackingId,
    pub claim_token: TrackingClaimToken,
    pub episode: EpisodeSnapshot,
    pub next_check_at: time::OffsetDateTime,
    pub actions: Vec<SourceChoiceAction>,
    pub poster_url: Option<String>,
    pub rezka_count: u32,
    pub prowlarr_count: u32,
    pub season_complete: bool,
}

#[async_trait::async_trait]
pub trait TrackingScheduleStore: Send + Sync {
    async fn claim_due(
        &self,
        now: time::OffsetDateTime,
        claim_token: TrackingClaimToken,
        claim_until: time::OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<TrackingSubscription>, PortError>;
    async fn set_release_metadata_if_missing(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        release_identity: crate::ReleaseIdentity,
        poster_url: String,
    ) -> Result<(), PortError>;
    async fn record_future_episode(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
        next_check_at: time::OffsetDateTime,
        actions: Vec<SourceChoiceAction>,
        poster_url: Option<String>,
    ) -> Result<bool, PortError>;

    /// Records a discovery and carries provider candidate counts into the
    /// compact source-choice notification. Existing adapters can keep the
    /// legacy method and receive zero counts until upgraded.
    async fn record_future_episode_with_counts(
        &self,
        record: FutureEpisodeRecord,
    ) -> Result<bool, PortError> {
        self.record_future_episode(
            record.id,
            record.claim_token,
            record.episode,
            record.next_check_at,
            record.actions,
            record.poster_url,
        )
        .await
    }
    async fn pending_episodes(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
    ) -> Result<Vec<EpisodeSnapshot>, PortError>;
    async fn record_pending_episode(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError>;
    async fn reserve_episode_download(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        episode: EpisodeSnapshot,
    ) -> Result<(), PortError>;
    async fn release_episode_download(
        &self,
        _id: TrackingId,
        _claim_token: TrackingClaimToken,
        _episode: EpisodeSnapshot,
    ) -> Result<(), PortError> {
        Err(PortError::Conflict)
    }
    async fn finish_check(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        next_check_at: time::OffsetDateTime,
        status: TrackingCheckStatus,
    ) -> Result<(), PortError>;

    /// Persist check status plus failure diagnostics used for enqueue backoff.
    /// Default adapters keep the legacy `finish_check` behavior.
    async fn finish_check_outcome(
        &self,
        id: TrackingId,
        claim_token: TrackingClaimToken,
        outcome: TrackingCheckOutcome,
    ) -> Result<(), PortError> {
        self.finish_check(id, claim_token, outcome.next_check_at, outcome.status)
            .await
    }
}

#[async_trait::async_trait]
pub trait EpisodeDiscoveryPort: Send + Sync {
    async fn resolved_release_metadata(
        &self,
        _tracking: &TrackingSubscription,
    ) -> Result<Option<(crate::ReleaseIdentity, String)>, PortError> {
        Ok(None)
    }

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
    /// Returns Ok(()) on success, or Err with a stable enqueue failure code
    /// (`enqueue_search_failed`, `enqueue_verify_failed`, …).
    async fn enqueue_episode(
        &self,
        tracking: &TrackingSubscription,
        episode: EpisodeSnapshot,
    ) -> Result<(), &'static str>;
}

#[async_trait::async_trait]
pub trait AnonymousSessionPort: Send + Sync {
    async fn prepare_anonymous_session(&self) -> Result<(), PortError>;
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct TrackingRunResult {
    pub checked: u32,
    pub discovered: u32,
    pub failed: u32,
    pub queued: u32,
    pub release_conflict_failures: u32,
    pub release_infrastructure_failures: u32,
    pub source_failures: u32,
}

pub struct TrackingRuntime {
    store: Arc<dyn TrackingScheduleStore>,
    discovery: Arc<dyn EpisodeDiscoveryPort>,
    availability: Option<Arc<dyn EpisodeAvailabilityPort>>,
    downloads: Option<Arc<dyn TrackedEpisodeDownloadPort>>,
    session: Option<Arc<dyn AnonymousSessionPort>>,
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
            session: None,
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

    #[must_use]
    pub fn with_anonymous_session(mut self, session: Arc<dyn AnonymousSessionPort>) -> Self {
        self.session = Some(session);
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
        let claim_token = TrackingClaimToken::new();
        let due = self
            .store
            .claim_due(now, claim_token, now + TRACKING_CLAIM_LEASE, limit)
            .await?;
        if !due.is_empty()
            && let Some(session) = self.session.as_deref()
        {
            let _ = session.prepare_anonymous_session().await;
        }
        let mut result = TrackingRunResult {
            checked: 0,
            discovered: 0,
            failed: 0,
            queued: 0,
            release_conflict_failures: 0,
            release_infrastructure_failures: 0,
            source_failures: 0,
        };
        for tracking in due {
            let default_next_check = if tracking.download().is_some() {
                now + DOWNLOAD_TRACKING_INTERVAL
            } else {
                now + NOTIFY_TRACKING_INTERVAL
            };
            result.checked += 1;
            let selected_season = tracking.download().map(TrackingDownload::season);
            let mut metadata_backfilled = false;
            if tracking.poster_url().is_none()
                && let Ok(Some((release_identity, poster_url))) =
                    self.discovery.resolved_release_metadata(&tracking).await
                && is_valid_tracking_poster_url(&poster_url)
            {
                metadata_backfilled = self
                    .store
                    .set_release_metadata_if_missing(
                        tracking.id(),
                        claim_token,
                        release_identity,
                        poster_url,
                    )
                    .await
                    .is_ok();
            }
            let discovery = match self.discovery.available_episodes(&tracking).await {
                Ok(available) => available,
                Err(error) => {
                    let error_code = match error {
                        PortError::Conflict => RELEASE_CONFLICT_FAILURE_CODE,
                        PortError::Infrastructure => RELEASE_INFRASTRUCTURE_FAILURE_CODE,
                    };
                    let failure_count = next_check_failure_count(
                        tracking.check_failure_count(),
                        tracking.check_last_error(),
                        error_code,
                    );
                    let _ = self
                        .store
                        .finish_check_outcome(
                            tracking.id(),
                            claim_token,
                            TrackingCheckOutcome::failure(
                                time::OffsetDateTime::now_utc().max(now)
                                    + tracking_failure_cooldown(failure_count),
                                TrackingCheckStatus::ReleaseError,
                                error_code,
                                failure_count,
                            ),
                        )
                        .await;
                    result.failed += 1;
                    match error {
                        PortError::Conflict => result.release_conflict_failures += 1,
                        PortError::Infrastructure => result.release_infrastructure_failures += 1,
                    }
                    continue;
                }
            };
            if !metadata_backfilled
                && tracking.poster_url().is_none()
                && let (Some(release_identity), Some(poster_url)) =
                    (discovery.release_identity(), discovery.poster_url())
                && is_valid_tracking_poster_url(poster_url)
            {
                let _ = self
                    .store
                    .set_release_metadata_if_missing(
                        tracking.id(),
                        claim_token,
                        release_identity,
                        poster_url.to_owned(),
                    )
                    .await;
            }
            let baseline = selected_season.and_then(|season| {
                tracking
                    .known_episodes()
                    .iter()
                    .filter(|episode| episode.season() == season)
                    .max()
                    .copied()
            });
            let pending = if tracking.download().is_none() {
                match self
                    .store
                    .pending_episodes(tracking.id(), claim_token)
                    .await
                {
                    Ok(pending) => pending,
                    Err(_) => {
                        let failure_count = next_check_failure_count(
                            tracking.check_failure_count(),
                            tracking.check_last_error(),
                            SOURCE_PROBE_FAILURE_CODE,
                        );
                        let _ = self
                            .store
                            .finish_check_outcome(
                                tracking.id(),
                                claim_token,
                                TrackingCheckOutcome::failure(
                                    time::OffsetDateTime::now_utc().max(now)
                                        + tracking_failure_cooldown(failure_count),
                                    TrackingCheckStatus::SourceError,
                                    SOURCE_PROBE_FAILURE_CODE,
                                    failure_count,
                                ),
                            )
                            .await;
                        result.failed += 1;
                        result.source_failures += 1;
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            let tracked_season = tracking
                .known_episodes()
                .iter()
                .map(|episode| episode.season())
                .max();
            let mut pending_availability = false;
            let mut source_error = false;
            let mut failure_code: Option<&'static str> = None;
            let discovered_before = result.discovered;
            let queued_before = result.queued;
            for episode in discovery.episodes().iter().copied() {
                let already_known = if tracking.download().is_some() {
                    baseline.is_some_and(|baseline| episode <= baseline)
                } else {
                    tracking.known_episodes().contains(&episode)
                };
                let latest_known_in_season = tracking
                    .known_episodes()
                    .iter()
                    .filter(|known| known.season() == episode.season())
                    .max()
                    .copied();
                let pending_candidate = pending.contains(&episode);
                if already_known
                    || selected_season.is_some_and(|season| episode.season() != season)
                    || (tracking.download().is_none()
                        && tracked_season.is_some_and(|season| episode.season() < season))
                    || (tracking.download().is_none()
                        && !pending_candidate
                        && latest_known_in_season.is_some_and(|latest| episode <= latest))
                {
                    continue;
                }
                let (actions, (rezka_count, prowlarr_count), season_complete) =
                    if tracking.download().is_some() {
                        (Vec::new(), (0, 0), false)
                    } else {
                        let Some(availability) = self.availability.as_deref() else {
                            if self
                                .store
                                .record_pending_episode(tracking.id(), claim_token, episode)
                                .await
                                .is_err()
                            {
                                result.failed += 1;
                                result.source_failures += 1;
                                source_error = true;
                                failure_code.get_or_insert(SOURCE_PROBE_FAILURE_CODE);
                                continue;
                            }
                            result.failed += 1;
                            result.source_failures += 1;
                            pending_availability = true;
                            source_error = true;
                            failure_code.get_or_insert(SOURCE_PROBE_FAILURE_CODE);
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
                                if self
                                    .store
                                    .record_pending_episode(tracking.id(), claim_token, episode)
                                    .await
                                    .is_err()
                                {
                                    result.failed += 1;
                                    result.source_failures += 1;
                                    source_error = true;
                                    failure_code.get_or_insert(SOURCE_PROBE_FAILURE_CODE);
                                    continue;
                                }
                                result.failed += 1;
                                result.source_failures += 1;
                                pending_availability = true;
                                source_error = true;
                                failure_code.get_or_insert(SOURCE_PROBE_FAILURE_CODE);
                                continue;
                            }
                        };
                        let counts = (availability.rezka_count(), availability.prowlarr_count());
                        let actions = availability.actions();
                        if actions.is_empty() {
                            if self
                                .store
                                .record_pending_episode(tracking.id(), claim_token, episode)
                                .await
                                .is_err()
                            {
                                result.failed += 1;
                                result.source_failures += 1;
                                source_error = true;
                                failure_code.get_or_insert(SOURCE_PROBE_FAILURE_CODE);
                                continue;
                            }
                            pending_availability = true;
                            continue;
                        }
                        (
                            actions,
                            counts,
                            discovery.is_last_scheduled_episode(episode),
                        )
                    };
                if tracking.download().is_some() {
                    let Some(downloads) = self.downloads.as_deref() else {
                        result.failed += 1;
                        result.source_failures += 1;
                        source_error = true;
                        failure_code.get_or_insert(SOURCE_UNAVAILABLE_CODE);
                        continue;
                    };
                    if self
                        .store
                        .reserve_episode_download(tracking.id(), claim_token, episode)
                        .await
                        .is_err()
                    {
                        result.failed += 1;
                        result.source_failures += 1;
                        source_error = true;
                        failure_code.get_or_insert(ENQUEUE_FAILURE_CODE);
                        continue;
                    }
                    if let Err(code) = downloads.enqueue_episode(&tracking, episode).await {
                        let _ = self
                            .store
                            .release_episode_download(tracking.id(), claim_token, episode)
                            .await;
                        result.failed += 1;
                        result.source_failures += 1;
                        source_error = true;
                        failure_code = Some(code);
                        continue;
                    }
                    result.queued += 1;
                }
                let recorded = self
                    .store
                    .record_future_episode_with_counts(FutureEpisodeRecord {
                        id: tracking.id(),
                        claim_token,
                        episode,
                        next_check_at: default_next_check,
                        actions,
                        poster_url: discovery.poster_url().map(str::to_owned),
                        rezka_count,
                        prowlarr_count,
                        season_complete,
                    })
                    .await;
                match recorded {
                    Ok(true) => result.discovered += 1,
                    Ok(false) => {}
                    Err(_) => {
                        result.failed += 1;
                        result.source_failures += 1;
                        source_error = true;
                        failure_code.get_or_insert(SOURCE_PROBE_FAILURE_CODE);
                    }
                }
            }
            let status = if result.queued > queued_before {
                TrackingCheckStatus::DownloadQueued
            } else if result.discovered > discovered_before {
                TrackingCheckStatus::EpisodeFound
            } else if source_error {
                TrackingCheckStatus::SourceError
            } else if pending_availability {
                TrackingCheckStatus::AwaitingSource
            } else {
                TrackingCheckStatus::NoNewEpisode
            };
            let outcome = if source_error {
                let error_code = failure_code.unwrap_or(SOURCE_PROBE_FAILURE_CODE);
                let failure_count = next_check_failure_count(
                    tracking.check_failure_count(),
                    tracking.check_last_error(),
                    error_code,
                );
                TrackingCheckOutcome::failure(
                    time::OffsetDateTime::now_utc().max(now)
                        + tracking_failure_cooldown(failure_count),
                    status,
                    error_code,
                    failure_count,
                )
            } else {
                let next_check = if pending_availability {
                    now + NOTIFY_TRACKING_INTERVAL
                } else {
                    default_next_check
                };
                TrackingCheckOutcome::success(next_check, status)
            };
            if self
                .store
                .finish_check_outcome(tracking.id(), claim_token, outcome)
                .await
                .is_err()
            {
                result.failed += 1;
                result.source_failures += 1;
            }
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
    #[error("tracking subscription already exists")]
    AlreadyExists(TrackingId),
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
        let release_identity = command.release_identity;
        let provider = command.provider;
        let title = command.title.clone();
        let translation = command.translation.clone();
        let scope = command.scope;
        let value = NewTrackingSubscription::new(TrackingId::new(), owner, command)
            .map_err(TrackingApplicationError::InvalidInput)?;
        match self.store.add(operation, value).await {
            Ok(created) => Ok(created),
            Err(PortError::Conflict) => {
                let listed = self.store.list_visible(owner).await.map_err(map_port_error)?;
                if let Some(identity) = release_identity {
                    if let Some(existing) = listed.iter().find(|candidate| {
                        candidate.download().is_none()
                            && candidate.release_identity() == Some(identity)
                    }) {
                        return Err(TrackingApplicationError::AlreadyExists(existing.id()));
                    }
                }
                if let Some(existing) = listed.into_iter().find(|candidate| {
                    candidate.provider() == provider
                        && candidate.title() == title
                        && candidate.translation() == translation
                        && candidate.scope() == scope
                        && (scope != TrackingScope::Personal || candidate.owner_id() == owner)
                }) {
                    return Err(TrackingApplicationError::AlreadyExists(existing.id()));
                }
                Err(TrackingApplicationError::Conflict)
            }
            Err(error) => Err(map_port_error(error)),
        }
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

    pub async fn set_baseline(
        &self,
        actor: &Actor,
        id: TrackingId,
        baseline: EpisodeSnapshot,
    ) -> Result<TrackingSubscription, TrackingApplicationError> {
        let user = actor
            .require_user()
            .map_err(|_| TrackingApplicationError::Forbidden)?;
        self.store
            .set_baseline_visible(id, user, baseline)
            .await
            .map_err(map_port_error)?
            .ok_or(TrackingApplicationError::NotFound)
    }

    pub async fn check_now(
        &self,
        actor: &Actor,
        id: TrackingId,
    ) -> Result<TrackingSubscription, TrackingApplicationError> {
        let user = actor
            .require_user()
            .map_err(|_| TrackingApplicationError::Forbidden)?;
        self.store
            .request_check_visible(id, user)
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

#[cfg(test)]
mod failure_backoff_tests {
    use super::{
        ENQUEUE_FAILURE_CODE, next_check_failure_count, tracking_failure_cooldown,
    };

    #[test]
    fn tracking_failure_cooldown_grows_then_caps() {
        assert_eq!(tracking_failure_cooldown(1), time::Duration::minutes(15));
        assert_eq!(tracking_failure_cooldown(2), time::Duration::minutes(30));
        assert_eq!(tracking_failure_cooldown(3), time::Duration::minutes(60));
        assert_eq!(tracking_failure_cooldown(4), time::Duration::minutes(120));
        assert_eq!(tracking_failure_cooldown(5), time::Duration::minutes(240));
        assert_eq!(tracking_failure_cooldown(20), time::Duration::minutes(240));
    }

    #[test]
    fn identical_enqueue_failures_increment_while_new_codes_reset() {
        assert_eq!(
            next_check_failure_count(0, None, ENQUEUE_FAILURE_CODE),
            1
        );
        assert_eq!(
            next_check_failure_count(1, Some(ENQUEUE_FAILURE_CODE), ENQUEUE_FAILURE_CODE),
            2
        );
        assert_eq!(
            next_check_failure_count(4, Some(ENQUEUE_FAILURE_CODE), ENQUEUE_FAILURE_CODE),
            5
        );
        assert_eq!(
            next_check_failure_count(5, Some(ENQUEUE_FAILURE_CODE), "other"),
            1
        );
    }
}
