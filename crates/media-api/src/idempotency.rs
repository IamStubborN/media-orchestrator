use std::future::Future;

use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use media_contract::ApiError as ErrorBody;
use media_core::{Actor, ClientId, OperationKey};
use sha2::{Digest, Sha256};

use crate::{ApiError, ApiState, MAX_REQUEST_BODY_BYTES, RequestId, request_id::REQUEST_ID_HEADER};

const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
const OPERATION_KEY_DOMAIN: &[u8] = b"media-orchestrator:operation-key:v1";
const MAX_STORED_RESPONSE_BYTES: usize = 1024 * 1024;

/// Identity and fingerprint of one authenticated write request.
#[derive(Clone, Eq, PartialEq)]
pub struct IdempotencyRequest {
    client_id: ClientId,
    key: String,
    fingerprint: [u8; 32],
}

impl std::fmt::Debug for IdempotencyRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IdempotencyRequest")
            .field("client_id", &self.client_id)
            .field("key", &"[REDACTED]")
            .field("fingerprint", &"[REDACTED]")
            .finish()
    }
}

impl IdempotencyRequest {
    #[must_use]
    pub fn new(client_id: ClientId, key: String, fingerprint: [u8; 32]) -> Self {
        Self {
            client_id,
            key,
            fingerprint,
        }
    }

    #[must_use]
    pub const fn client_id(&self) -> ClientId {
        self.client_id
    }

    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub const fn fingerprint(&self) -> &[u8; 32] {
        &self.fingerprint
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct IdempotencyGeneration(uuid::Uuid);

impl IdempotencyGeneration {
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(value: uuid::Uuid) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }
}

impl Default for IdempotencyGeneration {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for IdempotencyGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IdempotencyGeneration([REDACTED])")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct IdempotencyHandle {
    request: IdempotencyRequest,
    generation: IdempotencyGeneration,
}

impl IdempotencyHandle {
    #[must_use]
    pub const fn new(request: IdempotencyRequest, generation: IdempotencyGeneration) -> Self {
        Self {
            request,
            generation,
        }
    }

    #[must_use]
    pub const fn client_id(&self) -> ClientId {
        self.request.client_id()
    }

    #[must_use]
    pub fn key(&self) -> &str {
        self.request.key()
    }

    #[must_use]
    pub const fn fingerprint(&self) -> &[u8; 32] {
        self.request.fingerprint()
    }

    #[must_use]
    pub const fn generation(&self) -> IdempotencyGeneration {
        self.generation
    }
}

impl std::fmt::Debug for IdempotencyHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IdempotencyHandle")
            .field("request", &self.request)
            .field("generation", &self.generation)
            .finish()
    }
}

