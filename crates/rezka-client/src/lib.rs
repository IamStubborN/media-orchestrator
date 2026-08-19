#![forbid(unsafe_code)]

pub mod catalog;
pub mod discovery;
pub mod error;
pub mod mirror;
pub mod playback;
pub mod quality;
pub mod redaction;
pub mod secret_url;
pub mod session;
pub mod subtitles;
pub mod trailer;
pub mod transport;

pub use catalog::{
    CatalogBrowse, CatalogCategory, CatalogContinuation, CatalogEntry, CatalogPage, CatalogQuery,
    CatalogSlug, CatalogSort, FranchiseTitle, RatingSource, RezkaMediaKind, RezkaTitleId,
    SeriesLifecycleStatus, TitleDetails, TitleLocator, TitleRating, Translation, TranslationId,
    TranslationKey,
};
pub use discovery::{PremiumStatus, QuickSearchEntry, QuickSearchQuery};
pub use error::{ProviderFailureReason, RezkaError, RezkaErrorCode};
pub use mirror::MirrorSet;
pub use playback::{
    EpisodeAvailability, PlaybackManifest, PlaybackRequest, ResolvedTarget, SeasonAvailability,
    SelectedEpisode, SelectedTranslation, SeriesAvailability, TitlePlaybackRef,
};
pub use quality::{
    AdvertisedQuality, QualityTier, StreamEndpoint, StreamKind, StreamVariant,
    parse_stream_variants,
};
pub use secret_url::{PublicImageUrl, SecretMediaUrl, SecretSubtitleUrl};
pub use session::{
    ProbeResponse, RezkaClient, RezkaClientConfig, SessionValidation, SessionValidationProbe,
    cookie::SessionSnapshot,
};
pub use subtitles::{SubtitleLanguage, SubtitleTrack, SubtitleTrackId, parse_subtitle_fields};
pub use trailer::Trailer;
