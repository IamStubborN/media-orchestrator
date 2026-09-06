#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    IdentityAmbiguous,
    PlexMismatch,
    MissingIdempotencyKey,
    IdempotencyConflict,
    IdempotencyInProgress,
    InvalidToken,
    LeaseNotFound,
    VpnRotationRequired,
    ProviderUnavailable,
    Internal,
}

/// Safe, provider-independent diagnostic categories exposed at delivery
/// boundaries.  Keep this enum intentionally small: provider response bodies,
/// challenge payloads, cookies and credentials must never cross the API.
#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum RezkaDiagnosticCategoryDto {
    RezkaReachable,
    AnubisChallengeRequired,
    AnubisChallengeFailed,
    RezkaProviderRejected,
    RezkaParserInvalid,
    SessionStoreError,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<RezkaDiagnosticCategoryDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ApiError, ApiErrorCode, RezkaDiagnosticCategoryDto};

    #[test]
    fn api_error_has_a_stable_public_shape() {
        let error = ApiError {
            code: ApiErrorCode::IdentityAmbiguous,
            message: "Episode numbering needs confirmation".to_owned(),
            request_id: "req-123".to_owned(),
            diagnostic: None,
            tracking_id: None,
        };

        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "code": "identity_ambiguous",
                "message": "Episode numbering needs confirmation",
                "request_id": "req-123"
            }),
        );
        assert_eq!(serde_json::from_value::<ApiError>(value).unwrap(), error);
    }

    #[test]
    fn rezka_diagnostic_has_the_exact_safe_public_category() {
        let error = ApiError {
            code: ApiErrorCode::ProviderUnavailable,
            message: "media provider diagnostic".to_owned(),
            request_id: "req-456".to_owned(),
            diagnostic: Some(RezkaDiagnosticCategoryDto::AnubisChallengeRequired),
            tracking_id: None,
        };

        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["diagnostic"], "AnubisChallengeRequired");
        assert_eq!(serde_json::from_value::<ApiError>(value).unwrap(), error);
    }

    #[test]
    fn api_error_codes_have_fixed_public_names() {
        let cases = [
            (ApiErrorCode::InvalidRequest, "invalid_request"),
            (ApiErrorCode::Unauthorized, "unauthorized"),
            (ApiErrorCode::Forbidden, "forbidden"),
            (ApiErrorCode::NotFound, "not_found"),
            (ApiErrorCode::Conflict, "conflict"),
            (ApiErrorCode::IdentityAmbiguous, "identity_ambiguous"),
            (ApiErrorCode::PlexMismatch, "plex_mismatch"),
            (
                ApiErrorCode::MissingIdempotencyKey,
                "missing_idempotency_key",
            ),
            (ApiErrorCode::IdempotencyConflict, "idempotency_conflict"),
            (
                ApiErrorCode::IdempotencyInProgress,
                "idempotency_in_progress",
            ),
            (ApiErrorCode::InvalidToken, "invalid_token"),
            (ApiErrorCode::LeaseNotFound, "lease_not_found"),
            (ApiErrorCode::VpnRotationRequired, "vpn_rotation_required"),
            (ApiErrorCode::ProviderUnavailable, "provider_unavailable"),
            (ApiErrorCode::Internal, "internal"),
        ];

        for (code, name) in cases {
            let json = format!("\"{name}\"");
            assert_eq!(serde_json::to_string(&code).unwrap(), json);
            assert_eq!(serde_json::from_str::<ApiErrorCode>(&json).unwrap(), code);
        }
    }
}
