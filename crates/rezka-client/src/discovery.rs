use std::fmt;

use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::{
    RezkaError, TitleLocator,
    redaction::sanitize_provider_text,
    session::{RezkaClient, anubis::detect_challenge},
    transport::MAX_PROVIDER_RESPONSE_BODY_BYTES,
};

pub const MAX_QUICK_SEARCH_ENTRIES: usize = 32;
const MAX_QUICK_SEARCH_QUERY_SCALARS: usize = 200;
const MAX_QUICK_SEARCH_QUERY_BYTES: usize = 512;
const MAX_QUICK_SEARCH_TITLE_BYTES: usize = 512;
const MAX_QUICK_SEARCH_INFO_BYTES: usize = 1_024;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PremiumStatus {
    Active,
    Inactive,
}

pub struct QuickSearchQuery(String);

impl QuickSearchQuery {
    pub fn new(value: &str) -> Result<Self, RezkaError> {
        let normalized = value.trim();
        let valid = !normalized.is_empty()
            && normalized.chars().all(|character| !character.is_control())
            && normalized.chars().any(is_visible_scalar)
            && normalized.chars().count() <= MAX_QUICK_SEARCH_QUERY_SCALARS
            && normalized.len() <= MAX_QUICK_SEARCH_QUERY_BYTES;
        if !valid {
            return Err(invalid_discovery("invalid quick search query"));
        }

        Ok(Self(normalized.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for QuickSearchQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("QuickSearchQuery([REDACTED])")
    }
}

pub struct QuickSearchEntry {
    title: String,
    info: Option<String>,
    locator: TitleLocator,
}

impl QuickSearchEntry {
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn info(&self) -> Option<&str> {
        self.info.as_deref()
    }

    #[must_use]
    pub fn locator(&self) -> &TitleLocator {
        &self.locator
    }
}

impl fmt::Debug for QuickSearchEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("QuickSearchEntry([REDACTED])")
    }
}

impl RezkaClient {
    pub async fn premium_status(&mut self) -> Result<PremiumStatus, RezkaError> {
        let url = self
            .transport_mut()
            .selected_origin()
            .join("/")
            .map_err(|_| invalid_discovery("invalid premium status endpoint"))?;
        let response = self
            .transport_mut()
            .get_first_with_failover(url, None)
            .await?;
        reject_access_page(&response.body, "premium status")?;
        parse_premium_status(&response.body)
    }

    pub async fn quick_search(
        &mut self,
        query: &QuickSearchQuery,
    ) -> Result<Vec<QuickSearchEntry>, RezkaError> {
        let url = quick_search_url(self.transport_mut().selected_origin(), query)?;
        let response = self
            .transport_mut()
            .get_first_with_failover(url, None)
            .await?;
        reject_access_page(&response.body, "quick search")?;
        let selected_origin = self.transport_mut().selected_origin().clone();
        parse_quick_search(&response.body, query, &selected_origin)
    }
}

pub fn parse_premium_status(html: &str) -> Result<PremiumStatus, RezkaError> {
    ensure_bounded_html(html)?;
    let premium =
        Selector::parse("body.b-premium_user__body").expect("static premium selector is valid");
    let document = Html::parse_document(html);
    Ok(if document.select(&premium).next().is_some() {
        PremiumStatus::Active
    } else {
        PremiumStatus::Inactive
    })
}

pub fn parse_quick_search(
    html: &str,
    _query: &QuickSearchQuery,
    origin: &Url,
) -> Result<Vec<QuickSearchEntry>, RezkaError> {
    ensure_bounded_html(html)?;
    let section = Selector::parse("div.b-search__live_section")
        .expect("static quick search section selector is valid");
    let item = Selector::parse("ul > li").expect("static quick search item selector is valid");
    let link = Selector::parse("a[href]").expect("static link selector is valid");
    let title = Selector::parse(".b-search__section_list_title")
        .expect("static quick search title selector is valid");
    let info = Selector::parse(".b-search__section_list_info")
        .expect("static quick search info selector is valid");
    let document = Html::parse_document(html);
    let mut sections = document.select(&section);
    let section = sections
        .next()
        .ok_or_else(|| invalid_discovery("quick search section missing"))?;
    if sections.next().is_some() {
        return Err(invalid_discovery("ambiguous quick search sections"));
    }

    let mut entries = Vec::new();
    for element in section.select(&item) {
        if entries.len() == MAX_QUICK_SEARCH_ENTRIES {
            return Err(invalid_discovery("quick search result limit exceeded"));
        }
        entries.push(parse_entry(element, origin, &link, &title, &info)?);
    }
    Ok(entries)
}

