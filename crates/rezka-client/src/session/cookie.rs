use std::{collections::BTreeSet, fmt, io::Cursor};

use cookie_store::{CookieStore, RawCookie};
use secrecy::{ExposeSecret, SecretBox};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{RezkaError, redaction::sanitize_provider_text};

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
        }
    }

    pub fn import(snapshot: &SessionSnapshot) -> Result<Self, RezkaError> {
        snapshot.with_secret_bytes(|bytes| {
            let snapshot: SnapshotDocument =
                serde_json::from_slice(bytes).map_err(|_| invalid_snapshot())?;
            snapshot.origin.validate()?;
            let cookie_bytes =
                serde_json::to_vec(&snapshot.cookies).map_err(|_| invalid_snapshot())?;
            let store = cookie_store::serde::json::load(Cursor::new(cookie_bytes))
                .map_err(|_| invalid_snapshot())?;
            Ok(Self {
                origin: Some(snapshot.origin),
                store,
            })
        })
    }

    pub fn export(&self) -> Result<SessionSnapshot, RezkaError> {
        let origin = self.origin.clone().ok_or_else(invalid_snapshot)?;
        let mut cookie_bytes = Vec::new();
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(
            &self.store,
            &mut cookie_bytes,
        )
        .map_err(|_| invalid_snapshot())?;
        let cookies = serde_json::from_slice(&cookie_bytes).map_err(|_| invalid_snapshot())?;
        let bytes = serde_json::to_vec(&SnapshotDocument { origin, cookies })
            .map_err(|_| invalid_snapshot())?;
        Ok(SessionSnapshot::from_secret_bytes(SecretBox::new(
            Box::new(bytes),
        )))
    }

    pub fn store_response_cookies<'a>(
        &mut self,
        headers: impl Iterator<Item = &'a str>,
        url: &Url,
    ) {
        let _ = self.store_response_cookies_with_names(headers, url);
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

    pub(crate) fn store_response_cookies_with_names<'a>(
        &mut self,
        headers: impl Iterator<Item = &'a str>,
        url: &Url,
    ) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        if self.bind_or_matches(url).is_err() {
            return names;
        }

        for header in headers {
            let Ok(cookie) = RawCookie::parse(header.to_owned()) else {
                continue;
            };
            if self.store.insert_raw(&cookie, url).is_ok() {
                names.insert(cookie.name().to_owned());
            }
        }

        names
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
