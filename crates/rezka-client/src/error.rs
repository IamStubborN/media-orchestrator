#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RezkaErrorCode {
    ChallengeRequired,
    ChallengeFailed,
    AuthenticationRequired,
    AuthenticationFailed,
    ProviderResponseInvalid,
    RateLimited,
    Transport,
    Configuration,
}

#[derive(Debug, thiserror::Error)]
pub enum RezkaError {
    #[error("challenge failed: {context}")]
    ChallengeFailed {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("authentication required: {context}")]
    AuthenticationRequired {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("authentication failed: {context}")]
    AuthenticationFailed {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("provider response invalid: {context}")]
    ProviderResponseInvalid {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("rate limited")]
    RateLimited { retry_after_seconds: Option<u64> },
    #[error("transport failed: {context}")]
    Transport {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("configuration invalid: {message}")]
    Configuration { message: &'static str },
}

impl RezkaError {
    #[must_use]
    pub const fn code(&self) -> RezkaErrorCode {
        match self {
            Self::ChallengeFailed { .. } => RezkaErrorCode::ChallengeFailed,
            Self::AuthenticationRequired { .. } => RezkaErrorCode::AuthenticationRequired,
            Self::AuthenticationFailed { .. } => RezkaErrorCode::AuthenticationFailed,
            Self::ProviderResponseInvalid { .. } => RezkaErrorCode::ProviderResponseInvalid,
            Self::RateLimited { .. } => RezkaErrorCode::RateLimited,
            Self::Transport { .. } => RezkaErrorCode::Transport,
            Self::Configuration { .. } => RezkaErrorCode::Configuration,
        }
    }
}
