use std::fmt;

use crate::{
    ProviderFailureReason, RezkaError,
    catalog::{RezkaMediaKind, RezkaTitleId, TitleLocator, Translation},
};

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
}

impl fmt::Debug for PlaybackRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Movie(selection) => formatter.debug_tuple("Movie").field(selection).finish(),
        }
    }
}
