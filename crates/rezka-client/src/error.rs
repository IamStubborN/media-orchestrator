#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RezkaErrorCode {
    ChallengeRequired,
    ChallengeFailed,
    AnubisUnsupportedAlgorithm,
    AnubisExcessiveDifficulty,
    AnubisTimeout,
    AnubisRejected,
    AuthenticationRequired,
    AuthenticationFailed,
    ProviderResponseInvalid,
    TitleNotFound,
    TranslationUnavailable,
    EpisodeUnavailable,
    QualityUnavailable,
    StreamExpired,
    RateLimited,
    Transport,
    Configuration,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RezkaDiagnosticCategory {
    RezkaReachable,
    AnubisChallengeRequired,
    AnubisChallengeFailed,
    RezkaProviderRejected,
    RezkaParserInvalid,
    SessionStoreError,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProviderFailureReason {
    AuthenticationRequired,
    PremiumRequired,
    Restricted,
    TranslationUnavailable,
    EpisodeUnavailable,
    RateLimited,
    Unknown,
}

impl std::fmt::Display for ProviderFailureReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::AuthenticationRequired => "authentication required",
            Self::PremiumRequired => "premium required",
            Self::Restricted => "content restricted",
            Self::TranslationUnavailable => "translation unavailable",
            Self::EpisodeUnavailable => "episode unavailable",
            Self::RateLimited => "rate limited",
            Self::Unknown => "provider failure",
        };
        formatter.write_str(message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RezkaError {
    #[error("challenge required: {context}")]
    ChallengeRequired {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("challenge failed: {context}")]
    ChallengeFailed {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("Anubis algorithm is not supported")]
    AnubisUnsupportedAlgorithm {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("Anubis proof difficulty is excessive")]
    AnubisExcessiveDifficulty {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("Anubis proof timed out or was cancelled")]
    AnubisTimeout {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("Anubis solution was rejected")]
    AnubisRejected {
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
    #[error("title not found: {context}")]
    TitleNotFound {
        context: crate::redaction::SanitizedSnippet,
    },
    #[error("translation unavailable: {reason}")]
    TranslationUnavailable { reason: ProviderFailureReason },
    #[error("episode unavailable: {reason}")]
    EpisodeUnavailable { reason: ProviderFailureReason },
    #[error("quality unavailable: {reason}")]
    QualityUnavailable { reason: ProviderFailureReason },
    #[error("stream expired: {reason}")]
    StreamExpired { reason: ProviderFailureReason },
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
            Self::ChallengeRequired { .. } => RezkaErrorCode::ChallengeRequired,
            Self::ChallengeFailed { .. } => RezkaErrorCode::ChallengeFailed,
            Self::AnubisUnsupportedAlgorithm { .. } => RezkaErrorCode::AnubisUnsupportedAlgorithm,
            Self::AnubisExcessiveDifficulty { .. } => RezkaErrorCode::AnubisExcessiveDifficulty,
            Self::AnubisTimeout { .. } => RezkaErrorCode::AnubisTimeout,
            Self::AnubisRejected { .. } => RezkaErrorCode::AnubisRejected,
            Self::AuthenticationRequired { .. } => RezkaErrorCode::AuthenticationRequired,
            Self::AuthenticationFailed { .. } => RezkaErrorCode::AuthenticationFailed,
            Self::ProviderResponseInvalid { .. } => RezkaErrorCode::ProviderResponseInvalid,
            Self::TitleNotFound { .. } => RezkaErrorCode::TitleNotFound,
            Self::TranslationUnavailable { .. } => RezkaErrorCode::TranslationUnavailable,
            Self::EpisodeUnavailable { .. } => RezkaErrorCode::EpisodeUnavailable,
            Self::QualityUnavailable { .. } => RezkaErrorCode::QualityUnavailable,
            Self::StreamExpired { .. } => RezkaErrorCode::StreamExpired,
            Self::RateLimited { .. } => RezkaErrorCode::RateLimited,
            Self::Transport { .. } => RezkaErrorCode::Transport,
            Self::Configuration { .. } => RezkaErrorCode::Configuration,
        }
    }

    #[must_use]
    pub const fn diagnostic_category(&self) -> RezkaDiagnosticCategory {
        match self {
            Self::ChallengeRequired { .. } => RezkaDiagnosticCategory::AnubisChallengeRequired,
            Self::ChallengeFailed { .. }
            | Self::AnubisUnsupportedAlgorithm { .. }
            | Self::AnubisExcessiveDifficulty { .. }
            | Self::AnubisTimeout { .. }
            | Self::AnubisRejected { .. } => RezkaDiagnosticCategory::AnubisChallengeFailed,
            Self::AuthenticationRequired { .. }
            | Self::AuthenticationFailed { .. }
            | Self::TranslationUnavailable { .. }
            | Self::EpisodeUnavailable { .. }
            | Self::QualityUnavailable { .. }
            | Self::RateLimited { .. }
            | Self::TitleNotFound { .. } => RezkaDiagnosticCategory::RezkaProviderRejected,
            Self::ProviderResponseInvalid { .. } | Self::Configuration { .. } => {
                RezkaDiagnosticCategory::RezkaParserInvalid
            }
            Self::Transport { .. } => RezkaDiagnosticCategory::RezkaProviderRejected,
            Self::StreamExpired { .. } => RezkaDiagnosticCategory::RezkaProviderRejected,
        }
    }
}
