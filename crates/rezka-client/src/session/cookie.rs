use std::{collections::BTreeSet, fmt, io::Cursor};

use cookie_store::{CookieStore, RawCookie};
use secrecy::{ExposeSecret, SecretBox};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{RezkaError, redaction::sanitize_provider_text};

pub(crate) const MAX_SESSION_SNAPSHOT_BYTES: usize = 128 * 1024;
const MAX_SET_COOKIE_HEADERS: usize = 64;
const MAX_SET_COOKIE_HEADER_BYTES: usize = 8 * 1024;
const MAX_SESSION_COOKIES: usize = 64;
const CURRENT_SESSION_FORMAT: u8 = 2;
// The former DLE login flow only bound authentication state to this exact
// cookie.  Do not broaden this list: PHPSESSID may be present alongside
// anonymous Anubis and provider cookies, all of which must remain reusable.
const DLE_AUTHENTICATION_COOKIE: &str = "PHPSESSID";

pub struct SessionSnapshot {
    bytes: SecretBox<Vec<u8>>,
}

impl SessionSnapshot {
    #[must_use]
    pub fn from_secret_bytes(bytes: SecretBox<Vec<u8>>) -> Self {
        Self { bytes }
    }

    pub fn with_secret_bytes<R>(&self, consumer: impl FnOnce(&[u8]) -> R) -> R {
        consumer(self.bytes.expose_secret())
    }

    #[must_use]
    pub fn secret_eq(&self, other: &Self) -> bool {
        self.bytes.expose_secret() == other.bytes.expose_secret()
    }
}

impl fmt::Debug for SessionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionSnapshot { bytes: [REDACTED] }")
    }
}

pub struct SessionJar {
    origin: Option<OriginBinding>,
    store: CookieStore,
    anonymous_migrated: bool,
}

impl fmt::Debug for SessionJar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionJar([REDACTED])")
    }
}

