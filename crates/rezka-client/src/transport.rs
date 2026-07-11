use std::{collections::BTreeSet, error::Error as _, fmt, io, time::Duration as StdDuration};

use reqwest::{
    Client, Method, StatusCode,
    header::{COOKIE, HeaderName, HeaderValue, LOCATION, REFERER, RETRY_AFTER, SET_COOKIE},
    redirect::Policy,
};

const X_REQUESTED_WITH: HeaderName = HeaderName::from_static("x-requested-with");
const XML_HTTP_REQUEST: HeaderValue = HeaderValue::from_static("XMLHttpRequest");
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

impl Transport {
    pub fn new(
        mirrors: MirrorSet,
        jar: SessionJar,
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
        self.send_first(Method::GET, url, referer, None)
            .await
            .map_err(|failure| failure.error)
    }

    pub async fn get_first_with_failover(
        &mut self,
        url: Url,
        referer: Option<Url>,
    ) -> Result<TransportResponse, RezkaError> {
        let max_attempts = usize::from(self.max_retries)
            .saturating_add(1)
            .min(self.mirrors.len());
        let original = url;

        for attempt in 0..max_attempts {
            let attempt_url = self.mirrors.rewrite_to_selected(&original)?;

            match self
                .send_first(Method::GET, attempt_url, referer.clone(), None)
                .await
            {
                Ok(response) => return Ok(response),
                Err(failure) if failure.eligible && attempt + 1 < max_attempts => {
                    if !self.mirrors.select_next() {
                        return Err(failure.error);
                    }
                }
                Err(failure) => return Err(failure.error),
            }
        }

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
                .send_first(Method::GET, current_url, current_referer, None)
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
        self.send_first(Method::POST, url, referer, Some(form))
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
        self.process_response(response).await
    }

    async fn process_response(
        &mut self,
        response: reqwest::Response,
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
            .filter_map(|value| value.to_str().ok());
        let stored_cookie_names = self
            .jar
            .store_response_cookies_with_names(cookie_headers, &url);

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
        if !(status.is_success() || status.is_redirection()) {
            return Err(AttemptFailure::terminal(invalid_http_status(status, &url)));
        }

        let body = response
            .bytes()
            .await
            .map_err(|_| AttemptFailure::terminal(transport_error()))?;
        let body = String::from_utf8_lossy(&body).into_owned();

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

fn invalid_http_status(status: StatusCode, url: &Url) -> RezkaError {
    let url = redact_url(url.as_str());
    RezkaError::ProviderResponseInvalid {
        context: sanitize_http_status(status.as_u16(), &url),
    }
}
