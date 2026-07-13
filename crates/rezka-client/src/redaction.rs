use std::fmt;

#[derive(Clone, Eq, PartialEq)]
pub struct SanitizedSnippet(String);

impl AsRef<str> for SanitizedSnippet {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SanitizedSnippet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SanitizedSnippet")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for SanitizedSnippet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RedactedUrl(String);

impl AsRef<str> for RedactedUrl {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RedactedUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Display for RedactedUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[must_use]
pub fn redact_url(value: &str) -> RedactedUrl {
    match url::Url::parse(value) {
        Ok(mut url) => {
            if url.query().is_some() {
                url.set_query(Some("[REDACTED]"));
            }
            if !url.username().is_empty() || url.password().is_some() {
                let _ = url.set_username("[REDACTED]");
                let _ = url.set_password(Some("[REDACTED]"));
            }
            if matches!(url.host(), Some(url::Host::Ipv4(_) | url::Host::Ipv6(_))) {
                let _ = url.set_host(Some("redacted.invalid"));
            }
            RedactedUrl(url.to_string())
        }
        Err(_) => RedactedUrl("[REDACTED_URL]".to_owned()),
    }
}

#[must_use]
pub fn sanitize_provider_text(input: &str) -> SanitizedSnippet {
    let _ = input;
    SanitizedSnippet("[REDACTED_PROVIDER_TEXT]".to_owned())
}

pub(crate) fn trusted_internal_text(input: &'static str) -> SanitizedSnippet {
    SanitizedSnippet(input.to_owned())
}

#[must_use]
pub(crate) fn sanitize_http_status(status: u16, url: &RedactedUrl) -> SanitizedSnippet {
    SanitizedSnippet(format!("HTTP {status} at {url}"))
}
