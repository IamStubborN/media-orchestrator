use std::collections::BTreeSet;

use crate::{EpisodeId, MediaId, NeedsActionReason, Provider, SeasonId};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ExternalNamespace {
    Tmdb,
    Tvdb,
    Imdb,
    AniList,
    Rezka,
    Plex,
    ProwlarrResult,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum MappingSource {
    Discovered,
    ConfirmedByUser,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExternalReference {
    namespace: ExternalNamespace,
    value: String,
    source: MappingSource,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SeriesOrdering {
    TmdbAired,
    TvdbAired,
    TvdbDvd,
    TvdbAbsolute,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MediaKind {
    Movie,
    Series,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum IdentityValidationError {
    #[error("title cannot be empty")]
    EmptyTitle,
    #[error("release year must be between 1878 and 9999")]
    InvalidReleaseYear,
    #[error("series media requires an ordering policy")]
    SeriesOrderingRequired,
    #[error("movie media cannot have a series ordering policy")]
    MovieOrderingForbidden,
    #[error("external reference cannot be empty")]
    EmptyExternalReference,
    #[error("provider media reference cannot be empty")]
    EmptyProviderMediaReference,
    #[error("media number exceeds the supported persistence range")]
    NumberOutOfRange,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CanonicalMedia {
    id: MediaId,
    kind: MediaKind,
    title: String,
    release_year: Option<i32>,
    ordering: Option<SeriesOrdering>,
}

impl CanonicalMedia {
    pub fn new(
        id: MediaId,
        kind: MediaKind,
        title: String,
        release_year: Option<i32>,
        ordering: Option<SeriesOrdering>,
    ) -> Result<Self, IdentityValidationError> {
        let title = normalized_required(title, IdentityValidationError::EmptyTitle)?;
        if release_year.is_some_and(|year| !(1878..=9999).contains(&year)) {
            return Err(IdentityValidationError::InvalidReleaseYear);
        }
        match (kind, ordering) {
            (MediaKind::Series, None) => {
                return Err(IdentityValidationError::SeriesOrderingRequired);
            }
            (MediaKind::Movie, Some(_)) => {
                return Err(IdentityValidationError::MovieOrderingForbidden);
            }
            _ => {}
        }

        Ok(Self {
            id,
            kind,
            title,
            release_year,
            ordering,
        })
    }

    #[must_use]
    pub const fn id(&self) -> MediaId {
        self.id
    }

    #[must_use]
    pub const fn kind(&self) -> MediaKind {
        self.kind
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub const fn release_year(&self) -> Option<i32> {
        self.release_year
    }

    #[must_use]
    pub const fn ordering(&self) -> Option<SeriesOrdering> {
        self.ordering
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CanonicalSeason {
    id: SeasonId,
    media_id: MediaId,
    season_number: u32,
    title: Option<String>,
}

impl CanonicalSeason {
    pub fn new(
        id: SeasonId,
        media_id: MediaId,
        season_number: u32,
        title: Option<String>,
    ) -> Result<Self, IdentityValidationError> {
        validate_number(season_number)?;
        Ok(Self {
            id,
            media_id,
            season_number,
            title: normalized_optional(title)?,
        })
    }

    #[must_use]
    pub const fn id(&self) -> SeasonId {
        self.id
    }

    #[must_use]
    pub const fn media_id(&self) -> MediaId {
        self.media_id
    }

    #[must_use]
    pub const fn season_number(&self) -> u32 {
        self.season_number
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CanonicalEpisode {
    id: EpisodeId,
    season_id: SeasonId,
    episode_number: u32,
    absolute_number: Option<u32>,
    title: Option<String>,
}

impl CanonicalEpisode {
    pub fn new(
        id: EpisodeId,
        season_id: SeasonId,
        episode_number: u32,
        absolute_number: Option<u32>,
        title: Option<String>,
    ) -> Result<Self, IdentityValidationError> {
        validate_positive_number(episode_number)?;
        if let Some(number) = absolute_number {
            validate_positive_number(number)?;
        }
        Ok(Self {
            id,
            season_id,
            episode_number,
            absolute_number,
            title: normalized_optional(title)?,
        })
    }

    #[must_use]
    pub const fn id(&self) -> EpisodeId {
        self.id
    }

    #[must_use]
    pub const fn season_id(&self) -> SeasonId {
        self.season_id
    }

    #[must_use]
    pub const fn episode_number(&self) -> u32 {
        self.episode_number
    }

    #[must_use]
    pub const fn absolute_number(&self) -> Option<u32> {
        self.absolute_number
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}

impl ExternalReference {
    pub fn new(
        namespace: ExternalNamespace,
        value: String,
        source: MappingSource,
    ) -> Result<Self, IdentityValidationError> {
        Ok(Self {
            namespace,
            value: normalized_required(value, IdentityValidationError::EmptyExternalReference)?,
            source,
        })
    }

    #[must_use]
    pub const fn namespace(&self) -> ExternalNamespace {
        self.namespace
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub const fn source(&self) -> MappingSource {
        self.source
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaExternalReference {
    media_id: MediaId,
    reference: ExternalReference,
}

impl MediaExternalReference {
    #[must_use]
    pub const fn new(media_id: MediaId, reference: ExternalReference) -> Self {
        Self {
            media_id,
            reference,
        }
    }

    #[must_use]
    pub const fn media_id(&self) -> MediaId {
        self.media_id
    }

    #[must_use]
    pub const fn reference(&self) -> &ExternalReference {
        &self.reference
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EpisodeProviderMapping {
    episode_id: EpisodeId,
    provider: Provider,
    provider_media_ref: String,
    provider_season_number: u32,
    provider_episode_number: u32,
    source: MappingSource,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CanonicalEpisodeCoordinates {
    episode_id: EpisodeId,
    season: u32,
    episode: u32,
    media_title: String,
}

impl CanonicalEpisodeCoordinates {
    #[must_use]
    pub fn new(episode_id: EpisodeId, season: u32, episode: u32, media_title: String) -> Self {
        Self {
            episode_id,
            season,
            episode,
            media_title,
        }
    }

    #[must_use]
    pub const fn episode_id(&self) -> EpisodeId {
        self.episode_id
    }

    #[must_use]
    pub const fn season(&self) -> u32 {
        self.season
    }

    #[must_use]
    pub const fn episode(&self) -> u32 {
        self.episode
    }

    #[must_use]
    pub fn media_title(&self) -> &str {
        &self.media_title
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EpisodeMappingConfirmation {
    provider: Provider,
    provider_media_ref: String,
    provider_season: u32,
    provider_episode: u32,
    title: String,
    release_year: Option<i32>,
    canonical_season: u32,
    canonical_episode: u32,
}

impl EpisodeMappingConfirmation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Provider,
        provider_media_ref: String,
        provider_season: u32,
        provider_episode: u32,
        title: String,
        release_year: Option<i32>,
        canonical_season: u32,
        canonical_episode: u32,
    ) -> Result<Self, IdentityValidationError> {
        let provider_media_ref = normalized_required(
            provider_media_ref,
            IdentityValidationError::EmptyProviderMediaReference,
        )?;
        let title = normalized_required(title, IdentityValidationError::EmptyTitle)?;
        validate_number(provider_season)?;
        validate_positive_number(provider_episode)?;
        validate_number(canonical_season)?;
        validate_positive_number(canonical_episode)?;
        if release_year.is_some_and(|year| !(1878..=9999).contains(&year)) {
            return Err(IdentityValidationError::InvalidReleaseYear);
        }
        Ok(Self {
            provider,
            provider_media_ref,
            provider_season,
            provider_episode,
            title,
            release_year,
            canonical_season,
            canonical_episode,
        })
    }

    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }

    #[must_use]
    pub fn provider_media_ref(&self) -> &str {
        &self.provider_media_ref
    }

    #[must_use]
    pub const fn provider_season(&self) -> u32 {
        self.provider_season
    }

    #[must_use]
    pub const fn provider_episode(&self) -> u32 {
        self.provider_episode
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub const fn release_year(&self) -> Option<i32> {
        self.release_year
    }

    #[must_use]
    pub const fn canonical_season(&self) -> u32 {
        self.canonical_season
    }

    #[must_use]
    pub const fn canonical_episode(&self) -> u32 {
        self.canonical_episode
    }
}

impl EpisodeProviderMapping {
    pub fn new(
        episode_id: EpisodeId,
        provider: Provider,
        provider_media_ref: String,
        provider_season_number: u32,
        provider_episode_number: u32,
        source: MappingSource,
    ) -> Result<Self, IdentityValidationError> {
        validate_number(provider_season_number)?;
        validate_positive_number(provider_episode_number)?;
        Ok(Self {
            episode_id,
            provider,
            provider_media_ref: normalized_required(
                provider_media_ref,
                IdentityValidationError::EmptyProviderMediaReference,
            )?,
            provider_season_number,
            provider_episode_number,
            source,
        })
    }

    #[must_use]
    pub const fn episode_id(&self) -> EpisodeId {
        self.episode_id
    }

    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }

    #[must_use]
    pub fn provider_media_ref(&self) -> &str {
        &self.provider_media_ref
    }

    #[must_use]
    pub const fn provider_season_number(&self) -> u32 {
        self.provider_season_number
    }

    #[must_use]
    pub const fn provider_episode_number(&self) -> u32 {
        self.provider_episode_number
    }

    #[must_use]
    pub const fn source(&self) -> MappingSource {
        self.source
    }
}

fn normalized_required(
    value: String,
    error: IdentityValidationError,
) -> Result<String, IdentityValidationError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        Err(error)
    } else {
        Ok(value)
    }
}

fn normalized_optional(value: Option<String>) -> Result<Option<String>, IdentityValidationError> {
    value
        .map(|value| normalized_required(value, IdentityValidationError::EmptyTitle))
        .transpose()
}

fn validate_number(value: u32) -> Result<(), IdentityValidationError> {
    i32::try_from(value)
        .map(|_| ())
        .map_err(|_| IdentityValidationError::NumberOutOfRange)
}

fn validate_positive_number(value: u32) -> Result<(), IdentityValidationError> {
    if value == 0 {
        return Err(IdentityValidationError::NumberOutOfRange);
    }
    validate_number(value)
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum EpisodeResolution {
    Resolved(EpisodeId),
    NeedsAction(NeedsActionReason),
}

#[must_use]
pub fn resolve_episode_candidates(candidates: &[EpisodeId]) -> EpisodeResolution {
    let unique: BTreeSet<_> = candidates.iter().copied().collect();

    match (unique.len(), unique.first()) {
        (1, Some(episode)) => EpisodeResolution::Resolved(*episode),
        _ => EpisodeResolution::NeedsAction(NeedsActionReason::IdentityAmbiguous),
    }
}

#[cfg(test)]
mod tests {
    use crate::{EpisodeId, MediaId, Provider, SeasonId};

    use super::{
        CanonicalEpisode, CanonicalMedia, CanonicalSeason, EpisodeProviderMapping,
        EpisodeResolution, ExternalNamespace, ExternalReference, IdentityValidationError,
        MappingSource, MediaExternalReference, MediaKind, NeedsActionReason, SeriesOrdering,
        resolve_episode_candidates,
    };

    #[test]
    fn one_unique_candidate_resolves() {
        let episode = EpisodeId::new();
        assert_eq!(
            resolve_episode_candidates(&[episode, episode]),
            EpisodeResolution::Resolved(episode),
        );
    }

    #[test]
    fn no_candidate_needs_user_action() {
        assert_eq!(
            resolve_episode_candidates(&[]),
            EpisodeResolution::NeedsAction(NeedsActionReason::IdentityAmbiguous),
        );
    }

    #[test]
    fn conflicting_candidates_need_user_action() {
        assert_eq!(
            resolve_episode_candidates(&[EpisodeId::new(), EpisodeId::new()]),
            EpisodeResolution::NeedsAction(NeedsActionReason::IdentityAmbiguous),
        );
    }

    #[test]
    fn canonical_media_enforces_kind_ordering_and_title_invariants() {
        assert_eq!(
            CanonicalMedia::new(
                MediaId::new(),
                MediaKind::Series,
                "Series".to_owned(),
                Some(2026),
                None,
            )
            .unwrap_err(),
            IdentityValidationError::SeriesOrderingRequired,
        );
        assert_eq!(
            CanonicalMedia::new(
                MediaId::new(),
                MediaKind::Movie,
                "Movie".to_owned(),
                Some(2026),
                Some(SeriesOrdering::TmdbAired),
            )
            .unwrap_err(),
            IdentityValidationError::MovieOrderingForbidden,
        );
        assert_eq!(
            CanonicalMedia::new(
                MediaId::new(),
                MediaKind::Movie,
                " \t".to_owned(),
                None,
                None,
            )
            .unwrap_err(),
            IdentityValidationError::EmptyTitle,
        );
    }

    #[test]
    fn canonical_numbers_must_fit_the_persistence_contract() {
        let too_large = (i32::MAX as u32) + 1;

        assert_eq!(
            CanonicalSeason::new(SeasonId::new(), MediaId::new(), too_large, None).unwrap_err(),
            IdentityValidationError::NumberOutOfRange,
        );
        assert_eq!(
            CanonicalEpisode::new(EpisodeId::new(), SeasonId::new(), 0, None, None).unwrap_err(),
            IdentityValidationError::NumberOutOfRange,
        );
        assert_eq!(
            EpisodeProviderMapping::new(
                EpisodeId::new(),
                Provider::Rezka,
                "provider-ref".to_owned(),
                0,
                0,
                MappingSource::Discovered,
            )
            .unwrap_err(),
            IdentityValidationError::NumberOutOfRange,
        );
        assert_eq!(
            CanonicalEpisode::new(EpisodeId::new(), SeasonId::new(), 1, Some(too_large), None,)
                .unwrap_err(),
            IdentityValidationError::NumberOutOfRange,
        );
        assert_eq!(
            EpisodeProviderMapping::new(
                EpisodeId::new(),
                Provider::Rezka,
                "provider-ref".to_owned(),
                too_large,
                1,
                MappingSource::Discovered,
            )
            .unwrap_err(),
            IdentityValidationError::NumberOutOfRange,
        );
    }

    #[test]
    fn canonical_identity_records_are_validated_and_immutable() {
        let media_id = MediaId::new();
        let media = CanonicalMedia::new(
            media_id,
            MediaKind::Series,
            "  Example Series  ".to_owned(),
            Some(2026),
            Some(SeriesOrdering::TvdbAired),
        )
        .unwrap();
        let season = CanonicalSeason::new(
            SeasonId::new(),
            media_id,
            1,
            Some(" Season One ".to_owned()),
        )
        .unwrap();
        let episode = CanonicalEpisode::new(
            EpisodeId::new(),
            season.id(),
            2,
            Some(14),
            Some(" Episode Two ".to_owned()),
        )
        .unwrap();

        assert_eq!(media.title(), "Example Series");
        assert_eq!(media.ordering(), Some(SeriesOrdering::TvdbAired));
        assert_eq!(season.title(), Some("Season One"));
        assert_eq!(episode.episode_number(), 2);
        assert_eq!(episode.absolute_number(), Some(14));
        assert_eq!(episode.title(), Some("Episode Two"));
    }

    #[test]
    fn external_references_and_episode_mappings_reject_blank_provider_values() {
        assert_eq!(
            ExternalReference::new(
                ExternalNamespace::Rezka,
                "  ".to_owned(),
                MappingSource::Discovered,
            )
            .unwrap_err(),
            IdentityValidationError::EmptyExternalReference,
        );
        assert_eq!(
            EpisodeProviderMapping::new(
                EpisodeId::new(),
                Provider::Rezka,
                "\n".to_owned(),
                1,
                2,
                MappingSource::ConfirmedByUser,
            )
            .unwrap_err(),
            IdentityValidationError::EmptyProviderMediaReference,
        );
    }

    #[test]
    fn external_reference_is_bound_to_canonical_media_explicitly() {
        let media_id = MediaId::new();
        let reference = ExternalReference::new(
            ExternalNamespace::Tmdb,
            " 12345 ".to_owned(),
            MappingSource::ConfirmedByUser,
        )
        .unwrap();
        let linked = MediaExternalReference::new(media_id, reference);

        assert_eq!(linked.media_id(), media_id);
        assert_eq!(linked.reference().value(), "12345");
    }
}
