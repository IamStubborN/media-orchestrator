use axum::{
    Json,
    extract::Request,
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use media_contract::{ApiError as ErrorBody, ApiErrorCode};

use crate::{MAX_REQUEST_BODY_BYTES, RequestId};

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

pub(crate) async fn enforce_request_limits(request: Request, next: Next) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .expect("request-ID middleware must run outside request limits")
        .clone();
    if let Some(content_length) = request.headers().get(header::CONTENT_LENGTH) {
        let Some(content_length) = content_length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return ApiError::invalid_content_length(&request_id).into_response();
        };
        if content_length > MAX_REQUEST_BODY_BYTES as u64 {
            return ApiError::payload_too_large(&request_id).into_response();
        }
    }

    let response = next.run(request).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::payload_too_large(&request_id).into_response();
    }
    response
}
