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
const DEFAULT_ANUBIS_MAX_NONCE: u64 = 5_000_000;
const ANUBIS_SOLVER_TIMEOUT: StdDuration = StdDuration::from_secs(30);
use time::Duration;
use url::Url;

use crate::{
    RezkaError, RezkaErrorCode,
    mirror::{MirrorSet, same_origin},
    redaction::{redact_url, sanitize_http_status, sanitize_provider_text},
    session::{
        anubis::{
            self, BrowserChallengeFallback, CLEARANCE_COOKIE, detect_challenge, parse_challenge,
        },
        cookie::{SessionJar, SessionSnapshot},
    },
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
    anubis_max_nonce: u64,
    browser_fallback: Option<Box<dyn BrowserChallengeFallback>>,
    // Set when a failover replaces the jar with an empty one bound to a new origin. Cleared only when
    // the selected origin is safely confirmed; while set, the current jar is a transient failover
    // artifact, so exporting it would overwrite a previously persisted session.
    session_reset_by_failover: bool,
    held_session: bool,
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
        mirrors: MirrorSet,
        jar: SessionJar,
        user_agent: String,
        request_timeout: Duration,
        max_retries: u8,
    ) -> Result<Self, RezkaError> {
        Self::new_with_proxy(mirrors, jar, user_agent, request_timeout, max_retries, None)
    }

    pub fn new_with_proxy(
        mirrors: MirrorSet,
        jar: SessionJar,
        user_agent: String,
        request_timeout: Duration,
        max_retries: u8,
        proxy_url: Option<Url>,
    ) -> Result<Self, RezkaError> {
        Self::new_with_proxy_and_anubis(
            mirrors,
            jar,
            user_agent,
            request_timeout,
            max_retries,
            proxy_url,
            DEFAULT_ANUBIS_MAX_NONCE,
        )
    }

    pub(crate) fn new_with_proxy_and_anubis(
        mut mirrors: MirrorSet,
        mut jar: SessionJar,
        user_agent: String,
        request_timeout: Duration,
        max_retries: u8,
        proxy_url: Option<Url>,
        anubis_max_nonce: u64,
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

        let held_session = jar.has_cookies();

        let mut client = Client::builder()
            .redirect(Policy::none())
            .timeout(request_timeout)
            .user_agent(user_agent);
        if let Some(proxy_url) = proxy_url {
            client = client.proxy(reqwest::Proxy::all(proxy_url).map_err(|_| {
                RezkaError::Configuration {
                    message: "invalid proxy URL",
                }
            })?);
        }
        let client = client.build().map_err(|_| RezkaError::Configuration {
            message: "invalid transport configuration",
        })?;

        Ok(Self {
            client,
            mirrors,
            jar,
            max_retries,
            anubis_max_nonce,
            browser_fallback: None,
            session_reset_by_failover: false,
            held_session,
        })
    }

    pub fn from_snapshot(
        mirrors: MirrorSet,
        snapshot: &SessionSnapshot,
        user_agent: String,
        request_timeout: Duration,
        max_retries: u8,
    ) -> Result<Self, RezkaError> {
        Self::from_snapshot_with_proxy(
            mirrors,
            snapshot,
            user_agent,
            request_timeout,
            max_retries,
            None,
        )
    }

    pub fn from_snapshot_with_proxy(
        mirrors: MirrorSet,
        snapshot: &SessionSnapshot,
        user_agent: String,
        request_timeout: Duration,
        max_retries: u8,
        proxy_url: Option<Url>,
    ) -> Result<Self, RezkaError> {
        Self::from_snapshot_with_proxy_and_anubis(
            mirrors,
            snapshot,
            user_agent,
            request_timeout,
            max_retries,
            proxy_url,
            DEFAULT_ANUBIS_MAX_NONCE,
        )
    }

    pub(crate) fn from_snapshot_with_proxy_and_anubis(
        mirrors: MirrorSet,
        snapshot: &SessionSnapshot,
        user_agent: String,
        request_timeout: Duration,
        max_retries: u8,
        proxy_url: Option<Url>,
        anubis_max_nonce: u64,
    ) -> Result<Self, RezkaError> {
        Self::new_with_proxy_and_anubis(
            mirrors,
            SessionJar::import(snapshot)?,
            user_agent,
            request_timeout,
            max_retries,
            proxy_url,
            anubis_max_nonce,
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

    pub(crate) async fn get_first_without_challenge(
        &mut self,
        url: Url,
        referer: Option<Url>,
    ) -> Result<TransportResponse, RezkaError> {
        self.send_first_raw(
            Method::GET,
            url,
            referer,
            None,
            ResponseStatusPolicy::Default,
        )
        .await
        .map_err(|failure| failure.error)
    }

    async fn send_idempotent_with_failover(
        &mut self,
        method: Method,
        url: Url,
        referer: Option<Url>,
        form: Option<&[(&str, &str)]>,
        status_policy: ResponseStatusPolicy,
    ) -> Result<TransportResponse, RezkaError> {
        let max_attempts = usize::from(self.max_retries).saturating_add(1);
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
                    if self.mirrors.select_next() {
                        self.jar = SessionJar::bound_empty(self.mirrors.selected_origin())?;
                        self.session_reset_by_failover = true;
                    }
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
                .send_idempotent_with_failover(
                    Method::GET,
                    current_url,
                    current_referer,
                    None,
                    ResponseStatusPolicy::Default,
                )
                .await?;

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
        if self.session_reset_by_failover {
            // A failover discarded the previous origin's jar and no later request re-established a
            // session, so the current jar is empty. Refuse to export it rather than let a caller
            // overwrite a previously persisted, still-valid snapshot with an empty one.
            return Err(RezkaError::Transport {
                context: sanitize_provider_text("session reset by failover; snapshot preserved"),
            });
        }
        self.jar.export()
    }

    pub(crate) fn invalidate_anubis_clearance(&mut self) -> bool {
        self.jar.invalidate_anubis_clearance()
    }

    pub(crate) fn remove_dle_authentication_cookie(&mut self) -> bool {
        self.jar.remove_dle_authentication_cookie()
    }

    /// Attach the private-runner browser helper for unsupported or rejected native challenges.
    pub fn with_browser_fallback(mut self, fallback: Box<dyn BrowserChallengeFallback>) -> Self {
        self.browser_fallback = Some(fallback);
        self
    }

    async fn send_first(
        &mut self,
        method: Method,
        url: Url,
        referer: Option<Url>,
        form: Option<&[(&str, &str)]>,
        status_policy: ResponseStatusPolicy,
    ) -> Result<TransportResponse, AttemptFailure> {
        let response = self
            .send_first_raw(
                method.clone(),
                url.clone(),
                referer.clone(),
                form,
                status_policy,
            )
            .await?;
        if detect_challenge(&response.body) {
            return self
                .solve_and_retry_challenge(method, url, referer, form, status_policy, response)
                .await;
        }
        self.confirm_current_origin();
        Ok(response)
    }

    async fn send_first_raw(
        &mut self,
        method: Method,
        url: Url,
        referer: Option<Url>,
        form: Option<&[(&str, &str)]>,
        status_policy: ResponseStatusPolicy,
    ) -> Result<TransportResponse, AttemptFailure> {
        self.guard_selected_origin(&url)
            .map_err(AttemptFailure::terminal)?;

        let mut request = self.client.request(method.clone(), url.clone());
        if let Some(ref referer) = referer {
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

    async fn solve_and_retry_challenge(
        &mut self,
        method: Method,
        url: Url,
        referer: Option<Url>,
        form: Option<&[(&str, &str)]>,
        status_policy: ResponseStatusPolicy,
        challenge_response: TransportResponse,
    ) -> Result<TransportResponse, AttemptFailure> {
        // A challenge response means the existing clearance is no longer accepted for this IP.
        // Replace only that cookie; unrelated anonymous provider state remains in the jar.
        self.invalidate_anubis_clearance();
        let challenge = match parse_challenge(&challenge_response.body) {
            Ok(challenge) => challenge,
            Err(error @ RezkaError::AnubisUnsupportedAlgorithm { .. }) => {
                let challenge = anubis::parse_challenge_for_fallback(&challenge_response.body)
                    .map_err(AttemptFailure::terminal)?;
                if !self
                    .run_browser_fallback(&challenge, challenge_response.url.clone())
                    .await?
                {
                    return Err(AttemptFailure::terminal(error));
                }
                let retry = self
                    .send_first_raw(method, url, referer, form, status_policy)
                    .await?;
                if detect_challenge(&retry.body) {
                    return Err(AttemptFailure::terminal(RezkaError::AnubisRejected {
                        context: sanitize_provider_text("browser challenge clearance was rejected"),
                    }));
                }
                self.held_session = true;
                self.confirm_current_origin();
                return Ok(retry);
            }
            Err(RezkaError::ProviderResponseInvalid { .. }) => {
                return Err(AttemptFailure::terminal(RezkaError::ChallengeRequired {
                    context: sanitize_provider_text("Anubis challenge requires handling"),
                }));
            }
            Err(error) => return Err(AttemptFailure::terminal(error)),
        };
        let started = std::time::Instant::now();
        let proof = anubis::solve_challenge_bounded(
            challenge.clone(),
            self.anubis_max_nonce,
            ANUBIS_SOLVER_TIMEOUT,
        )
        .await
        .map_err(AttemptFailure::terminal)?;
        let mut browser_fallback_used = false;
        let pass_result = anubis::submit_challenge(
            self,
            &challenge,
            &proof,
            challenge_response.url.clone(),
            started.elapsed().as_millis(),
        )
        .await;
        if let Err(error) = pass_result {
            let mapped = match error {
                RezkaError::ProviderResponseInvalid { .. } | RezkaError::ChallengeFailed { .. } => {
                    RezkaError::AnubisRejected {
                        context: sanitize_provider_text(
                            "Anubis pass endpoint rejected the solution",
                        ),
                    }
                }
                other => other,
            };
            if !matches!(&mapped, RezkaError::AnubisRejected { .. }) {
                return Err(AttemptFailure::terminal(mapped));
            }
            browser_fallback_used = true;
            if !self
                .run_browser_fallback(&challenge, challenge_response.url.clone())
                .await?
            {
                return Err(AttemptFailure::terminal(mapped));
            }
        }
        if !self
            .jar
            .contains_cookie_for_url(self.selected_origin(), CLEARANCE_COOKIE)
        {
            if browser_fallback_used {
                return Err(AttemptFailure::terminal(RezkaError::AnubisRejected {
                    context: sanitize_provider_text(
                        "Anubis pass endpoint did not return clearance",
                    ),
                }));
            }
            browser_fallback_used = true;
            if !self
                .run_browser_fallback(&challenge, challenge_response.url.clone())
                .await?
            {
                return Err(AttemptFailure::terminal(RezkaError::AnubisRejected {
                    context: sanitize_provider_text(
                        "Anubis pass endpoint did not return clearance",
                    ),
                }));
            }
        }

        let retry = self
            .send_first_raw(
                method.clone(),
                url.clone(),
                referer.clone(),
                form,
                status_policy,
            )
            .await?;
        if detect_challenge(&retry.body) {
            if !browser_fallback_used {
                // Native clearance was rejected. Give the optional browser
                // seam exactly one opportunity, then perform one final raw
                // retry; no challenge recursion is allowed.
                self.invalidate_anubis_clearance();
                if self
                    .run_browser_fallback(&challenge, challenge_response.url.clone())
                    .await?
                {
                    let fallback_retry = self
                        .send_first_raw(method, url, referer, form, status_policy)
                        .await?;
                    if !detect_challenge(&fallback_retry.body) {
                        self.held_session = true;
                        self.confirm_current_origin();
                        return Ok(fallback_retry);
                    }
                }
            }
            return Err(AttemptFailure::terminal(RezkaError::AnubisRejected {
                context: sanitize_provider_text("Anubis clearance was rejected after one retry"),
            }));
        }
        self.held_session = true;
        self.confirm_current_origin();
        Ok(retry)
    }

    async fn run_browser_fallback(
        &mut self,
        challenge: &anubis::AnubisChallenge,
        origin: Url,
    ) -> Result<bool, AttemptFailure> {
        let Some(fallback) = self.browser_fallback.take() else {
            return Ok(false);
        };
        let mut guard = BrowserFallbackGuard {
            slot: &mut self.browser_fallback,
            fallback: Some(fallback),
        };
        let result = guard
            .fallback
            .as_mut()
            .expect("browser fallback guard owns the adapter")
            .solve(challenge, &origin)
            .await;
        let cookie_headers = result.map_err(AttemptFailure::terminal)?;
        self.jar
            .store_response_cookies_with_names(cookie_headers.iter().map(String::as_bytes), &origin)
            .map_err(AttemptFailure::terminal)?;
        if !self.jar.contains_cookie_for_url(&origin, CLEARANCE_COOKIE) {
            return Err(AttemptFailure::terminal(RezkaError::AnubisRejected {
                context: sanitize_provider_text("browser challenge clearance was missing"),
            }));
        }
        Ok(true)
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

        // Anubis may use an access-denied status for its interstitial. Read the
        // bounded body for those statuses so challenge markers win over the
        // generic HTTP-status classification and can enter the same solver path
        // as a 200 interstitial. Other terminal statuses retain the existing
        // short-circuit behavior (in particular, do not read an unbounded 404
        // body just to look for a challenge).
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            let body = match read_provider_body(&mut response).await {
                Ok(body) => body,
                // A truncated terminal response is still a provider status,
                // not a transport retry. Preserve the established typed
                // status error while allowing complete 401/403 bodies to
                // enter challenge handling above.
                Err(failure) if failure.error.code() == RezkaErrorCode::Transport => {
                    return Err(AttemptFailure::terminal(invalid_http_status(status, &url)));
                }
                Err(failure) => return Err(failure),
            };
            if detect_challenge(&body) {
                return Ok(TransportResponse {
                    status,
                    url,
                    body,
                    location,
                    stored_cookie_names,
                });
            }
            return Err(AttemptFailure::terminal(invalid_http_status(status, &url)));
        }
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

        let body = read_provider_body(&mut response).await?;

        Ok(TransportResponse {
            status,
            url,
            body,
            location,
            stored_cookie_names,
        })
    }

    fn confirm_current_origin(&mut self) {
        // The selected origin answered. Clear the failover guard only when the resulting jar is safe
        // to persist: either it is authenticated again on this origin, or no authenticated session
        // was ever held (so there is nothing an anonymous jar could overwrite). Otherwise a partial
        // failover (old origin down, new one serving anonymous pages) would drop the guard on the
        // pre-login probe success and let a later export overwrite a saved authenticated snapshot.
        if self
            .jar
            .contains_cookie_for_url(self.mirrors.selected_origin(), CLEARANCE_COOKIE)
            // PHPSESSID is retained here only as a narrowly recognized legacy
            // re-authentication signal for snapshots created before the
            // anonymous migration. Arbitrary provider cookies must not lift
            // the guard after a cross-origin failover.
            || self
                .jar
                .contains_cookie_for_url(self.mirrors.selected_origin(), "PHPSESSID")
            || !self.held_session
        {
            self.session_reset_by_failover = false;
        }
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

async fn read_provider_body(response: &mut reqwest::Response) -> Result<String, AttemptFailure> {
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
    while let Some(chunk) = response.chunk().await.map_err(|_| AttemptFailure {
        error: transport_error(),
        eligible: true,
    })? {
        let cumulative_length = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| AttemptFailure::terminal(oversized_response_body()))?;
        if cumulative_length >= PROVIDER_RESPONSE_BODY_OVERFLOW_BYTES {
            return Err(AttemptFailure::terminal(oversized_response_body()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(decode_provider_body(body))
}

fn decode_provider_body(body: Vec<u8>) -> String {
    String::from_utf8(body)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

struct BrowserFallbackGuard<'a> {
    slot: &'a mut Option<Box<dyn BrowserChallengeFallback>>,
    fallback: Option<Box<dyn BrowserChallengeFallback>>,
}

impl Drop for BrowserFallbackGuard<'_> {
    fn drop(&mut self) {
        if let Some(fallback) = self.fallback.take() {
            *self.slot = Some(fallback);
        }
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
    async fn explicit_proxy_routes_only_the_rezka_transport() {
        let proxy = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/title"))
            .respond_with(ResponseTemplate::new(200).set_body_string("proxied"))
            .expect(1)
            .mount(&proxy)
            .await;
        let origin = Url::parse("http://127.0.0.1:9").unwrap();
        let mut transport = Transport::new_with_proxy(
            MirrorSet::new(vec![origin.clone()]).unwrap(),
            SessionJar::empty(),
            "media-orchestrator-test".to_owned(),
            Duration::seconds(2),
            0,
            Some(Url::parse(&proxy.uri()).unwrap()),
        )
        .unwrap();

        let response = transport
            .get_first_with_failover_accepting(origin.join("/title").unwrap(), None)
            .await
            .unwrap();

        assert_eq!(response.body, "proxied");
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
