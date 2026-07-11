use std::{collections::BTreeSet, error::Error as _, fmt, io, time::Duration as StdDuration};

use reqwest::{
    Client, Method, StatusCode,
    header::{
        CONTENT_LENGTH, COOKIE, HeaderName, HeaderValue, LOCATION, REFERER, RETRY_AFTER, SET_COOKIE,
    },
    redirect::Policy,
};

pub const MAX_PROVIDER_RESPONSE_BODY_BYTES: usize = 2 * 1024 * 1024;
const PROVIDER_RESPONSE_BODY_OVERFLOW_BYTES: usize = MAX_PROVIDER_RESPONSE_BODY_BYTES + 1;
const X_REQUESTED_WITH: HeaderName = HeaderName::from_static("x-requested-with");
const XML_HTTP_REQUEST: HeaderValue = HeaderValue::from_static("XMLHttpRequest");
const TITLE_ACCEPTED_TERMINAL_STATUSES: [StatusCode; 2] = [StatusCode::NOT_FOUND, StatusCode::GONE];
use time::Duration;
use url::Url;

use crate::{
    RezkaError,
    mirror::{MirrorSet, same_origin},
    redaction::{redact_url, sanitize_http_status, sanitize_provider_text},
    session::cookie::{SessionJar, SessionSnapshot},
};

pub struct TransportResponse {
    pub status: StatusCode,
    pub url: Url,
    pub body: String,
    pub location: Option<Url>,
    stored_cookie_names: BTreeSet<String>,
}

impl fmt::Debug for TransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("url", &redact_url(self.url.as_str()))
            .field(
                "location",
                &self
                    .location
                    .as_ref()
                    .map(|location| redact_url(location.as_str())),
            )
            .field("body", &"[REDACTED]")
            .field("stored_cookie_names", &self.stored_cookie_names)
            .finish()
    }
}

impl TransportResponse {
    #[must_use]
    pub fn stored_cookie_names(&self) -> &BTreeSet<String> {
        &self.stored_cookie_names
    }
}

pub struct Transport {
    client: Client,
    mirrors: MirrorSet,
    jar: SessionJar,
    max_retries: u8,
}

#[derive(Copy, Clone)]
enum ResponseStatusPolicy {
    Default,
    #[allow(dead_code, reason = "Task 4 is the first title caller")]
    Title,
}

impl ResponseStatusPolicy {
    fn accepts_terminal_status(self, status: StatusCode) -> bool {
        matches!(self, Self::Title) && TITLE_ACCEPTED_TERMINAL_STATUSES.contains(&status)
    }
}

impl Transport {
    pub fn new(
        mut mirrors: MirrorSet,
        mut jar: SessionJar,
        user_agent: String,
        request_timeout: Duration,
        max_retries: u8,
    ) -> Result<Self, RezkaError> {
        let request_timeout =
            StdDuration::try_from(request_timeout).map_err(|_| RezkaError::Configuration {
                message: "invalid request timeout",
            })?;
        if request_timeout.is_zero() {
            return Err(RezkaError::Configuration {
                message: "invalid request timeout",
            });
        }

        if jar.has_origin_binding() {
            if !mirrors.select_matching_origin(|origin| jar.is_bound_to(origin)) {
                return Err(RezkaError::Configuration {
                    message: "snapshot origin is not a configured Rezka mirror",
                });
            }
        } else {
            jar.bind_to(mirrors.selected_origin())?;
        }

        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(request_timeout)
            .user_agent(user_agent)
            .build()
            .map_err(|_| RezkaError::Configuration {
                message: "invalid transport configuration",
            })?;

        Ok(Self {
            client,
            mirrors,
            jar,
            max_retries,
        })
    }

    pub fn from_snapshot(
        mirrors: MirrorSet,
        snapshot: &SessionSnapshot,
        user_agent: String,
        request_timeout: Duration,
        max_retries: u8,
    ) -> Result<Self, RezkaError> {
        Self::new(
            mirrors,
            SessionJar::import(snapshot)?,
            user_agent,
            request_timeout,
            max_retries,
        )
    }

    #[must_use]
    pub fn selected_origin(&self) -> &Url {
        self.mirrors.selected_origin()
    }

