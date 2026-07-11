use std::{collections::BTreeSet, fmt, io::Cursor};

use cookie_store::{CookieStore, RawCookie};
use secrecy::{ExposeSecret, SecretBox};
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
        formatter.write_str("SessionSnapshot([REDACTED])")
    }
}

#[derive(Default)]
pub struct SessionJar {
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
        Self::default()
    }

    pub fn import(snapshot: &SessionSnapshot) -> Result<Self, RezkaError> {
        snapshot.with_secret_bytes(|bytes| {
            cookie_store::serde::json::load(Cursor::new(bytes))
                .map(|store| Self { store })
                .map_err(|_| invalid_snapshot())
        })
    }

    pub fn export(&self) -> Result<SessionSnapshot, RezkaError> {
        let mut bytes = Vec::new();
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&self.store, &mut bytes)
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
        let values = self
            .store
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>();

        (!values.is_empty()).then(|| values.join("; "))
    }
}

fn invalid_snapshot() -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text("invalid cookie snapshot"),
    }
}
