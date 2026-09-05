#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum NeedsActionReason {
    IdentityAmbiguous,
    PlexMismatch,
    NoMatchingEpisodes,
}
