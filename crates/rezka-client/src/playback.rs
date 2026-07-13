use std::fmt;

use crate::{
    ProviderFailureReason, RezkaError, StreamVariant, SubtitleTrack,
    catalog::{RezkaMediaKind, RezkaTitleId, TitleLocator, Translation, TranslationKey},
    session::RezkaClient,
};

pub mod parser;

const AJAX_PATH: &str = "/ajax/get_cdn_series/";

#[derive(Clone)]
pub struct TitlePlaybackRef {
    id: RezkaTitleId,
    locator: TitleLocator,
    kind: RezkaMediaKind,
}

impl TitlePlaybackRef {
    pub(crate) fn new(id: RezkaTitleId, locator: TitleLocator, kind: RezkaMediaKind) -> Self {
        Self { id, locator, kind }
    }

    #[must_use]
    pub const fn id(&self) -> RezkaTitleId {
        self.id
    }

    #[must_use]
    pub const fn locator(&self) -> &TitleLocator {
        &self.locator
    }

    #[must_use]
    pub const fn kind(&self) -> RezkaMediaKind {
        self.kind
    }
}

impl fmt::Debug for TitlePlaybackRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TitlePlaybackRef")
            .field("id", &self.id)
            .field("locator", &"[REDACTED]")
            .field("kind", &self.kind)
            .finish()
    }
}

#[derive(Clone)]
pub struct SelectedTranslation {
    title: TitlePlaybackRef,
    translation: Translation,
}

impl SelectedTranslation {
    pub(crate) fn new(title: TitlePlaybackRef, translation: Translation) -> Self {
        Self { title, translation }
    }

    #[must_use]
    pub const fn title(&self) -> &TitlePlaybackRef {
        &self.title
    }

    #[must_use]
    pub const fn translation(&self) -> &Translation {
        &self.translation
    }

    pub fn movie_request(self) -> Result<PlaybackRequest, RezkaError> {
        if self.title.kind() != RezkaMediaKind::Movie {
            return Err(RezkaError::TranslationUnavailable {
                reason: ProviderFailureReason::TranslationUnavailable,
            });
        }
        Ok(PlaybackRequest::Movie(self))
    }
}

pub struct SeriesAvailability {
    selection: SelectedTranslation,
    seasons: Vec<SeasonAvailability>,
}

impl SeriesAvailability {
    pub(crate) fn new(selection: SelectedTranslation, seasons: Vec<SeasonAvailability>) -> Self {
        Self { selection, seasons }
    }

    #[must_use]
    pub fn seasons(&self) -> &[SeasonAvailability] {
        &self.seasons
    }

    pub fn select_episode(&self, season: u32, episode: u32) -> Result<SelectedEpisode, RezkaError> {
        let present = self.seasons.iter().any(|candidate| {
            candidate.number == season
                && candidate
                    .episodes
                    .iter()
                    .any(|candidate| candidate.number == episode)
        });
        if !present {
            return Err(RezkaError::EpisodeUnavailable {
                reason: ProviderFailureReason::EpisodeUnavailable,
            });
        }

        Ok(SelectedEpisode {
            selection: self.selection.clone(),
            season,
            episode,
        })
    }
}

impl fmt::Debug for SeriesAvailability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SeriesAvailability")
            .field("selection", &self.selection)
            .field("seasons", &self.seasons)
            .finish()
    }
}

#[derive(Debug)]
pub struct SeasonAvailability {
    number: u32,
    label: String,
    episodes: Vec<EpisodeAvailability>,
}

impl SeasonAvailability {
    pub(crate) fn new(number: u32, label: String, episodes: Vec<EpisodeAvailability>) -> Self {
        Self {
            number,
            label,
            episodes,
        }
    }

    #[must_use]
    pub const fn number(&self) -> u32 {
        self.number
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn episodes(&self) -> &[EpisodeAvailability] {
        &self.episodes
    }
}

#[derive(Debug)]
pub struct EpisodeAvailability {
    number: u32,
    label: String,
}

impl EpisodeAvailability {
    pub(crate) fn new(number: u32, label: String) -> Self {
        Self { number, label }
    }

    #[must_use]
    pub const fn number(&self) -> u32 {
        self.number
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Clone)]
pub struct SelectedEpisode {
    selection: SelectedTranslation,
    season: u32,
    episode: u32,
}

impl SelectedEpisode {
    #[must_use]
    pub fn playback_request(self) -> PlaybackRequest {
        PlaybackRequest::Episode(self)
    }
}

impl fmt::Debug for SelectedEpisode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedEpisode")
            .field("selection", &self.selection)
            .field("season", &self.season)
            .field("episode", &self.episode)
            .finish()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ResolvedTarget {
    Movie,
    Episode { season: u32, episode: u32 },
}

impl ResolvedTarget {
    #[must_use]
    pub const fn season_episode(self) -> Option<(u32, u32)> {
        match self {
            Self::Movie => None,
            Self::Episode { season, episode } => Some((season, episode)),
        }
    }
}

impl fmt::Debug for SelectedTranslation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedTranslation")
            .field("title", &self.title)
            .field("translation", &self.translation)
            .finish()
    }
}

pub enum PlaybackRequest {
    Movie(SelectedTranslation),
    Episode(SelectedEpisode),
}

impl PlaybackRequest {
    #[must_use]
    pub fn target(&self) -> ResolvedTarget {
        match self {
            Self::Movie(_) => ResolvedTarget::Movie,
            Self::Episode(selected) => ResolvedTarget::Episode {
                season: selected.season,
                episode: selected.episode,
            },
        }
    }

