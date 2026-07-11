use std::path::PathBuf;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PlexExpectation {
    pub path: PathBuf,
    pub canonical_id: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PlexObservation {
    pub path: PathBuf,
    pub canonical_id: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("Plex item does not match the expected publication")]
pub struct PlexMismatch;

pub fn validate_plex_observation(
    expected: &PlexExpectation,
    observed: &PlexObservation,
) -> Result<(), PlexMismatch> {
    if expected.path != observed.path
        || expected.canonical_id != observed.canonical_id
        || expected.season != observed.season
        || expected.episode != observed.episode
    {
        return Err(PlexMismatch);
    }
    Ok(())
}
