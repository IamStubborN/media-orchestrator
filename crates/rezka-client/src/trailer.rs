use scraper::{Html, Selector};
use serde::Deserialize;
use url::Url;

use crate::{
    RezkaError, RezkaTitleId, TitleLocator, redaction::sanitize_provider_text, session::RezkaClient,
};

const TRAILER_PATH: &str = "/engine/ajax/gettrailervideo.php";
const MAX_TRAILER_TEXT_BYTES: usize = 4_096;

#[derive(Clone)]
pub struct Trailer {
    title: Option<String>,
    description: Option<String>,
    embed_url: Url,
}

impl Trailer {
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub const fn embed_url(&self) -> &Url {
        &self.embed_url
    }
}

impl std::fmt::Debug for Trailer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Trailer")
            .field("title", &self.title.as_ref().map(|_| "[REDACTED]"))
            .field(
                "description",
                &self.description.as_ref().map(|_| "[REDACTED]"),
            )
            .field("embed_url", &"[REDACTED]")
            .finish()
    }
}

impl RezkaClient {
    pub async fn trailer(
        &mut self,
        id: RezkaTitleId,
        locator: &TitleLocator,
    ) -> Result<Option<Trailer>, RezkaError> {
        let origin = self.transport_mut().selected_origin().clone();
        let endpoint = origin
            .join(TRAILER_PATH)
            .map_err(|_| invalid_trailer("invalid trailer endpoint"))?;
        let referer = origin
            .join(locator.as_str())
            .map_err(|_| invalid_trailer("invalid trailer referer"))?;
        let id = id.get().to_string();
        let response = self
            .transport_mut()
            .post_form_with_failover(endpoint, Some(referer), &[("id", id.as_str())])
            .await?;
        parse_trailer(&response.body)
    }
}

#[derive(Deserialize)]
struct TrailerResponse {
    success: bool,
    #[serde(default)]
    code: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    link: String,
}

fn parse_trailer(body: &str) -> Result<Option<Trailer>, RezkaError> {
    let response: TrailerResponse =
        serde_json::from_str(body).map_err(|_| invalid_trailer("invalid trailer response"))?;
    if !response.success {
        return Ok(None);
    }
    let iframe_url = if response.code.is_empty() {
        None
    } else {
        let document = Html::parse_fragment(&response.code);
        let selector = Selector::parse("iframe[src]").expect("static selector is valid");
        document
            .select(&selector)
            .next()
            .and_then(|element| element.value().attr("src"))
            .map(str::to_owned)
    };
    let candidate = iframe_url
        .as_deref()
        .or((!response.link.is_empty()).then_some(response.link.as_str()))
        .ok_or_else(|| invalid_trailer("trailer response missing URL"))?;
    let embed_url = Url::parse(candidate).map_err(|_| invalid_trailer("invalid trailer URL"))?;
    if embed_url.scheme() != "https" || embed_url.host_str().is_none() {
        return Err(invalid_trailer("invalid trailer URL"));
    }
    Ok(Some(Trailer {
        title: bounded_optional(response.title)?,
        description: bounded_optional(response.description)?,
        embed_url,
    }))
}

fn bounded_optional(value: String) -> Result<Option<String>, RezkaError> {
    let value = value.trim();
    if value.len() > MAX_TRAILER_TEXT_BYTES || value.chars().any(char::is_control) {
        return Err(invalid_trailer("invalid trailer text"));
    }
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

fn invalid_trailer(reason: &str) -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_trailer;

    #[test]
    fn parses_https_iframe_without_exposing_it_in_debug() {
        let body = r#"{"success":true,"code":"<iframe src=\"https://youtube.com/embed/abc?token=secret\"></iframe>","title":"Trailer","description":"Preview","link":""}"#;
        let trailer = parse_trailer(body).unwrap().unwrap();
        assert_eq!(trailer.title(), Some("Trailer"));
        assert_eq!(trailer.embed_url().host_str(), Some("youtube.com"));
        assert!(!format!("{trailer:?}").contains("token=secret"));
    }

    #[test]
    fn rejects_insecure_urls_and_accepts_absent_trailer() {
        let insecure = r#"{"success":true,"code":"","title":"","description":"","link":"http://example.com/trailer"}"#;
        assert!(parse_trailer(insecure).is_err());
        let absent = r#"{"success":false,"code":"","title":"","description":"","link":""}"#;
        assert!(parse_trailer(absent).unwrap().is_none());
    }
}