impl SessionJar {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            origin: None,
            store: CookieStore::default(),
            anonymous_migrated: true,
        }
    }

    pub fn import(snapshot: &SessionSnapshot) -> Result<Self, RezkaError> {
        snapshot.with_secret_bytes(|bytes| {
            if bytes.len() > MAX_SESSION_SNAPSHOT_BYTES {
                return Err(cookie_budget_exceeded());
            }
            let snapshot: SnapshotDocument =
                serde_json::from_slice(bytes).map_err(|_| invalid_snapshot())?;
            snapshot.origin.validate()?;
            let cookie_bytes =
                serde_json::to_vec(&snapshot.cookies).map_err(|_| invalid_snapshot())?;
            let store = cookie_store::serde::json::load(Cursor::new(cookie_bytes))
                .map_err(|_| invalid_snapshot())?;
            if store.iter_any().take(MAX_SESSION_COOKIES + 1).count() > MAX_SESSION_COOKIES {
                return Err(cookie_budget_exceeded());
            }
            Ok(Self {
                origin: Some(snapshot.origin),
                store,
                anonymous_migrated: snapshot.session_format >= CURRENT_SESSION_FORMAT,
            })
        })
    }

    pub fn export(&self) -> Result<SessionSnapshot, RezkaError> {
        let origin = self.origin.clone().ok_or_else(invalid_snapshot)?;
        let bytes = serialize_snapshot(&origin, &self.store)?;
        if bytes.len() > MAX_SESSION_SNAPSHOT_BYTES {
            return Err(cookie_budget_exceeded());
        }
        Ok(SessionSnapshot::from_secret_bytes(SecretBox::new(
            Box::new(bytes),
        )))
    }

    /// Remove only the Anubis clearance cookie. All other anonymous provider cookies remain intact.
    pub fn invalidate_anubis_clearance(&mut self) -> bool {
        let host_only_domain = self.origin.as_ref().map(|origin| origin.host.as_str());
        let locations = self
            .store
            .iter_any()
            .filter(|cookie| cookie.name() == crate::session::anubis::CLEARANCE_COOKIE)
            .map(|cookie| {
                (
                    cookie
                        .domain()
                        .or(host_only_domain)
                        .unwrap_or_default()
                        .to_owned(),
                    cookie.path().unwrap_or("/").to_owned(),
                    cookie.name().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let mut removed = false;
        for (domain, path, name) in locations {
            removed |= self.store.remove(&domain, &path, &name).is_some();
        }
        removed
    }

    /// Remove the one cookie owned by the retired DLE authentication flow.
    /// This is an idempotent anonymous migration; Anubis and unrelated
    /// provider cookies are deliberately preserved.
    pub fn remove_dle_authentication_cookie(&mut self) -> bool {
        if self.anonymous_migrated {
            return false;
        }
        self.anonymous_migrated = true;
        let host_only_domain = self.origin.as_ref().map(|origin| origin.host.as_str());
        let locations = self
            .store
            .iter_any()
            .filter(|cookie| {
                cookie.name() == DLE_AUTHENTICATION_COOKIE
                    && cookie.domain().is_none_or(|domain| {
                        host_only_domain.is_some_and(|host| domain.trim_start_matches('.') == host)
                    })
            })
            .map(|cookie| {
                (
                    cookie
                        .domain()
                        .or(host_only_domain)
                        .unwrap_or_default()
                        .to_owned(),
                    cookie.path().unwrap_or("/").to_owned(),
                    cookie.name().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let mut removed = false;
        for (domain, path, name) in locations {
            removed |= self.store.remove(&domain, &path, &name).is_some();
        }
        removed
    }

    pub fn store_response_cookies<'a>(
        &mut self,
        headers: impl Iterator<Item = &'a str>,
        url: &Url,
    ) {
        let _ = self.store_response_cookies_with_names(headers.map(str::as_bytes), url);
    }

    #[must_use]
    pub fn contains_cookie_for_url(&self, url: &Url, name: &str) -> bool {
        if !self.is_bound_to(url) {
            return false;
        }
        self.store
            .get_request_values(url)
            .any(|(cookie_name, _)| cookie_name == name)
    }

    #[must_use]
    pub fn has_cookies(&self) -> bool {
        self.store.iter_any().next().is_some()
    }

    pub(crate) fn store_response_cookies_with_names<'a>(
        &mut self,
        headers: impl Iterator<Item = &'a [u8]>,
        url: &Url,
    ) -> Result<BTreeSet<String>, RezkaError> {
        let mut bounded_headers = Vec::with_capacity(MAX_SET_COOKIE_HEADERS);
        for header in headers {
            if bounded_headers.len() == MAX_SET_COOKIE_HEADERS
                || header.len() > MAX_SET_COOKIE_HEADER_BYTES
            {
                return Err(cookie_budget_exceeded());
            }
            bounded_headers.push(header);
        }

        let mut names = BTreeSet::new();
        let candidate_origin = match &self.origin {
            Some(origin) if origin.matches(url) => origin.clone(),
            Some(_) => return Ok(names),
            None => OriginBinding::from_url(url)?,
        };
        let mut candidate = self.store.clone();

        for header in bounded_headers {
            let Ok(header) = std::str::from_utf8(header) else {
                continue;
            };
            let Ok(cookie) = RawCookie::parse(header.to_owned()) else {
                continue;
            };
            if candidate.insert_raw(&cookie, url).is_ok() {
                names.insert(cookie.name().to_owned());
            }
        }
        if candidate.iter_any().take(MAX_SESSION_COOKIES + 1).count() > MAX_SESSION_COOKIES
            || serialize_snapshot(&candidate_origin, &candidate)?.len() > MAX_SESSION_SNAPSHOT_BYTES
        {
            return Err(cookie_budget_exceeded());
        }

        self.origin = Some(candidate_origin);
        self.store = candidate;
        Ok(names)
    }

    pub(crate) fn request_cookie_header(&self, url: &Url) -> Option<String> {
        if !self.is_bound_to(url) {
            return None;
        }
        let values = self
            .store
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>();

        (!values.is_empty()).then(|| values.join("; "))
    }

    pub(crate) fn bind_to(&mut self, url: &Url) -> Result<(), RezkaError> {
        self.bind_or_matches(url)
    }

    pub(crate) fn bound_empty(url: &Url) -> Result<Self, RezkaError> {
        Ok(Self {
            origin: Some(OriginBinding::from_url(url)?),
            store: CookieStore::default(),
            anonymous_migrated: true,
        })
    }

    pub(crate) fn has_origin_binding(&self) -> bool {
        self.origin.is_some()
    }

    pub(crate) fn is_bound_to(&self, url: &Url) -> bool {
        self.origin
            .as_ref()
            .is_some_and(|origin| origin.matches(url))
    }

    fn bind_or_matches(&mut self, url: &Url) -> Result<(), RezkaError> {
        if let Some(origin) = &self.origin {
            return origin
                .matches(url)
                .then_some(())
                .ok_or_else(invalid_snapshot);
        }

        self.origin = Some(OriginBinding::from_url(url)?);
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotDocument {
    #[serde(default)]
    session_format: u8,
    origin: OriginBinding,
    cookies: serde_json::Value,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OriginBinding {
    scheme: String,
    host: String,
    port: u16,
}

impl OriginBinding {
    fn from_url(url: &Url) -> Result<Self, RezkaError> {
        let scheme = url.scheme();
        let host = url.host_str().ok_or_else(invalid_snapshot)?;
        let port = url.port_or_known_default().ok_or_else(invalid_snapshot)?;
        if !matches!(scheme, "http" | "https") {
            return Err(invalid_snapshot());
        }

        Ok(Self {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            port,
        })
    }

    fn matches(&self, url: &Url) -> bool {
        self.scheme == url.scheme()
            && url.host_str().is_some_and(|host| self.host == host)
            && url.port_or_known_default() == Some(self.port)
    }

    fn validate(&self) -> Result<(), RezkaError> {
        if !matches!(self.scheme.as_str(), "http" | "https")
            || self.host.is_empty()
            || self.port == 0
        {
            return Err(invalid_snapshot());
        }
        Ok(())
    }
}

fn invalid_snapshot() -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text("invalid cookie snapshot"),
    }
}

fn serialize_snapshot(origin: &OriginBinding, store: &CookieStore) -> Result<Vec<u8>, RezkaError> {
    let mut cookie_bytes = Vec::new();
    cookie_store::serde::json::save_incl_expired_and_nonpersistent(store, &mut cookie_bytes)
        .map_err(|_| invalid_snapshot())?;
    let cookies = serde_json::from_slice(&cookie_bytes).map_err(|_| invalid_snapshot())?;
    serde_json::to_vec(&SnapshotDocument {
        session_format: CURRENT_SESSION_FORMAT,
        origin: origin.clone(),
        cookies,
    })
    .map_err(|_| invalid_snapshot())
}

fn cookie_budget_exceeded() -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text("cookie session exceeds limits"),
    }
}