    #[must_use]
    pub(crate) fn contains_mirror_origin(&self, candidate: &Url) -> bool {
        self.mirrors.contains_origin(candidate)
    }

    pub async fn get_first(
        &mut self,
        url: Url,
        referer: Option<Url>,
    ) -> Result<TransportResponse, RezkaError> {
        self.send_first(
            Method::GET,
            url,
            referer,
            None,
            ResponseStatusPolicy::Default,
        )
        .await
        .map_err(|failure| failure.error)
    }

    pub async fn get_first_with_failover(
        &mut self,
        url: Url,
        referer: Option<Url>,
    ) -> Result<TransportResponse, RezkaError> {
        self.get_first_with_failover_using_policy(url, referer, ResponseStatusPolicy::Default)
            .await
    }

    #[allow(dead_code, reason = "Task 4 is the first title caller")]
    pub(crate) async fn get_first_with_failover_accepting(
        &mut self,
        url: Url,
        referer: Option<Url>,
    ) -> Result<TransportResponse, RezkaError> {
        self.get_first_with_failover_using_policy(url, referer, ResponseStatusPolicy::Title)
            .await
    }

    async fn get_first_with_failover_using_policy(
        &mut self,
        url: Url,
        referer: Option<Url>,
        status_policy: ResponseStatusPolicy,
    ) -> Result<TransportResponse, RezkaError> {
        self.send_idempotent_with_failover(Method::GET, url, referer, None, status_policy)
            .await
    }

    pub(crate) async fn post_form_with_failover(
        &mut self,
        url: Url,
        referer: Option<Url>,
        form: &[(&str, &str)],
    ) -> Result<TransportResponse, RezkaError> {
        self.send_idempotent_with_failover(
            Method::POST,
            url,
            referer,
            Some(form),
            ResponseStatusPolicy::Default,
        )
        .await
    }

    async fn send_idempotent_with_failover(
        &mut self,
        method: Method,
        url: Url,
        referer: Option<Url>,
        form: Option<&[(&str, &str)]>,
        status_policy: ResponseStatusPolicy,
    ) -> Result<TransportResponse, RezkaError> {
        let max_attempts = usize::from(self.max_retries)
            .saturating_add(1)
            .min(self.mirrors.len());
        let original = url;
        let original_referer = referer;

        for attempt in 0..max_attempts {
            let attempt_url = self.mirrors.rewrite_to_selected(&original)?;
            let attempt_referer = original_referer
                .as_ref()
                .map(|referer| self.mirrors.rewrite_to_selected(referer))
                .transpose()?;

            match self
                .send_first(
                    method.clone(),
                    attempt_url,
                    attempt_referer,
                    form,
                    status_policy,
                )
                .await
            {
                Ok(response) => {
                    self.mirrors.promote_selected();
                    return Ok(response);
                }
                Err(failure) if failure.eligible && attempt + 1 < max_attempts => {
                    if !self.mirrors.select_next() {
                        self.mirrors.promote_selected();
                        return Err(failure.error);
                    }
                    self.jar = SessionJar::bound_empty(self.mirrors.selected_origin())?;
                }
                Err(failure) => {
                    self.mirrors.promote_selected();
                    return Err(failure.error);
                }
            }
        }

        self.mirrors.promote_selected();
        Err(transport_error())
    }

    pub async fn get_following(
        &mut self,
        url: Url,
        referer: Option<Url>,
        max_redirects: u8,
    ) -> Result<TransportResponse, RezkaError> {
        let mut current_url = url;
        let mut current_referer = referer;
        let mut followed = 0_u8;

        loop {
            let response = self
                .send_first(
                    Method::GET,
                    current_url,
                    current_referer,
                    None,
                    ResponseStatusPolicy::Default,
                )
                .await
                .map_err(|failure| failure.error)?;

            if !response.status.is_redirection() {
                return Ok(response);
            }
            let Some(location) = response.location.clone() else {
                return Err(invalid_response("redirect missing valid location"));
            };
            if followed >= max_redirects {
                return Err(invalid_response("redirect limit exceeded"));
            }

            followed += 1;
            current_referer = Some(response.url);
            current_url = location;
        }
    }

