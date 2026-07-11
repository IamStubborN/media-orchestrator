use std::fmt;

use reqwest::StatusCode;
use url::Url;

use crate::RezkaError;

pub struct SessionValidationProbe {
    pub(crate) url: Url,
    valid_markers: Vec<String>,
    invalid_markers: Vec<String>,
}

impl SessionValidationProbe {
    pub fn new(
        url: Url,
        valid_markers: Vec<String>,
        invalid_markers: Vec<String>,
    ) -> Result<Self, RezkaError> {
        if !matches!(url.scheme(), "http" | "https")
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || !valid_marker_set(&valid_markers)
            || !valid_marker_set(&invalid_markers)
        {
            return Err(RezkaError::Configuration {
                message: "invalid session validation probe",
            });
        }

        Ok(Self {
            url,
            valid_markers,
            invalid_markers,
        })
    }
}

impl fmt::Debug for SessionValidationProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "SessionValidationProbe { url: [REDACTED], valid_markers: [REDACTED], invalid_markers: [REDACTED] }",
        )
    }
}

pub struct ProbeResponse {
    pub status: StatusCode,
    pub url: Url,
    body: String,
}

impl ProbeResponse {
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    pub(crate) fn new(status: StatusCode, url: Url, body: String) -> Self {
        Self { status, url, body }
    }
}

impl fmt::Debug for ProbeResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeResponse")
            .field("status", &self.status)
            .field("url", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SessionValidation {
    Valid,
    Invalid,
    Inconclusive,
}

pub(crate) fn classify(
    probe: &SessionValidationProbe,
    response: &ProbeResponse,
) -> SessionValidation {
    let valid = probe
        .valid_markers
        .iter()
        .any(|marker| response.body.contains(marker));
    let invalid = probe
        .invalid_markers
        .iter()
        .any(|marker| response.body.contains(marker));

    match (valid, invalid) {
        (true, false) => SessionValidation::Valid,
        (false, true) => SessionValidation::Invalid,
        _ => SessionValidation::Inconclusive,
    }
}

fn valid_marker_set(markers: &[String]) -> bool {
    !markers.is_empty() && markers.iter().all(|marker| !marker.trim().is_empty())
}
