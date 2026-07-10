use media_core::ClientId;

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
    Reserved,
    Replay(StoredHttpResponse),
    Conflict,
    InProgress,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum IdempotencyError {
    #[error("idempotency storage failed")]
    Infrastructure,
}

/// HTTP-owned persistence port for exact idempotent response replay.
#[async_trait::async_trait]
pub trait IdempotencyStore: Send + Sync {
    async fn reserve(&self, request: IdempotencyRequest) -> Result<Reservation, IdempotencyError>;

    async fn complete(
        &self,
        request: IdempotencyRequest,
        response: StoredHttpResponse,
    ) -> Result<(), IdempotencyError>;

    async fn abort(&self, request: IdempotencyRequest) -> Result<(), IdempotencyError>;
}

#[cfg(test)]
mod tests {
    use media_core::PRIMARY_CLIENT_ID;

    use super::{IdempotencyRequest, IdempotencyStore, StoredHttpResponse};

    #[test]
    fn port_is_object_safe() {
        fn accept(_: Option<&dyn IdempotencyStore>) {}

        accept(None);
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
}