fn parse_entry(
    element: ElementRef<'_>,
    origin: &Url,
    link_selector: &Selector,
    title_selector: &Selector,
    info_selector: &Selector,
) -> Result<QuickSearchEntry, RezkaError> {
    let mut links = element.select(link_selector);
    let link = links
        .next()
        .ok_or_else(|| invalid_discovery("quick search link missing"))?;
    if links.next().is_some() {
        return Err(invalid_discovery("ambiguous quick search links"));
    }

    let href = link
        .value()
        .attr("href")
        .ok_or_else(|| invalid_discovery("quick search locator missing"))?;
    let resolved = origin
        .join(href)
        .map_err(|_| invalid_discovery("invalid quick search locator"))?;
    if !same_origin(origin, &resolved)
        || resolved.query().is_some()
        || resolved.fragment().is_some()
    {
        return Err(invalid_discovery("invalid quick search locator"));
    }
    let locator = TitleLocator::new(resolved.path())?;

    let title = unique_text(&link, title_selector, MAX_QUICK_SEARCH_TITLE_BYTES, true)?
        .ok_or_else(|| invalid_discovery("quick search title missing"))?;
    let info = unique_text(&link, info_selector, MAX_QUICK_SEARCH_INFO_BYTES, false)?;

    Ok(QuickSearchEntry {
        title,
        info,
        locator,
    })
}

fn unique_text(
    parent: &ElementRef<'_>,
    selector: &Selector,
    max_bytes: usize,
    required: bool,
) -> Result<Option<String>, RezkaError> {
    let mut values = parent.select(selector);
    let Some(value) = values.next() else {
        return if required {
            Err(invalid_discovery("quick search field missing"))
        } else {
            Ok(None)
        };
    };
    if values.next().is_some() {
        return Err(invalid_discovery("ambiguous quick search field"));
    }
    let normalized = normalize_text(value.text());
    if normalized.is_empty() || normalized.len() > max_bytes {
        return Err(invalid_discovery("invalid quick search field"));
    }
    Ok(Some(normalized))
}

fn quick_search_url(origin: &Url, query: &QuickSearchQuery) -> Result<Url, RezkaError> {
    let mut url = origin
        .join("/engine/ajax/search.php")
        .map_err(|_| invalid_discovery("invalid quick search endpoint"))?;
    url.query_pairs_mut().append_pair("q", query.as_str());
    Ok(url)
}

fn reject_access_page(html: &str, operation: &'static str) -> Result<(), RezkaError> {
    if detect_challenge(html) {
        return Err(RezkaError::ChallengeRequired {
            context: sanitize_provider_text(operation),
        });
    }

    let title = Selector::parse("title").expect("static title selector is valid");
    let page_title = Html::parse_document(html)
        .select(&title)
        .next()
        .map(|element| normalize_text(element.text()));
    match page_title.as_deref() {
        Some("Sign In") => Err(RezkaError::AuthenticationRequired {
            context: sanitize_provider_text(operation),
        }),
        Some("Verify") => Err(RezkaError::ChallengeRequired {
            context: sanitize_provider_text(operation),
        }),
        _ => Ok(()),
    }
}

fn ensure_bounded_html(html: &str) -> Result<(), RezkaError> {
    if html.len() > MAX_PROVIDER_RESPONSE_BODY_BYTES {
        return Err(invalid_discovery("discovery response exceeds limit"));
    }
    Ok(())
}

fn normalize_text<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_visible_scalar(character: char) -> bool {
    !matches!(
        character,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'
            | '\u{1160}'
            | '\u{17b4}'
            | '\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{e0100}'..='\u{e01ef}'
    )
}

fn invalid_discovery(reason: &'static str) -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text(reason),
    }
}
