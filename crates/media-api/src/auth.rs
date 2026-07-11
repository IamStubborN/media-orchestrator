use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Response},
};
use media_core::CredentialDigest;
use sha2::{Digest, Sha256};

use crate::{ApiError, ApiState, RequestId};

const MAX_BEARER_TOKEN_BYTES: usize = 512;

pub(crate) async fn authenticate(
    State(state): State<ApiState>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .expect("request-ID middleware must run outside authentication")
        .clone();
    let token = match bearer_token(&request) {
        Ok(token) => token,
        Err(BearerError::MissingOrMalformed) => {
            return ApiError::invalid_token(&request_id).into_response();
        }
        Err(BearerError::InvalidHeader | BearerError::TooLong) => {
            return ApiError::invalid_authorization_header(&request_id).into_response();
        }
    };
    let digest = CredentialDigest::from(<[u8; 32]>::from(Sha256::digest(token)));

    match state.clients.find_by_digest(digest).await {
        Ok(Some(actor)) => {
            request.extensions_mut().insert(actor);
            next.run(request).await
        }
        Ok(None) => ApiError::invalid_token(&request_id).into_response(),
        Err(_) => ApiError::internal(&request_id).into_response(),
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum BearerError {
    MissingOrMalformed,
    InvalidHeader,
    TooLong,
}

fn bearer_token(request: &Request) -> Result<&[u8], BearerError> {
    let mut values = request.headers().get_all(header::AUTHORIZATION).iter();
    let value = values
        .next()
        .ok_or(BearerError::MissingOrMalformed)?
        .as_bytes();
    if values.next().is_some() {
        return Err(BearerError::InvalidHeader);
    }
    let Some(separator) = value.iter().position(|byte| *byte == b' ') else {
        return Err(BearerError::MissingOrMalformed);
    };
    let (scheme, token_with_space) = value.split_at(separator);
    let token = &token_with_space[1..];
    if !scheme.eq_ignore_ascii_case(b"bearer")
        || token.is_empty()
        || !token.iter().all(|byte| (0x21..=0x7e).contains(byte))
    {
        return Err(BearerError::MissingOrMalformed);
    }
    if token.len() > MAX_BEARER_TOKEN_BYTES {
        return Err(BearerError::TooLong);
    }

    Ok(token)
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};

    use super::{BearerError, bearer_token};

    #[test]
    fn bearer_scheme_is_case_insensitive_without_copying_the_token() {
        let request = Request::builder()
            .header("authorization", "bEaReR opaque-token")
            .body(Body::empty())
            .unwrap();

        assert_eq!(bearer_token(&request), Ok(b"opaque-token".as_slice()));
    }

    #[test]
    fn malformed_credentials_share_one_result() {
        for value in ["Basic value", "Bearer", "Bearer ", "Bearer two values"] {
            let request = Request::builder()
                .header("authorization", value)
                .body(Body::empty())
                .unwrap();
            assert_eq!(bearer_token(&request), Err(BearerError::MissingOrMalformed),);
        }
    }

    #[test]
    fn rejects_ambiguous_or_non_visible_bearer_values() {
        let mut duplicate = Request::builder()
            .header("authorization", "Bearer first")
            .body(Body::empty())
            .unwrap();
        duplicate
            .headers_mut()
            .append("authorization", "Bearer second".parse().unwrap());
        assert_eq!(bearer_token(&duplicate), Err(BearerError::InvalidHeader));

        let tab = Request::builder()
            .header("authorization", "Bearer token\tpart")
            .body(Body::empty())
            .unwrap();
        assert_eq!(bearer_token(&tab), Err(BearerError::MissingOrMalformed));
    }

    #[test]
    fn enforces_the_bearer_length_boundary() {
        let accepted_value = format!("Bearer {}", "a".repeat(512));
        let accepted = Request::builder()
            .header("authorization", accepted_value)
            .body(Body::empty())
            .unwrap();
        assert_eq!(bearer_token(&accepted).unwrap().len(), 512);

        let rejected_value = format!("Bearer {}", "a".repeat(513));
        let rejected = Request::builder()
            .header("authorization", rejected_value)
            .body(Body::empty())
            .unwrap();
        assert_eq!(bearer_token(&rejected), Err(BearerError::TooLong));
    }
}