    pub async fn post_form_first(
        &mut self,
        url: Url,
        referer: Option<Url>,
        form: &[(&str, &str)],
    ) -> Result<TransportResponse, RezkaError> {
        self.send_first(
            Method::POST,
            url,
            referer,
            Some(form),
            ResponseStatusPolicy::Default,
        )
        .await
        .map_err(|failure| failure.error)
    }

    pub fn export_session(&self) -> Result<SessionSnapshot, RezkaError> {
        self.jar.export()
    }

    async fn send_first(
        &mut self,
        method: Method,
        url: Url,
        referer: Option<Url>,
        form: Option<&[(&str, &str)]>,
        status_policy: ResponseStatusPolicy,
    ) -> Result<TransportResponse, AttemptFailure> {
        self.guard_selected_origin(&url)
            .map_err(AttemptFailure::terminal)?;

        let mut request = self.client.request(method, url.clone());
        if let Some(referer) = referer {
            request = request.header(REFERER, referer.as_str());
        }
        if let Some(cookie) = self.jar.request_cookie_header(&url) {
            request = request.header(COOKIE, cookie);
        }
        if let Some(form) = form {
            request = request
                .header(X_REQUESTED_WITH, XML_HTTP_REQUEST)
                .form(form);
        }

        let response = request.send().await.map_err(|error| AttemptFailure {
            eligible: eligible_request_failure(&error),
            error: transport_error(),
        })?;
        self.process_response(response, status_policy).await
    }

    async fn process_response(
        &mut self,
        mut response: reqwest::Response,
        status_policy: ResponseStatusPolicy,
    ) -> Result<TransportResponse, AttemptFailure> {
        let status = response.status();
        let url = response.url().clone();
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| url.join(value).ok());
        let retry_after_seconds = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok());
        let cookie_headers = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .map(HeaderValue::as_bytes);
        let stored_cookie_names = self
            .jar
            .store_response_cookies_with_names(cookie_headers, &url)
            .map_err(AttemptFailure::terminal)?;

        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(AttemptFailure::terminal(RezkaError::RateLimited {
                retry_after_seconds,
            }));
        }
        if matches!(
            status,
            StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
        ) {
            return Err(AttemptFailure {
                error: transport_error(),
                eligible: true,
            });
        }
        if !(status.is_success()
            || status.is_redirection()
            || status_policy.accepts_terminal_status(status))
        {
            return Err(AttemptFailure::terminal(invalid_http_status(status, &url)));
        }

        let declared_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if declared_length.is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BODY_BYTES as u64) {
            return Err(AttemptFailure::terminal(oversized_response_body()));
        }

        let initial_capacity = declared_length
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(MAX_PROVIDER_RESPONSE_BODY_BYTES);
        let mut body = Vec::with_capacity(initial_capacity);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AttemptFailure::terminal(transport_error()))?
        {
            let cumulative_length = body
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| AttemptFailure::terminal(oversized_response_body()))?;
            if cumulative_length >= PROVIDER_RESPONSE_BODY_OVERFLOW_BYTES {
                return Err(AttemptFailure::terminal(oversized_response_body()));
            }
            body.extend_from_slice(&chunk);
        }
        let body = decode_provider_body(body);

        Ok(TransportResponse {
            status,
            url,
            body,
            location,
            stored_cookie_names,
        })
    }

    fn guard_selected_origin(&self, request_url: &Url) -> Result<(), RezkaError> {
        if !same_origin(self.mirrors.selected_origin(), request_url) {
            return Err(RezkaError::Configuration {
                message: "request URL is not the selected mirror origin",
            });
        }
        Ok(())
    }
}

fn decode_provider_body(body: Vec<u8>) -> String {
    String::from_utf8(body)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

struct AttemptFailure {
    error: RezkaError,
    eligible: bool,
}

impl AttemptFailure {
    fn terminal(error: RezkaError) -> Self {
        Self {
            error,
            eligible: false,
        }
    }
}

fn eligible_request_failure(error: &reqwest::Error) -> bool {
    if error.is_timeout() {
        return true;
    }
    if !error.is_connect() {
        return false;
    }

    let mut source = error.source();
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<io::Error>() {
            return matches!(
                io_error.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::NetworkUnreachable
                    | io::ErrorKind::HostUnreachable
                    | io::ErrorKind::NotConnected
                    | io::ErrorKind::AddrNotAvailable
                    | io::ErrorKind::TimedOut
            );
        }
        source = current.source();
    }
    false
}

