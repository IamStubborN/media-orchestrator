#![forbid(unsafe_code)]

pub mod catalog;
pub mod error;
pub mod mirror;
pub mod playback;
pub mod redaction;
pub mod secret_url;
pub mod session;
pub mod transport;

pub use catalog::{
    CatalogContinuation, CatalogEntry, CatalogPage, CatalogQuery, RezkaMediaKind, RezkaTitleId,
    TitleDetails, TitleLocator, Translation, TranslationId, TranslationKey,
};
pub use error::{ProviderFailureReason, RezkaError, RezkaErrorCode};
pub use mirror::MirrorSet;
pub use playback::{PlaybackRequest, SelectedTranslation, TitlePlaybackRef};
pub use secret_url::{PublicImageUrl, SecretMediaUrl, SecretSubtitleUrl};
pub use session::{
    ProbeResponse, RezkaClient, RezkaClientConfig, RezkaCredentials, SessionValidation,
    SessionValidationProbe, cookie::SessionSnapshot,
};
