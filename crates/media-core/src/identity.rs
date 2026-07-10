use std::collections::BTreeSet;

use crate::{EpisodeId, NeedsActionReason};

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
    pub namespace: ExternalNamespace,
    pub value: String,
    pub source: MappingSource,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SeriesOrdering {
    TmdbAired,
    TvdbAired,
    TvdbDvd,
    TvdbAbsolute,
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
    use crate::EpisodeId;

    use super::{EpisodeResolution, NeedsActionReason, resolve_episode_candidates};

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
}
