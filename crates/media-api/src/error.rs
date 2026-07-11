use std::error::Error as _;

use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use media_contract::{ApiError as ErrorBody, ApiErrorCode};

use crate::{
    MAX_REQUEST_BODY_BYTES, MAX_REQUEST_HEADER_BYTES, MAX_REQUEST_HEADER_COUNT, RequestId,
    RequestTimeout,
};

pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}

impl ApiError {
    fn new(
        status: StatusCode,
        code: ApiErrorCode,
        message: &'static str,
        request_id: &RequestId,
    ) -> Self {
        Self {
            status,
            body: ErrorBody {
                code,
                message: message.to_owned(),
                request_id: request_id.as_str().to_owned(),
            },
        }
    }

    #[must_use]
    pub fn forbidden(request_id: &RequestId, message: &'static str) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ApiErrorCode::Forbidden,
            message,
            request_id,
        )
    }

    pub(crate) fn invalid_token(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ApiErrorCode::InvalidToken,
            "authentication failed",
            request_id,
        )
    }

    pub(crate) fn invalid_authorization_header(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::InvalidRequest,
            "authorization header is invalid",
            request_id,
        )
    }

    pub(crate) fn payload_too_large(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ApiErrorCode::InvalidRequest,
            "request body is too large",
            request_id,
        )
    }

    pub(crate) fn invalid_content_length(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::InvalidRequest,
            "content-length header is invalid",
            request_id,
        )
    }

    pub(crate) fn request_headers_too_large(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::InvalidRequest,
            "request headers are too large",
            request_id,
        )
    }

    pub(crate) fn invalid_body(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::InvalidRequest,
            "request body could not be read",
            request_id,
        )
    }

    pub(crate) fn request_timeout(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::REQUEST_TIMEOUT,
            ApiErrorCode::Internal,
            "request timed out",
            request_id,
        )
    }

    pub(crate) fn invalid_request(request_id: &RequestId, message: &'static str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::InvalidRequest,
            message,
            request_id,
        )
    }

    pub(crate) fn missing_idempotency_key(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::MissingIdempotencyKey,
            "idempotency-key header is required",
            request_id,
        )
    }

    pub(crate) fn idempotency_conflict(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            ApiErrorCode::IdempotencyConflict,
            "idempotency key was reused for a different request",
            request_id,
        )
    }

    pub(crate) fn idempotency_in_progress(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            ApiErrorCode::IdempotencyInProgress,
            "an identical request is already in progress",
            request_id,
        )
    }

    pub(crate) fn not_found(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            ApiErrorCode::NotFound,
            "resource was not found",
            request_id,
        )
    }

    pub(crate) fn lease_not_found(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            ApiErrorCode::LeaseNotFound,
            "lease was not found",
            request_id,
        )
    }

    pub(crate) fn conflict(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            ApiErrorCode::Conflict,
            "operation conflicts with current state",
            request_id,
        )
    }

    pub(crate) fn not_ready(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::Internal,
            "service is not ready",
            request_id,
        )
    }

    pub(crate) fn internal(request_id: &RequestId) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiErrorCode::Internal,
            "internal server error",
            request_id,
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

pub(crate) async fn enforce_request_timeout(
    State(timeout): State<RequestTimeout>,
    request: Request,
    next: Next,
) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .expect("request-ID middleware must run outside request timeout")
        .clone();
    match tokio::time::timeout(timeout.0, next.run(request)).await {
        Ok(response) => response,
        Err(_) => ApiError::request_timeout(&request_id).into_response(),
    }
}

pub(crate) async fn enforce_request_limits(request: Request, next: Next) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .expect("request-ID middleware must run outside request limits")
        .clone();
    if !headers_within_budget(request.headers()) {
        return ApiError::request_headers_too_large(&request_id).into_response();
    }
    if let Some(content_length) = content_length(request.headers()) {
        let Ok(content_length) = content_length else {
            return ApiError::invalid_content_length(&request_id).into_response();
        };
        if content_length > MAX_REQUEST_BODY_BYTES as u64 {
            return ApiError::payload_too_large(&request_id).into_response();
        }
    }

    // This is the sole bounded collection point: unknown-length streams are
    // validated before auth and downstream code never sees an unchecked body.
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(body) => body,
        Err(error)
            if error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>()) =>
        {
            return ApiError::payload_too_large(&request_id).into_response();
        }
        Err(_) => return ApiError::invalid_body(&request_id).into_response(),
    };

    next.run(Request::from_parts(parts, Body::from(body))).await
}

fn headers_within_budget(headers: &HeaderMap) -> bool {
    if headers.len() > MAX_REQUEST_HEADER_COUNT {
        return false;
    }

    headers
        .iter()
        .try_fold(0_usize, |total, (name, value)| {
            // `: ` and CRLF approximate each serialized HTTP header line.
            total
                .checked_add(name.as_str().len())?
                .checked_add(value.as_bytes().len())?
                .checked_add(4)
        })
        .is_some_and(|total| total <= MAX_REQUEST_HEADER_BYTES)
}

fn content_length(headers: &HeaderMap) -> Option<Result<u64, ()>> {
    let mut values = headers.get_all(header::CONTENT_LENGTH).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return Some(Err(()));
    }

    Some(
        value
            .to_str()
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(()),
    )
}
