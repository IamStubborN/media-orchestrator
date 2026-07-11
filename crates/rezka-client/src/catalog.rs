use std::fmt;

use crate::{PublicImageUrl, RezkaError, redaction::sanitize_provider_text};

pub mod parser;

pub const MAX_CATALOG_ENTRIES: usize = 64;
const MAX_CATALOG_QUERY_SCALARS: usize = 200;
const MAX_CATALOG_QUERY_BYTES: usize = 512;
const MAX_TITLE_LOCATOR_BYTES: usize = 2_048;
const MAX_CATALOG_CONTINUATION_BYTES: usize = 2_048;

pub struct CatalogQuery(String);

impl CatalogQuery {
    pub fn new(value: &str) -> Result<Self, RezkaError> {
        let normalized = value.trim();
        let valid = !normalized.is_empty()
            && normalized.chars().all(|character| !character.is_control())
            && normalized.chars().count() <= MAX_CATALOG_QUERY_SCALARS
            && normalized.len() <= MAX_CATALOG_QUERY_BYTES;
        if !valid {
            return Err(invalid_catalog("invalid catalog query"));
        }

        Ok(Self(normalized.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CatalogQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CatalogQuery([REDACTED])")
    }
}

pub struct TitleLocator(String);

impl TitleLocator {
    pub fn new(value: &str) -> Result<Self, RezkaError> {
        let valid = value.starts_with('/')
            && !value.starts_with("//")
            && value.ends_with(".html")
            && value.len() <= MAX_TITLE_LOCATOR_BYTES
            && !value.contains(['?', '#', '\\'])
            && !value.chars().any(char::is_control)
            && !path_has_prohibited_segment(value);
        if !valid {
            return Err(invalid_catalog("invalid title locator"));
        }

        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TitleLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TitleLocator([REDACTED])")
    }
}

pub struct CatalogContinuation(String);

impl CatalogContinuation {
    pub(crate) fn from_normalized(value: String) -> Result<Self, RezkaError> {
        if value.len() > MAX_CATALOG_CONTINUATION_BYTES {
            return Err(invalid_catalog("catalog continuation exceeds limit"));
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CatalogContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CatalogContinuation([REDACTED])")
    }
}

pub struct CatalogEntry {
    locator: TitleLocator,
    title: String,
    description: Option<String>,
    info: Option<String>,
    thumbnail: Option<PublicImageUrl>,
}

impl CatalogEntry {
    pub(crate) fn new(
        locator: TitleLocator,
        title: String,
        description: Option<String>,
        info: Option<String>,
        thumbnail: Option<PublicImageUrl>,
    ) -> Self {
        Self {
            locator,
            title,
            description,
            info,
            thumbnail,
        }
    }

    #[must_use]
    pub fn locator(&self) -> &TitleLocator {
        &self.locator
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub fn info(&self) -> Option<&str> {
        self.info.as_deref()
    }

    #[must_use]
    pub fn thumbnail(&self) -> Option<&PublicImageUrl> {
        self.thumbnail.as_ref()
    }
}

impl fmt::Debug for CatalogEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CatalogEntry([REDACTED])")
    }
}

pub struct CatalogPage {
    entries: Vec<CatalogEntry>,
    continuation: Option<CatalogContinuation>,
}

impl CatalogPage {
    pub(crate) fn new(
        entries: Vec<CatalogEntry>,
        continuation: Option<CatalogContinuation>,
    ) -> Self {
        Self {
            entries,
            continuation,
        }
    }

    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    #[must_use]
    pub fn continuation(&self) -> Option<&CatalogContinuation> {
        self.continuation.as_ref()
    }
}

impl fmt::Debug for CatalogPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogPage")
            .field("entries", &self.entries.len())
            .field("continuation", &self.continuation.is_some())
            .finish()
    }
}

pub(crate) fn invalid_catalog(reason: &str) -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text(reason),
    }
}

pub(crate) fn path_has_prohibited_segment(path: &str) -> bool {
    let mut decoded = path.to_owned();
    loop {
        let Some(next) = decode_percent_once(&decoded) else {
            return true;
        };
        decoded = next;
        if decoded.chars().any(char::is_control)
            || decoded
                .split('/')
                .any(|segment| matches!(segment, "." | ".."))
        {
            return true;
        }
        if !contains_percent_encoded_byte(&decoded) {
            return false;
        }
    }
}

fn decode_percent_once(path: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = decode_hex(bytes.get(index + 1).copied())?;
            let low = decode_hex(bytes.get(index + 2).copied())?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(decoded).ok()
}

fn contains_percent_encoded_byte(value: &str) -> bool {
    value.as_bytes().windows(3).any(|window| {
        window[0] == b'%'
            && decode_hex(Some(window[1])).is_some()
            && decode_hex(Some(window[2])).is_some()
    })
}

fn decode_hex(value: Option<u8>) -> Option<u8> {
    let value = value?;
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{CatalogContinuation, MAX_CATALOG_CONTINUATION_BYTES};

    #[test]
    fn continuation_budget_accepts_the_boundary_and_rejects_the_next_byte() {
        assert!(
            CatalogContinuation::from_normalized("x".repeat(MAX_CATALOG_CONTINUATION_BYTES))
                .is_ok()
        );
        assert!(
            CatalogContinuation::from_normalized("x".repeat(MAX_CATALOG_CONTINUATION_BYTES + 1))
                .is_err()
        );
    }
}