    #[must_use]
    pub fn translation_key(&self) -> &TranslationKey {
        match self {
            Self::Movie(selection) => selection.translation().key(),
            Self::Episode(selected) => selected.selection.translation().key(),
        }
    }
}

pub struct PlaybackManifest {
    title: TitlePlaybackRef,
    translation: TranslationKey,
    target: ResolvedTarget,
    variants: Vec<StreamVariant>,
    preferred_variant: usize,
    subtitles: Vec<SubtitleTrack>,
}

impl PlaybackManifest {
    pub(crate) fn new(
        title: TitlePlaybackRef,
        translation: TranslationKey,
        target: ResolvedTarget,
        variants: Vec<StreamVariant>,
        subtitles: Vec<SubtitleTrack>,
    ) -> Self {
        Self {
            title,
            translation,
            target,
            variants,
            preferred_variant: 0,
            subtitles,
        }
    }

    #[must_use]
    pub const fn title(&self) -> &TitlePlaybackRef {
        &self.title
    }

    #[must_use]
    pub const fn translation(&self) -> &TranslationKey {
        &self.translation
    }

    #[must_use]
    pub const fn target(&self) -> ResolvedTarget {
        self.target
    }

    #[must_use]
    pub fn variants(&self) -> &[StreamVariant] {
        &self.variants
    }

    #[must_use]
    pub const fn preferred_variant_index(&self) -> usize {
        self.preferred_variant
    }

    #[must_use]
    pub fn preferred_variant(&self) -> &StreamVariant {
        &self.variants[self.preferred_variant]
    }

    #[must_use]
    pub fn subtitles(&self) -> &[SubtitleTrack] {
        &self.subtitles
    }
}

impl fmt::Debug for PlaybackManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaybackManifest")
            .field("title", &self.title)
            .field("translation", &self.translation)
            .field("target", &self.target)
            .field("variants", &self.variants)
            .field("preferred_variant", &self.preferred_variant)
            .field("subtitles", &self.subtitles)
            .finish()
    }
}

impl fmt::Debug for PlaybackRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Movie(selection) => formatter.debug_tuple("Movie").field(selection).finish(),
            Self::Episode(selected) => formatter.debug_tuple("Episode").field(selected).finish(),
        }
    }
}

impl RezkaClient {
    pub async fn series_availability(
        &mut self,
        selection: &SelectedTranslation,
    ) -> Result<SeriesAvailability, RezkaError> {
        if selection.title().kind() != RezkaMediaKind::Series {
            return Err(RezkaError::TranslationUnavailable {
                reason: ProviderFailureReason::TranslationUnavailable,
            });
        }

        let origin = self.transport_mut().selected_origin().clone();
        let endpoint = origin
            .join(AJAX_PATH)
            .map_err(|_| invalid_playback("invalid playback endpoint"))?;
        let referer = origin
            .join(selection.title().locator().as_str())
            .map_err(|_| invalid_playback("invalid title referer"))?;
        let title_id = selection.title().id().get().to_string();
        let translation_id = selection.translation().id().get().to_string();
        let form = [
            ("id", title_id.as_str()),
            ("translator_id", translation_id.as_str()),
            ("action", "get_episodes"),
        ];
        let response = self
            .transport_mut()
            .post_form_with_failover(endpoint, Some(referer), &form)
            .await?;
        parser::parse_series_availability(&response.body, selection.clone())
    }

    pub async fn resolve(
        &mut self,
        request: PlaybackRequest,
    ) -> Result<PlaybackManifest, RezkaError> {
        let origin = self.transport_mut().selected_origin().clone();
        let endpoint = origin
            .join(AJAX_PATH)
            .map_err(|_| invalid_playback("invalid playback endpoint"))?;
        let title = match &request {
            PlaybackRequest::Movie(selection) => selection.title(),
            PlaybackRequest::Episode(selected) => selected.selection.title(),
        };
        let referer = origin
            .join(title.locator().as_str())
            .map_err(|_| invalid_playback("invalid title referer"))?;

        let owned_form = playback_form(&request);
        let form = owned_form
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect::<Vec<_>>();
        let response = self
            .transport_mut()
            .post_form_with_failover(endpoint, Some(referer), &form)
            .await?;
        parser::parse_playback_manifest(&response.body, request)
    }
}

fn playback_form(request: &PlaybackRequest) -> Vec<(&'static str, String)> {
    match request {
        PlaybackRequest::Movie(selection) => vec![
            ("id", selection.title().id().get().to_string()),
            (
                "translator_id",
                selection.translation().id().get().to_string(),
            ),
            (
                "is_camrip",
                u8::from(selection.translation().is_camrip()).to_string(),
            ),
            (
                "is_ads",
                u8::from(selection.translation().has_ads()).to_string(),
            ),
            (
                "is_director",
                u8::from(selection.translation().is_director()).to_string(),
            ),
            ("action", "get_movie".to_owned()),
        ],
        PlaybackRequest::Episode(selected) => vec![
            ("id", selected.selection.title().id().get().to_string()),
            (
                "translator_id",
                selected.selection.translation().id().get().to_string(),
            ),
            ("season", selected.season.to_string()),
            ("episode", selected.episode.to_string()),
            ("action", "get_stream".to_owned()),
        ],
    }
}

pub(crate) fn invalid_playback(reason: &'static str) -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: crate::redaction::trusted_internal_text(reason),
    }
}
