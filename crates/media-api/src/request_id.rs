use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};

pub(crate) const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
const MAX_REQUEST_ID_BYTES: usize = 128;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RequestId(String);

impl RequestId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) async fn assign(mut request: Request, next: Next) -> Response {
    let mut values = request.headers().get_all(&REQUEST_ID_HEADER).iter();
    let request_id = match (values.next(), values.next()) {
        (Some(value), None) => valid_request_id(value),
        _ => None,
    }
    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let request_id = RequestId(request_id);
    request.extensions_mut().insert(request_id.clone());

    let mut response = next.run(request).await;
    let header = HeaderValue::from_str(request_id.as_str())
        .expect("a validated or generated request ID is always a valid header value");
    response.headers_mut().insert(REQUEST_ID_HEADER, header);
    response
}

fn valid_request_id(value: &HeaderValue) -> Option<String> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_REQUEST_ID_BYTES
        || !bytes.iter().all(|byte| (0x21..=0x7e).contains(byte))
    {
        return None;
    }

    std::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::valid_request_id;

    #[test]
    fn accepts_only_nonempty_visible_ascii_within_limit() {
        assert_eq!(
            valid_request_id(&HeaderValue::from_static("request-123")),
            Some("request-123".to_owned()),
        );
        assert_eq!(
            valid_request_id(&HeaderValue::from_static("has space")),
            None
        );
        assert_eq!(
            valid_request_id(&HeaderValue::from_str(&"a".repeat(129)).unwrap()),
            None,
        );
    }
}
