use std::fmt;

use reqwest::StatusCode;
use scraper::{Html, Selector};
use url::Url;

use crate::{
    ProviderFailureReason, PublicImageUrl, RezkaError,
    playback::{SelectedTranslation, TitlePlaybackRef},
    redaction::{sanitize_provider_text, trusted_internal_text},
    session::{RezkaClient, anubis::detect_challenge},
};

pub mod parser;

pub const MAX_CATALOG_ENTRIES: usize = 64;
const MAX_CATALOG_QUERY_SCALARS: usize = 200;
const MAX_CATALOG_QUERY_BYTES: usize = 512;
const MAX_TITLE_LOCATOR_BYTES: usize = 2_048;
const MAX_CATALOG_CONTINUATION_BYTES: usize = 2_048;
const MAX_CATALOG_SLUG_BYTES: usize = 64;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum CatalogCategory {
    Films,
    Series,
    Cartoons,
    Animation,
}

impl CatalogCategory {
    const fn path(self) -> &'static str {
        match self {
            Self::Films => "films",
            Self::Series => "series",
            Self::Cartoons => "cartoons",
            Self::Animation => "animation",
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum CatalogSort {
    New,
    Popular,
}

#[derive(Clone, Eq, PartialEq, Hash)]
pub struct CatalogSlug(String);

impl CatalogSlug {
    pub fn new(value: &str) -> Result<Self, RezkaError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_CATALOG_SLUG_BYTES
            && value
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-')
            && !value.contains("--");
        if !valid {
            return Err(invalid_catalog("invalid catalog slug"));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CatalogSlug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CatalogSlug([REDACTED])")
    }
}

#[derive(Debug, Clone)]
enum CatalogFilter {
    Country(CatalogSlug),
    Year(u16),
}

#[derive(Debug, Clone)]
pub struct CatalogBrowse {
    category: CatalogCategory,
    filter: Option<CatalogFilter>,
    sort: Option<CatalogSort>,
}

impl CatalogBrowse {
    #[must_use]
    pub const fn new(category: CatalogCategory) -> Self {
        Self {
            category,
            filter: None,
            sort: None,
        }
    }

    pub fn with_country(mut self, country: CatalogSlug) -> Result<Self, RezkaError> {
        self.set_filter(CatalogFilter::Country(country))?;
        Ok(self)
    }

    pub fn with_year(mut self, year: u16) -> Result<Self, RezkaError> {
        if !(1895..=9999).contains(&year) {
            return Err(invalid_catalog("invalid catalog year"));
        }
        self.set_filter(CatalogFilter::Year(year))?;
        Ok(self)
    }

    #[must_use]
    pub const fn with_sort(mut self, sort: CatalogSort) -> Self {
        self.sort = Some(sort);
        self
    }

    fn set_filter(&mut self, filter: CatalogFilter) -> Result<(), RezkaError> {
        if self.filter.is_some() {
            return Err(invalid_catalog("conflicting catalog filters"));
        }
        self.filter = Some(filter);
        Ok(())
    }

    pub fn url(&self, origin: &Url) -> Result<Url, RezkaError> {
        let category = self.category.path();
        let path = match &self.filter {
            Some(CatalogFilter::Country(country)) => {
                format!("/{category}/country/{}/", country.as_str())
            }
            Some(CatalogFilter::Year(year)) => format!("/{category}/year/{year}/"),
            None => format!("/{category}/"),
        };
        let mut url = origin
            .join(&path)
            .map_err(|_| invalid_catalog("invalid catalog browse endpoint"))?;
        if let Some(sort) = self.sort {
            url.query_pairs_mut().append_pair(
                "filter",
                match sort {
                    CatalogSort::New => "last",
                    CatalogSort::Popular => "popular",
                },
            );
        }
        Ok(url)
    }
}

pub struct CatalogQuery(String);

impl CatalogQuery {
    pub fn new(value: &str) -> Result<Self, RezkaError> {
        let normalized = value.trim();
        let valid = !normalized.is_empty()
            && normalized.chars().all(|character| !character.is_control())
            && normalized.chars().any(is_visible_catalog_scalar)
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

#[derive(Clone)]
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

pub struct CatalogContinuation {
    target: String,
    query: CatalogQuery,
}

impl CatalogContinuation {
    pub fn new(value: &str, query: &str) -> Result<Self, RezkaError> {
        let valid = value.starts_with('/')
            && !value.starts_with("//")
            && value.len() <= MAX_CATALOG_CONTINUATION_BYTES
            && !value.contains(['\\', '#'])
            && !value.chars().any(char::is_control);
        if !valid {
            return Err(invalid_catalog("invalid catalog continuation"));
        }
        Self::from_normalized(value.to_owned(), &CatalogQuery::new(query)?)
    }

    pub(crate) fn from_normalized(value: String, query: &CatalogQuery) -> Result<Self, RezkaError> {
        if value.len() > MAX_CATALOG_CONTINUATION_BYTES {
            return Err(invalid_catalog("catalog continuation exceeds limit"));
        }

        Ok(Self {
            target: value,
            query: CatalogQuery(query.as_str().to_owned()),
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.target
    }
}

impl fmt::Debug for CatalogContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CatalogContinuation([REDACTED])")
    }
}

impl RezkaClient {
    pub async fn browse(&mut self, browse: &CatalogBrowse) -> Result<CatalogPage, RezkaError> {
        let url = browse.url(self.transport_mut().selected_origin())?;
        let response = self
            .transport_mut()
            .get_first_with_failover(url, None)
            .await?;
        reject_catalog_access_page(&response.body)?;
        let selected_origin = self.transport_mut().selected_origin().clone();
        parser::parse_browse_page(&response.body, &selected_origin)
    }

    pub async fn search(&mut self, query: &CatalogQuery) -> Result<CatalogPage, RezkaError> {
        let url = initial_search_url(self.transport_mut().selected_origin(), query)?;
        self.fetch_catalog_page(url, query).await
    }

    pub async fn search_next(
        &mut self,
        continuation: &CatalogContinuation,
    ) -> Result<CatalogPage, RezkaError> {
        let url = self
            .transport_mut()
            .selected_origin()
            .join(continuation.as_str())
            .map_err(|_| invalid_catalog("invalid catalog continuation"))?;
        self.fetch_catalog_page(url, &continuation.query).await
    }

    pub async fn title(&mut self, locator: &TitleLocator) -> Result<TitleDetails, RezkaError> {
        let url = self
            .transport_mut()
            .selected_origin()
            .join(locator.as_str())
            .map_err(|_| invalid_catalog("invalid title endpoint"))?;
        let response = self
            .transport_mut()
            .get_first_with_failover_accepting(url, None)
            .await?;

        reject_title_access_page(&response.body)?;
        if response.status.is_redirection() {
            return Err(invalid_catalog("title redirect rejected"));
        }
        if matches!(response.status, StatusCode::NOT_FOUND | StatusCode::GONE) {
            return Err(RezkaError::TitleNotFound {
                context: sanitize_provider_text("title not found"),
            });
        }
        if response.status != StatusCode::OK {
            return Err(invalid_catalog("invalid title HTTP status"));
        }

        let selected_origin = self.transport_mut().selected_origin().clone();
        parser::parse_title_page(&response.body, locator, &selected_origin)
    }

    async fn fetch_catalog_page(
        &mut self,
        url: Url,
        query: &CatalogQuery,
    ) -> Result<CatalogPage, RezkaError> {
        let response = self
            .transport_mut()
            .get_first_with_failover(url, None)
            .await?;
        reject_catalog_access_page(&response.body)?;
        let selected_origin = self.transport_mut().selected_origin().clone();

        parser::parse_catalog_page(&response.body, query, &selected_origin)
    }
}

fn initial_search_url(origin: &Url, query: &CatalogQuery) -> Result<Url, RezkaError> {
    let mut url = origin
        .join("/search/")
        .map_err(|_| invalid_catalog("invalid catalog search endpoint"))?;
    url.query_pairs_mut()
        .append_pair("do", "search")
        .append_pair("subaction", "search")
        .append_pair("q", query.as_str());
    Ok(url)
}

fn reject_catalog_access_page(html: &str) -> Result<(), RezkaError> {
    if detect_challenge(html) {
        return Err(RezkaError::ChallengeRequired {
            context: sanitize_provider_text("catalog challenge required"),
        });
    }

    let selector = Selector::parse("title").expect("static title selector is valid");
    let title = Html::parse_document(html)
        .select(&selector)
        .next()
        .map(|element| element.text().collect::<String>())
        .map(|value| value.trim().to_owned());
    match title.as_deref() {
        Some("Sign In") => Err(RezkaError::AuthenticationRequired {
            context: sanitize_provider_text("catalog authentication required"),
        }),
        Some("Verify") => Err(RezkaError::ChallengeRequired {
            context: sanitize_provider_text("catalog verification required"),
        }),
        _ => Ok(()),
    }
}

fn reject_title_access_page(html: &str) -> Result<(), RezkaError> {
    if detect_challenge(html) {
        return Err(RezkaError::ChallengeRequired {
            context: sanitize_provider_text("title challenge required"),
        });
    }

    let document = Html::parse_document(html);
    let title_selector = Selector::parse("title").expect("static title selector is valid");
    let title = document
        .select(&title_selector)
        .next()
        .map(|element| element.text().collect::<String>())
        .map(|value| value.trim().to_owned());
    match title.as_deref() {
        Some("Sign In") => {
            return Err(RezkaError::AuthenticationRequired {
                context: sanitize_provider_text("title authentication required"),
            });
        }
        Some("Verify") => {
            return Err(RezkaError::ChallengeRequired {
                context: sanitize_provider_text("title verification required"),
            });
        }
        _ => {}
    }

    let restricted_selector = Selector::parse(".b-player__restricted__block_message")
        .expect("static restricted selector is valid");
    for restricted in document.select(&restricted_selector) {
        let message = restricted
            .descendants()
            .filter_map(|descendant| {
                let text = descendant.value().as_text()?;
                let inside_suggestion = descendant
                    .ancestors()
                    .take_while(|ancestor| ancestor.id() != restricted.id())
                    .filter_map(scraper::ElementRef::wrap)
                    .any(|element| {
                        element
                            .value()
                            .classes()
                            .any(|class| class == "b-restricted__suggest")
                    });
                (!inside_suggestion).then_some(&**text)
            })
            .collect::<String>();
        if !message.trim().is_empty() {
            return Err(RezkaError::TranslationUnavailable {
                reason: ProviderFailureReason::Restricted,
            });
        }
    }

    Ok(())
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

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum RezkaMediaKind {
    Movie,
    Series,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum SeriesLifecycleStatus {
    Completed,
    Ongoing,
    Unknown,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RezkaTitleId(u64);

impl RezkaTitleId {
    pub fn new(value: u64) -> Result<Self, RezkaError> {
        if value == 0 {
            return Err(invalid_catalog("invalid title ID"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TranslationId(u64);

impl TranslationId {
    pub fn new(value: u64) -> Result<Self, RezkaError> {
        if value == 0 {
            return Err(invalid_catalog("invalid translation ID"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum TranslationKey {
    Movie {
        id: TranslationId,
        is_camrip: bool,
        has_ads: bool,
        is_director: bool,
    },
    Series {
        id: TranslationId,
    },
}

impl TranslationKey {
    #[must_use]
    pub const fn id(self) -> TranslationId {
        match self {
            Self::Movie { id, .. } | Self::Series { id } => id,
        }
    }
}

#[derive(Clone)]
pub struct Translation {
    key: TranslationKey,
    name: String,
    is_premium: bool,
    is_director: bool,
    is_camrip: bool,
    has_ads: bool,
}

impl Translation {
    pub(crate) fn new(
        key: TranslationKey,
        name: String,
        is_premium: bool,
        is_director: bool,
        is_camrip: bool,
        has_ads: bool,
    ) -> Self {
        Self {
            key,
            name,
            is_premium,
            is_director,
            is_camrip,
            has_ads,
        }
    }

    #[must_use]
    pub const fn key(&self) -> &TranslationKey {
        &self.key
    }

    #[must_use]
    pub const fn id(&self) -> TranslationId {
        self.key.id()
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn is_premium(&self) -> bool {
        self.is_premium
    }

    #[must_use]
    pub const fn is_director(&self) -> bool {
        self.is_director
    }

    #[must_use]
    pub const fn is_camrip(&self) -> bool {
        self.is_camrip
    }

    #[must_use]
    pub const fn has_ads(&self) -> bool {
        self.has_ads
    }
}

impl fmt::Debug for Translation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Translation")
            .field("key", &self.key)
            .field("name", &"[REDACTED]")
            .field("is_premium", &self.is_premium)
            .finish_non_exhaustive()
    }
}

pub struct TitleDetails {
    id: RezkaTitleId,
    locator: TitleLocator,
    title: String,
    original_title: Option<String>,
    release_year: Option<u16>,
    kind: RezkaMediaKind,
    series_lifecycle_status: SeriesLifecycleStatus,
    description: Option<String>,
    countries: Vec<String>,
    genres: Vec<String>,
    duration_minutes: Option<u16>,
    age_rating: Option<u8>,
    ratings: Vec<TitleRating>,
    franchise: Vec<FranchiseTitle>,
    thumbnail: Option<PublicImageUrl>,
    translations: Vec<Translation>,
    default_translation: Option<TranslationKey>,
}

impl TitleDetails {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: RezkaTitleId,
        locator: TitleLocator,
        title: String,
        original_title: Option<String>,
        release_year: Option<u16>,
        kind: RezkaMediaKind,
        series_lifecycle_status: SeriesLifecycleStatus,
        description: Option<String>,
        countries: Vec<String>,
        genres: Vec<String>,
        duration_minutes: Option<u16>,
        age_rating: Option<u8>,
        ratings: Vec<TitleRating>,
        franchise: Vec<FranchiseTitle>,
        thumbnail: Option<PublicImageUrl>,
        translations: Vec<Translation>,
        default_translation: Option<TranslationKey>,
    ) -> Self {
        Self {
            id,
            locator,
            title,
            original_title,
            release_year,
            kind,
            series_lifecycle_status,
            description,
            countries,
            genres,
            duration_minutes,
            age_rating,
            ratings,
            franchise,
            thumbnail,
            translations,
            default_translation,
        }
    }

    #[must_use]
    pub const fn id(&self) -> RezkaTitleId {
        self.id
    }

    #[must_use]
    pub const fn locator(&self) -> &TitleLocator {
        &self.locator
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn original_title(&self) -> Option<&str> {
        self.original_title.as_deref()
    }

    #[must_use]
    pub const fn release_year(&self) -> Option<u16> {
        self.release_year
    }

    #[must_use]
    pub const fn kind(&self) -> RezkaMediaKind {
        self.kind
    }

    #[must_use]
    pub const fn series_lifecycle_status(&self) -> SeriesLifecycleStatus {
        self.series_lifecycle_status
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub fn countries(&self) -> &[String] {
        &self.countries
    }

    #[must_use]
    pub fn genres(&self) -> &[String] {
        &self.genres
    }

    #[must_use]
    pub const fn duration_minutes(&self) -> Option<u16> {
        self.duration_minutes
    }

    #[must_use]
    pub const fn age_rating(&self) -> Option<u8> {
        self.age_rating
    }

    #[must_use]
    pub fn ratings(&self) -> &[TitleRating] {
        &self.ratings
    }

    #[must_use]
    pub fn franchise(&self) -> &[FranchiseTitle] {
        &self.franchise
    }

    #[must_use]
    pub const fn thumbnail(&self) -> Option<&PublicImageUrl> {
        self.thumbnail.as_ref()
    }

    #[must_use]
    pub fn translations(&self) -> &[Translation] {
        &self.translations
    }

    #[must_use]
    pub const fn default_translation(&self) -> Option<&TranslationKey> {
        self.default_translation.as_ref()
    }

    pub fn select_translation(
        &self,
        key: &TranslationKey,
    ) -> Result<SelectedTranslation, RezkaError> {
        let translation = self
            .translations
            .iter()
            .find(|translation| translation.key() == key)
            .cloned()
            .ok_or(RezkaError::TranslationUnavailable {
                reason: ProviderFailureReason::TranslationUnavailable,
            })?;
        let title = TitlePlaybackRef::new(self.id, self.locator.clone(), self.kind);
        Ok(SelectedTranslation::new(title, translation))
    }
}

impl fmt::Debug for TitleDetails {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TitleDetails")
            .field("id", &self.id)
            .field("locator", &"[REDACTED]")
            .field("title", &"[REDACTED]")
            .field(
                "original_title",
                &self.original_title.as_ref().map(|_| "[REDACTED]"),
            )
            .field("release_year", &self.release_year)
            .field("kind", &self.kind)
            .field("series_lifecycle_status", &self.series_lifecycle_status)
            .field(
                "description",
                &self.description.as_ref().map(|_| "[REDACTED]"),
            )
            .field("countries", &self.countries.len())
            .field("genres", &self.genres.len())
            .field("duration_minutes", &self.duration_minutes)
            .field("age_rating", &self.age_rating)
            .field("ratings", &self.ratings)
            .field("franchise", &self.franchise.len())
            .field("thumbnail", &self.thumbnail.as_ref().map(|_| "[REDACTED]"))
            .field("translations", &self.translations.len())
            .field("default_translation", &self.default_translation)
            .finish()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum RatingSource {
    Kinopoisk,
    Imdb,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct TitleRating {
    source: RatingSource,
    value: f32,
}

impl TitleRating {
    pub(crate) const fn new(source: RatingSource, value: f32) -> Self {
        Self { source, value }
    }

    #[must_use]
    pub const fn source(self) -> RatingSource {
        self.source
    }

    #[must_use]
    pub const fn value(self) -> f32 {
        self.value
    }
}

pub struct FranchiseTitle {
    locator: TitleLocator,
    title: String,
    is_current: bool,
}

impl FranchiseTitle {
    pub(crate) const fn new(locator: TitleLocator, title: String, is_current: bool) -> Self {
        Self {
            locator,
            title,
            is_current,
        }
    }

    #[must_use]
    pub const fn locator(&self) -> &TitleLocator {
        &self.locator
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub const fn is_current(&self) -> bool {
        self.is_current
    }
}

impl fmt::Debug for FranchiseTitle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FranchiseTitle")
            .field("locator", &"[REDACTED]")
            .field("title", &"[REDACTED]")
            .field("is_current", &self.is_current)
            .finish()
    }
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

pub(crate) fn invalid_catalog(reason: &'static str) -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: trusted_internal_text(reason),
    }
}

pub(crate) fn path_has_prohibited_segment(path: &str) -> bool {
    if has_malformed_percent_encoding(path) {
        return true;
    }

    let mut candidate = path.to_owned();
    loop {
        if candidate.contains('\\')
            || candidate.chars().any(char::is_control)
            || candidate
                .split('/')
                .any(|segment| matches!(segment, "." | ".."))
        {
            return true;
        }
        if !contains_percent_encoded_byte(&candidate) {
            return false;
        }
        let Some(next) = decode_percent_once(&candidate) else {
            return true;
        };
        candidate = next;
    }
}

pub(crate) fn has_malformed_percent_encoding(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if decode_hex(bytes.get(index + 1).copied()).is_none()
            || decode_hex(bytes.get(index + 2).copied()).is_none()
        {
            return true;
        }
        index += 3;
    }

    false
}

fn is_visible_catalog_scalar(character: char) -> bool {
    !matches!(
        character as u32,
        0x00ad
            | 0x034f
            | 0x0600..=0x0605
            | 0x061c
            | 0x06dd
            | 0x070f
            | 0x0890..=0x0891
            | 0x08e2
            | 0x115f..=0x1160
            | 0x17b4..=0x17b5
            | 0x180b..=0x180f
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x206f
            | 0x2800
            | 0x3164
            | 0xfe00..=0xfe0f
            | 0xfeff
            | 0xffa0
            | 0xfff0..=0xfffb
            | 0x110bd
            | 0x110cd
            | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0000..=0xe0fff
    )
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
    use super::{CatalogContinuation, CatalogQuery, MAX_CATALOG_CONTINUATION_BYTES};

    #[test]
    fn continuation_budget_accepts_the_boundary_and_rejects_the_next_byte() {
        assert!(
            CatalogContinuation::from_normalized(
                "x".repeat(MAX_CATALOG_CONTINUATION_BYTES),
                &CatalogQuery::new("query").unwrap(),
            )
            .is_ok()
        );
        assert!(
            CatalogContinuation::from_normalized(
                "x".repeat(MAX_CATALOG_CONTINUATION_BYTES + 1),
                &CatalogQuery::new("query").unwrap(),
            )
            .is_err()
        );
    }
}
