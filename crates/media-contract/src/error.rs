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
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
    pub request_id: String,
}

#[cfg(test)]
mod tests {
    use super::{ApiError, ApiErrorCode};

    #[test]
    fn api_error_has_a_stable_public_shape() {
        let error = ApiError {
            code: ApiErrorCode::IdentityAmbiguous,
            message: "Episode numbering needs confirmation".to_owned(),
            request_id: "req-123".to_owned(),
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
}