fn transport_error() -> RezkaError {
    RezkaError::Transport {
        context: sanitize_provider_text("request transport failure"),
    }
}

fn invalid_response(reason: &str) -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text(reason),
    }
}

fn oversized_response_body() -> RezkaError {
    invalid_response("provider response body exceeds limit")
}

fn invalid_http_status(status: StatusCode, url: &Url) -> RezkaError {
    let url = redact_url(url.as_str());
    RezkaError::ProviderResponseInvalid {
        context: sanitize_http_status(status.as_u16(), &url),
    }
}

#[cfg(test)]
mod tests {
    use super::{Transport, decode_provider_body};
    use crate::{RezkaErrorCode, mirror::MirrorSet, session::cookie::SessionJar};
    use time::Duration;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[test]
    fn valid_utf8_body_reuses_the_original_allocation() {
        let body = b"valid provider body".to_vec();
        let original_pointer = body.as_ptr();

        let decoded = decode_provider_body(body);

        assert_eq!(decoded.as_ptr(), original_pointer);
    }

    #[test]
    fn invalid_utf8_body_is_decoded_lossily() {
        assert_eq!(decode_provider_body(vec![b'a', 0xff, b'b']), "a\u{fffd}b");
    }

    #[tokio::test]
    async fn title_status_policy_returns_404_and_410_after_storing_cookies_and_reading_bodies() {
        for status in [404, 410] {
            let server = MockServer::start().await;
            let origin = Url::parse(&server.uri()).unwrap();
            Mock::given(method("GET"))
                .and(path("/title"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("set-cookie", "title_status=opaque; Path=/")
                        .set_body_string("title-status-body"),
                )
                .expect(1)
                .mount(&server)
                .await;
            let mut transport = Transport::new(
                MirrorSet::new(vec![origin.clone()]).unwrap(),
                SessionJar::empty(),
                "media-orchestrator-test".to_owned(),
                Duration::seconds(2),
                0,
            )
            .unwrap();

            let response = transport
                .get_first_with_failover_accepting(origin.join("/title").unwrap(), None)
                .await
                .unwrap();

            assert_eq!(response.status.as_u16(), status);
            assert_eq!(response.body, "title-status-body");
            assert!(response.stored_cookie_names().contains("title_status"));
        }
    }

    #[tokio::test]
    async fn title_status_policy_keeps_rate_limits_and_eligible_upstream_statuses_precedent() {
        for status in [502, 503, 504] {
            let first = MockServer::start().await;
            let first_origin = Url::parse(&first.uri()).unwrap();
            Mock::given(method("GET"))
                .and(path("/title"))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&first)
                .await;
            let second = MockServer::start().await;
            let second_origin = Url::parse(&second.uri()).unwrap();
            Mock::given(method("GET"))
                .and(path("/title"))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&second)
                .await;
            let mut transport = Transport::new(
                MirrorSet::new(vec![first_origin.clone(), second_origin.clone()]).unwrap(),
                SessionJar::empty(),
                "media-orchestrator-test".to_owned(),
                Duration::seconds(2),
                1,
            )
            .unwrap();

            let response = transport
                .get_first_with_failover_accepting(first_origin.join("/title").unwrap(), None)
                .await
                .unwrap();
            assert_eq!(response.url, second_origin.join("/title").unwrap());
        }

        let rate_limited = MockServer::start().await;
        let rate_limited_origin = Url::parse(&rate_limited.uri()).unwrap();
        Mock::given(method("GET"))
            .and(path("/title"))
            .respond_with(ResponseTemplate::new(429))
            .expect(1)
            .mount(&rate_limited)
            .await;
        let mut transport = Transport::new(
            MirrorSet::new(vec![rate_limited_origin.clone()]).unwrap(),
            SessionJar::empty(),
            "media-orchestrator-test".to_owned(),
            Duration::seconds(2),
            0,
        )
        .unwrap();

        let error = transport
            .get_first_with_failover_accepting(rate_limited_origin.join("/title").unwrap(), None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), RezkaErrorCode::RateLimited);
    }
}