/// Buffered HTTP response persisted for byte-for-byte replay.
#[derive(Clone, Eq, PartialEq)]
pub struct StoredHttpResponse {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

impl std::fmt::Debug for StoredHttpResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredHttpResponse")
            .field("status", &self.status)
            .field("content_type", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl StoredHttpResponse {
    #[must_use]
    pub fn new(status: u16, content_type: String, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body,
        }
    }

    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Reservation {
    Reserved(IdempotencyHandle),
    Replay {
        handle: IdempotencyHandle,
        response: StoredHttpResponse,
    },
    Conflict,
    InProgress(IdempotencyHandle),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum IdempotencyError {
    #[error("idempotency storage conflict")]
    Conflict,
    #[error("idempotency storage failed")]
    Infrastructure,
}

/// HTTP-owned persistence port for exact idempotent response replay.
#[async_trait::async_trait]
pub trait IdempotencyStore: Send + Sync {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError>;

    async fn complete(
        &self,
        handle: &IdempotencyHandle,
        response: StoredHttpResponse,
    ) -> Result<(), IdempotencyError>;

    async fn abort_in_progress(&self, handle: &IdempotencyHandle) -> Result<(), IdempotencyError>;
}

/// API-owned read port for reconciling a stranded HTTP reservation.
#[async_trait::async_trait]
pub trait OperationCompletionStore: Send + Sync {
    async fn is_completed(&self, operation: OperationKey) -> Result<bool, IdempotencyError>;
}

pub(crate) async fn execute<F, Fut>(
    state: ApiState,
    actor: Actor,
    request_id: RequestId,
    request: Request,
    operation: F,
) -> Response
where
    F: FnOnce(ApiState, Actor, RequestId, OperationKey, Vec<u8>) -> Fut,
    Fut: Future<Output = Response>,
{
    let key = match idempotency_key(request.headers(), &request_id) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let method = request.method().as_str().as_bytes().to_vec();
    let path = request.uri().path().as_bytes().to_vec();
    let body = match to_bytes(request.into_body(), MAX_REQUEST_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return ApiError::invalid_body(&request_id).into_response(),
    };
    let fingerprint = fingerprint(actor.client_id(), &method, &path, &body);
    let idempotency_request = IdempotencyRequest::new(actor.client_id(), key, fingerprint);
    let operation_key = operation_key(&idempotency_request);

    let handle = match state.idempotency().reserve(idempotency_request).await {
        Ok(Reservation::Reserved(handle)) => handle,
        Ok(Reservation::Replay { response, .. }) => {
            return match replay(response, &request_id) {
                Some(response) => response,
                None => ApiError::internal(&request_id).into_response(),
            };
        }
        Ok(Reservation::Conflict) => {
            return ApiError::idempotency_conflict(&request_id).into_response();
        }
        Ok(Reservation::InProgress(handle)) => {
            match state.operations().is_completed(operation_key).await {
                Ok(true) => handle,
                Ok(false) => {
                    return ApiError::idempotency_in_progress(&request_id).into_response();
                }
                Err(_) => return ApiError::internal(&request_id).into_response(),
            }
        }
        Err(_) => return ApiError::internal(&request_id).into_response(),
    };

    let response = operation(
        state.clone(),
        actor,
        request_id.clone(),
        operation_key,
        body.to_vec(),
    )
    .await;
    let Some((stored, response)) = buffer(response, &request_id).await else {
        let _ = state.idempotency().abort_in_progress(&handle).await;
        return ApiError::internal(&request_id).into_response();
    };

    if !response.status().is_success() {
        // Only 2xx responses are durable idempotent outcomes. Any non-2xx (5xx, but also transient
        // 409/404) aborts the reservation so a client retrying the same key re-executes instead of
        // being pinned to a stale error forever.
        if state
            .idempotency()
            .abort_in_progress(&handle)
            .await
            .is_err()
        {
            return ApiError::internal(&request_id).into_response();
        }
        return response;
    }

    if state.idempotency().complete(&handle, stored).await.is_err() {
        let _ = state.idempotency().abort_in_progress(&handle).await;
        return ApiError::internal(&request_id).into_response();
    }

    response
}

fn idempotency_key(headers: &HeaderMap, request_id: &RequestId) -> Result<String, ApiError> {
    let mut values = headers.get_all(IDEMPOTENCY_KEY_HEADER).iter();
    let Some(value) = values.next() else {
        return Err(ApiError::missing_idempotency_key(request_id));
    };
    if values.next().is_some() {
        return Err(ApiError::invalid_request(
            request_id,
            "idempotency-key header is invalid",
        ));
    }
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || !bytes.iter().all(|byte| (0x21..=0x7e).contains(byte))
    {
        return Err(ApiError::invalid_request(
            request_id,
            "idempotency-key header is invalid",
        ));
    }

    Ok(std::str::from_utf8(bytes)
        .expect("visible ASCII is valid UTF-8")
        .to_owned())
}

fn fingerprint(client_id: ClientId, method: &[u8], path: &[u8], body: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for field in [
        client_id.as_uuid().as_bytes().as_slice(),
        method,
        path,
        body,
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    digest.finalize().into()
}

fn operation_key(request: &IdempotencyRequest) -> OperationKey {
    let mut digest = Sha256::new();
    for field in [
        OPERATION_KEY_DOMAIN,
        request.client_id().as_uuid().as_bytes().as_slice(),
        request.key().as_bytes(),
        request.fingerprint().as_slice(),
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    OperationKey::from_bytes(digest.finalize().into())
}

async fn buffer(
    response: Response,
    request_id: &RequestId,
) -> Option<(StoredHttpResponse, Response)> {
    let status = response.status();
    let content_type = match response.headers().get(header::CONTENT_TYPE) {
        Some(value) => value.to_str().ok()?.to_owned(),
        None => String::new(),
    };
    let body = to_bytes(response.into_body(), MAX_STORED_RESPONSE_BYTES)
        .await
        .ok()?
        .to_vec();
    let stored = StoredHttpResponse::new(status.as_u16(), content_type, body);
    let response = replay(stored.clone(), request_id)?;
    Some((stored, response))
}

fn replay(stored: StoredHttpResponse, fallback_request_id: &RequestId) -> Option<Response> {
    let status = StatusCode::from_u16(stored.status()).ok()?;
    let mut response = Response::new(Body::from(stored.body().to_vec()));
    *response.status_mut() = status;
    if !stored.content_type().is_empty() {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(stored.content_type()).ok()?,
        );
    }
    if status.is_client_error() || status.is_server_error() {
        let request_id = serde_json::from_slice::<ErrorBody>(stored.body())
            .ok()
            .map(|body| body.request_id)
            .unwrap_or_else(|| fallback_request_id.as_str().to_owned());
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, HeaderValue::from_str(&request_id).ok()?);
    }
    Some(response)
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, response::Response};
    use media_core::{ClientId, PRIMARY_CLIENT_ID};

    use crate::RequestId;

    use super::{
        IdempotencyError, IdempotencyRequest, IdempotencyStore, OperationCompletionStore,
        StoredHttpResponse, buffer, fingerprint, operation_key,
    };

    #[test]
    fn port_is_object_safe() {
        fn accept(_: Option<&dyn IdempotencyStore>) {}
        fn accept_operations(_: Option<&dyn OperationCompletionStore>) {}

        accept(None);
        accept_operations(None);
    }

    #[test]
    fn storage_conflicts_remain_distinct_from_infrastructure_failures() {
        assert_ne!(IdempotencyError::Conflict, IdempotencyError::Infrastructure);
        assert_eq!(
            IdempotencyError::Conflict.to_string(),
            "idempotency storage conflict"
        );
    }

    #[test]
    fn debug_output_redacts_header_and_body_material() {
        let request = IdempotencyRequest::new(
            PRIMARY_CLIENT_ID,
            "private-idempotency-key".to_owned(),
            [0xab; 32],
        );
        let response = StoredHttpResponse::new(
            201,
            "private/content-type".to_owned(),
            b"private-response-body".to_vec(),
        );

        let request_debug = format!("{request:?}");
        assert!(!request_debug.contains("private-idempotency-key"));
        assert!(!request_debug.contains("171"));

        let response_debug = format!("{response:?}");
        assert!(!response_debug.contains("private/content-type"));
        assert!(!response_debug.contains("private-response-body"));
    }

    #[test]
    fn fingerprint_separates_client_method_path_and_exact_body_bytes() {
        let other_client = ClientId::new();
        let base = fingerprint(PRIMARY_CLIENT_ID, b"POST", b"/v1/jobs", b"{\"a\":1}");

        assert_ne!(
            base,
            fingerprint(other_client, b"POST", b"/v1/jobs", b"{\"a\":1}")
        );
        assert_ne!(
            base,
            fingerprint(PRIMARY_CLIENT_ID, b"PUT", b"/v1/jobs", b"{\"a\":1}")
        );
        assert_ne!(
            base,
            fingerprint(PRIMARY_CLIENT_ID, b"POST", b"/v1/job", b"{\"a\":1}")
        );
        assert_ne!(
            base,
            fingerprint(PRIMARY_CLIENT_ID, b"POST", b"/v1/jobs", b"{ \"a\":1}")
        );
        assert_ne!(
            fingerprint(PRIMARY_CLIENT_ID, b"PO", b"ST/v1/jobs", b"body"),
            fingerprint(PRIMARY_CLIENT_ID, b"POST", b"/v1/jobs", b"body"),
            "length-prefixing must make field boundaries unambiguous",
        );
    }

    #[test]
    fn operation_key_is_deterministic_and_separates_every_authenticated_input() {
        let other_client = ClientId::new();
        let base = IdempotencyRequest::new(PRIMARY_CLIENT_ID, "private-key".to_owned(), [0x11; 32]);

        assert_eq!(operation_key(&base), operation_key(&base));
        assert_ne!(
            operation_key(&base),
            operation_key(&IdempotencyRequest::new(
                other_client,
                "private-key".to_owned(),
                [0x11; 32],
            )),
        );
        assert_ne!(
            operation_key(&base),
            operation_key(&IdempotencyRequest::new(
                PRIMARY_CLIENT_ID,
                "other-key".to_owned(),
                [0x11; 32],
            )),
        );
        assert_ne!(
            operation_key(&base),
            operation_key(&IdempotencyRequest::new(
                PRIMARY_CLIENT_ID,
                "private-key".to_owned(),
                [0x22; 32],
            )),
        );
        assert_eq!(
            format!("{:?}", operation_key(&base)),
            "OperationKey([REDACTED])"
        );
    }

    #[tokio::test]
    async fn buffered_response_accepts_exactly_one_mebibyte() {
        let response = Response::new(Body::from(vec![0x5a; 1024 * 1024]));

        let (stored, replayed) = buffer(response, &RequestId::for_test("one-mib"))
            .await
            .expect("the binding 1 MiB boundary must be accepted");

        assert_eq!(stored.body().len(), 1024 * 1024);
        assert_eq!(
            axum::body::to_bytes(replayed.into_body(), usize::MAX)
                .await
                .unwrap()
                .len(),
            1024 * 1024,
        );
    }

    #[tokio::test]
    async fn buffered_response_rejects_one_byte_over_one_mebibyte() {
        let response = Response::new(Body::from(vec![0x5a; 1024 * 1024 + 1]));

        assert!(
            buffer(response, &RequestId::for_test("over-one-mib"))
                .await
                .is_none()
        );
    }
}
