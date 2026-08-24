use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Argument, CallExpression, Expression};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::SourceType;
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::{
    PublicImageUrl, RezkaError,
    catalog::{
        CatalogContinuation, CatalogEntry, CatalogPage, CatalogQuery, FranchiseTitle,
        MAX_CATALOG_ENTRIES, RatingSource, RezkaMediaKind, RezkaTitleId, SeriesLifecycleStatus,
        TitleDetails, TitleLocator, TitleRating, Translation, TranslationId, TranslationKey,
        has_malformed_percent_encoding, invalid_catalog, path_has_prohibited_segment,
    },
    mirror::same_origin,
};

const MAX_NORMALIZED_TEXT_BYTES: usize = 4_096;
const MAX_TRANSLATIONS: usize = 128;
const MAX_METADATA_VALUES: usize = 64;
const MAX_FRANCHISE_TITLES: usize = 64;

pub fn parse_catalog_page(
    html: &str,
    query: &CatalogQuery,
    selected_origin: &Url,
) -> Result<CatalogPage, RezkaError> {
    parse_catalog_document(html, selected_origin, Some(query))
}

pub fn parse_browse_page(html: &str, selected_origin: &Url) -> Result<CatalogPage, RezkaError> {
    parse_catalog_document(html, selected_origin, None)
}

fn parse_catalog_document(
    html: &str,
    selected_origin: &Url,
    query: Option<&CatalogQuery>,
) -> Result<CatalogPage, RezkaError> {
    let document = Html::parse_document(html);
    let item_selector = selector("div.b-content__inline_items > div.b-content__inline_item");
    let title_selector = selector(".b-content__inline_item-link > a");
    let description_selector = selector(".b-content__inline_item-desc");
    let info_selector = selector(".b-content__inline_item-info");
    let image_selector = selector("img");
    let next_selector = selector(".b-navigation__next");

    let mut entries = Vec::new();
    for item in document.select(&item_selector) {
        if entries.len() == MAX_CATALOG_ENTRIES {
            return Err(invalid_catalog("catalog entry limit exceeded"));
        }
        entries.push(parse_entry(
            item,
            &title_selector,
            &description_selector,
            &info_selector,
            &image_selector,
            selected_origin,
        )?);
    }

    let continuation = match query {
        Some(query) => parse_continuation(document.select(&next_selector), query, selected_origin)?,
        None => None,
    };
    Ok(CatalogPage::new(entries, continuation))
}

pub fn parse_title_page(
    html: &str,
    locator: &TitleLocator,
    selected_origin: &Url,
) -> Result<TitleDetails, RezkaError> {
    let document = Html::parse_document(html);
    let initializations = parse_player_initializations(&document)?;
    let id = parse_title_id(&document, &initializations, locator)?;
    let kind = parse_media_kind(&document, &initializations)?;
    let title = required_element_text(
        &document,
        ".b-content__main .b-post__title, h1[itemprop=name], .b-post__title",
        "title page missing title",
    )?;
    let original_title = optional_element_text(&document, ".b-post__origtitle")?;
    let release_year = parse_release_year(&document)?;
    let series_lifecycle_status = parse_series_lifecycle_status(&document, kind)?;
    let description = optional_element_text(&document, ".b-post__description_text")?;
    let countries = parse_info_list(&document, &["country", "страна", "країна"])?;
    let genres = parse_info_list(&document, &["genre", "жанр"])?;
    let duration_minutes = parse_duration(&document)?;
    let age_rating = parse_age_rating(&document)?;
    let ratings = parse_ratings(&document)?;
    let franchise = parse_franchise(&document)?;
    let thumbnail = parse_title_thumbnail(&document, selected_origin)?;
    let translations = parse_translations(&document, kind, &initializations)?;
    let default_translation = parse_default_translation(&translations, kind, &initializations)?;

    Ok(TitleDetails::new(
        id,
        TitleLocator::new(locator.as_str())?,
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
    ))
}

fn parse_info_list(document: &Html, labels: &[&str]) -> Result<Vec<String>, RezkaError> {
    let Some(value_cell) = find_info_value(document, labels)? else {
        return Ok(Vec::new());
    };
    let links = value_cell.select(&selector("a")).collect::<Vec<_>>();
    let mut values = Vec::new();
    if links.is_empty() {
        if let Some(value) = normalized_text(value_cell.text())? {
            for part in value.split(',') {
                if let Some(part) = normalized_text(std::iter::once(part))? {
                    push_bounded_metadata(&mut values, part)?;
                }
            }
        }
    } else {
        for link in links {
            if let Some(value) = normalized_text(link.text())? {
                push_bounded_metadata(&mut values, value)?;
            }
        }
    }
    Ok(values)
}

fn push_bounded_metadata(values: &mut Vec<String>, value: String) -> Result<(), RezkaError> {
    if values.len() == MAX_METADATA_VALUES {
        return Err(invalid_catalog("title metadata value limit exceeded"));
    }
    if !values.contains(&value) {
        values.push(value);
    }
    Ok(())
}

fn find_info_value<'a>(
    document: &'a Html,
    labels: &[&str],
) -> Result<Option<ElementRef<'a>>, RezkaError> {
    let row_selector = selector(".b-content__main .b-post__info tr");
    let cell_selector = selector("td");
    for row in document.select(&row_selector) {
        let cells = row.select(&cell_selector).collect::<Vec<_>>();
        if cells.len() < 2 {
            continue;
        }
        let label = normalized_text(cells[0].text())?.unwrap_or_default();
        let label = normalize_info_label(&label);
        if labels.iter().any(|candidate| label == *candidate) {
            return Ok(cells.last().copied());
        }
    }
    Ok(None)
}

fn normalize_info_label(value: &str) -> String {
    value
        .trim()
        .trim_end_matches([':', '：'])
        .trim()
        .to_lowercase()
}

fn parse_duration(document: &Html) -> Result<Option<u16>, RezkaError> {
    let Some(cell) = find_info_value(
        document,
        &["duration", "время", "продолжительность", "тривалість"],
    )?
    else {
        return Ok(None);
    };
    let value = normalized_text(cell.text())?
        .unwrap_or_default()
        .to_lowercase();
    let numbers = decimal_numbers(&value)?;
    let minutes = match numbers.as_slice() {
        [] => return Ok(None),
        [number] if has_hour_marker(&value) => number
            .checked_mul(60)
            .ok_or_else(|| invalid_catalog("invalid title duration"))?,
        [minutes] => *minutes,
        [hours, minutes, ..] => hours
            .checked_mul(60)
            .and_then(|value| value.checked_add(*minutes))
            .ok_or_else(|| invalid_catalog("invalid title duration"))?,
    };
    u16::try_from(minutes)
        .ok()
        .filter(|minutes| *minutes > 0 && *minutes <= 24 * 60)
        .map(Some)
        .ok_or_else(|| invalid_catalog("invalid title duration"))
}

fn has_hour_marker(value: &str) -> bool {
    value.split_whitespace().any(|word| {
        matches!(
            word.trim_matches(|character: char| !character.is_alphabetic()),
            "h" | "hr"
                | "hrs"
                | "hour"
                | "hours"
                | "ч"
                | "час"
                | "часа"
                | "часов"
                | "год"
                | "година"
                | "години"
                | "годин"
        )
    })
}

fn parse_age_rating(document: &Html) -> Result<Option<u8>, RezkaError> {
    let Some(cell) = find_info_value(document, &["age", "возраст", "вік"])? else {
        return Ok(None);
    };
    let value = normalized_text(cell.text())?.unwrap_or_default();
    let Some(age) = decimal_numbers(&value)?.first().copied() else {
        return Ok(None);
    };
    u8::try_from(age)
        .ok()
        .filter(|age| *age <= 21)
        .map(Some)
        .ok_or_else(|| invalid_catalog("invalid title age rating"))
}

fn decimal_numbers(value: &str) -> Result<Vec<u32>, RezkaError> {
    let mut numbers = Vec::new();
    for part in value.split(|character: char| !character.is_ascii_digit()) {
        if part.is_empty() {
            continue;
        }
        if numbers.len() == 4 {
            return Err(invalid_catalog("title numeric metadata limit exceeded"));
        }
        numbers.push(
            part.parse::<u32>()
                .map_err(|_| invalid_catalog("invalid title numeric metadata"))?,
        );
    }
    Ok(numbers)
}

fn parse_ratings(document: &Html) -> Result<Vec<TitleRating>, RezkaError> {
    let selectors = [
        (".b-post__rating_kp .num", RatingSource::Kinopoisk),
        (".b-post__rating_imdb .num", RatingSource::Imdb),
    ];
    let mut ratings = Vec::new();
    for (selector_value, source) in selectors {
        let Some(value) = optional_element_text(document, selector_value)? else {
            continue;
        };
        let value = value
            .replace(',', ".")
            .parse::<f32>()
            .map_err(|_| invalid_catalog("invalid title rating"))?;
        if !value.is_finite() || !(0.0..=10.0).contains(&value) {
            return Err(invalid_catalog("invalid title rating"));
        }
        ratings.push(TitleRating::new(source, value));
    }
    Ok(ratings)
}

fn parse_franchise(document: &Html) -> Result<Vec<FranchiseTitle>, RezkaError> {
    let item_selector = selector(".b-post__partcontent_item[href]");
    let title_selector = selector(".b-post__partcontent_item_title");
    let mut titles = Vec::new();
    for item in document.select(&item_selector) {
        if titles.len() == MAX_FRANCHISE_TITLES {
            return Err(invalid_catalog("franchise title limit exceeded"));
        }
        let href = item
            .attr("href")
            .ok_or_else(|| invalid_catalog("franchise title missing locator"))?;
        let title = item
            .select(&title_selector)
            .next()
            .map(|element| normalized_text(element.text()))
            .transpose()?
            .flatten()
            .or(normalized_text(item.text())?)
            .ok_or_else(|| invalid_catalog("franchise title missing title"))?;
        let is_current = item.value().classes().any(|class| class == "current");
        titles.push(FranchiseTitle::new(
            TitleLocator::new(href)?,
            title,
            is_current,
        ));
    }
    if titles.iter().filter(|title| title.is_current()).count() > 1 {
        return Err(invalid_catalog("multiple current franchise titles"));
    }
    Ok(titles)
}

fn parse_series_lifecycle_status(
    document: &Html,
    kind: RezkaMediaKind,
) -> Result<SeriesLifecycleStatus, RezkaError> {
    if kind != RezkaMediaKind::Series {
        return Ok(SeriesLifecycleStatus::Unknown);
    }

    let row_selector = selector(".b-content__main .b-post__info tr");
    let cell_selector = selector("td");
    let mut found = Vec::new();
    for row in document.select(&row_selector) {
        let cells = row.select(&cell_selector).collect::<Vec<_>>();
        if cells.len() < 2 {
            continue;
        }
        let label = normalized_text(cells[0].text())?.unwrap_or_default();
        if !is_series_status_label(&label) {
            continue;
        }
        let value = normalized_text(cells[1].text())?.unwrap_or_default();
        let Some(status) = normalized_series_status(&value) else {
            return Ok(SeriesLifecycleStatus::Unknown);
        };
        found.push(status);
    }

    let Some(first) = found.first().copied() else {
        return Ok(SeriesLifecycleStatus::Unknown);
    };
    Ok(if found.iter().all(|status| *status == first) {
        first
    } else {
        SeriesLifecycleStatus::Unknown
    })
}

fn is_series_status_label(value: &str) -> bool {
    matches!(
        normalize_status_text(value).as_str(),
        "статус сериала" | "статус серіалу"
    )
}

fn normalized_series_status(value: &str) -> Option<SeriesLifecycleStatus> {
    match normalize_status_text(value).as_str() {
        "завершен" | "завершён" | "завершено" | "завершений" => {
            Some(SeriesLifecycleStatus::Completed)
        }
        "онгоинг" | "продолжается" | "выходит" | "триває" | "виходить" | "продовжується" => {
            Some(SeriesLifecycleStatus::Ongoing)
        }
        _ => None,
    }
}

fn normalize_status_text(value: &str) -> String {
    value
        .trim()
        .trim_end_matches([':', '：'])
        .trim()
        .to_lowercase()
}

#[derive(Copy, Clone)]
struct PlayerInitialization {
    kind: RezkaMediaKind,
    title_id: RezkaTitleId,
    translation_id: TranslationId,
}

fn parse_player_initializations(document: &Html) -> Result<Vec<PlayerInitialization>, RezkaError> {
    let mut found = Vec::new();
    let script_selector = selector("script");
    for script in document.select(&script_selector) {
        let source = script.text().collect::<String>();
        parse_script_player_initializations(&source, &mut found)?;
    }
    Ok(found)
}

fn parse_script_player_initializations(
    source: &str,
    found: &mut Vec<PlayerInitialization>,
) -> Result<(), RezkaError> {
    if !source.contains("initCDNMoviesEvents") && !source.contains("initCDNSeriesEvents") {
        return Ok(());
    }

    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::default()).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() || !parsed.irregular_whitespaces.is_empty()
    {
        return Ok(());
    }

    let mut visitor = PlayerInitializationVisitor::default();
    visitor.visit_program(&parsed.program);
    if let Some(error) = visitor.error {
        return Err(error);
    }
    found.extend(visitor.found);
    Ok(())
}

#[derive(Default)]
struct PlayerInitializationVisitor {
    found: Vec<PlayerInitialization>,
    error: Option<RezkaError>,
}

impl<'a> Visit<'a> for PlayerInitializationVisitor {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if self.error.is_none()
            && let Some(kind) = exact_player_call_kind(call)
        {
            self.capture(call, kind);
        }
        walk::walk_call_expression(self, call);
    }
}

impl PlayerInitializationVisitor {
    fn capture(&mut self, call: &CallExpression<'_>, kind: RezkaMediaKind) {
        let result = (|| {
            let title_id = player_integer_argument(call.arguments.first(), "invalid title ID")?;
            let translation_id =
                player_integer_argument(call.arguments.get(1), "invalid translation ID")?;
            Ok(PlayerInitialization {
                kind,
                title_id: RezkaTitleId::new(title_id)?,
                translation_id: TranslationId::new(translation_id)?,
            })
        })();

        match result {
            Ok(initialization) => self.found.push(initialization),
            Err(error) => self.error = Some(error),
        }
    }
}

fn exact_player_call_kind(call: &CallExpression<'_>) -> Option<RezkaMediaKind> {
    if call.optional {
        return None;
    }
    let Expression::StaticMemberExpression(function) = &call.callee else {
        return None;
    };
    if function.optional {
        return None;
    }
    let kind = match function.property.name.as_str() {
        "initCDNMoviesEvents" => RezkaMediaKind::Movie,
        "initCDNSeriesEvents" => RezkaMediaKind::Series,
        _ => return None,
    };

    let Expression::StaticMemberExpression(tv) = &function.object else {
        return None;
    };
    if tv.optional || tv.property.name != "tv" {
        return None;
    }
    let Expression::Identifier(root) = &tv.object else {
        return None;
    };
    (root.name == "sof").then_some(kind)
}

fn player_integer_argument(
    argument: Option<&Argument<'_>>,
    reason: &'static str,
) -> Result<u64, RezkaError> {
    let Some(Argument::NumericLiteral(literal)) = argument else {
        return Err(invalid_catalog(reason));
    };
    let Some(raw) = literal.raw else {
        return Err(invalid_catalog(reason));
    };
    parse_positive_decimal(raw.as_str(), reason)
}

fn parse_title_id(
    document: &Html,
    initializations: &[PlayerInitialization],
    locator: &TitleLocator,
) -> Result<RezkaTitleId, RezkaError> {
    let sources = [
        ("#post_id[value]", "value"),
        ("#send-video-issue[data-id]", "data-id"),
        ("#user-favorites-holder[data-post_id]", "data-post_id"),
        (".b-userset__fav_holder[data-post_id]", "data-post_id"),
    ];
    let mut candidates = Vec::new();
    for (selector_value, attribute) in sources {
        let source_selector = selector(selector_value);
        for source in document.select(&source_selector) {
            let value = source
                .attr(attribute)
                .ok_or_else(|| invalid_catalog("title ID source missing value"))?;
            candidates.push(RezkaTitleId::new(parse_positive_decimal(
                value,
                "invalid title ID",
            )?)?);
        }
    }
    candidates.extend(
        initializations
            .iter()
            .map(|initialization| initialization.title_id),
    );

    if candidates.is_empty() {
        candidates.push(parse_locator_title_id(locator)?);
    }
    equal_value(candidates, "conflicting title IDs")
}

fn parse_locator_title_id(locator: &TitleLocator) -> Result<RezkaTitleId, RezkaError> {
    let filename = locator
        .as_str()
        .rsplit('/')
        .next()
        .and_then(|value| value.strip_suffix(".html"))
        .ok_or_else(|| invalid_catalog("title ID missing"))?;
    let (prefix, _) = filename
        .split_once('-')
        .ok_or_else(|| invalid_catalog("title ID missing"))?;
    RezkaTitleId::new(parse_positive_decimal(prefix, "title ID missing")?)
}

fn parse_media_kind(
    document: &Html,
    initializations: &[PlayerInitialization],
) -> Result<RezkaMediaKind, RezkaError> {
    let kind_selector = selector("meta[property=\"og:type\"][content]");
    let mut candidates = Vec::new();
    for metadata in document.select(&kind_selector) {
        let kind = match metadata.attr("content") {
            Some("video.movie") => RezkaMediaKind::Movie,
            Some("video.tv_series") => RezkaMediaKind::Series,
            _ => return Err(invalid_catalog("invalid title media kind")),
        };
        candidates.push(kind);
    }
    candidates.extend(
        initializations
            .iter()
            .map(|initialization| initialization.kind),
    );
    equal_value(candidates, "title media kind missing or conflicting")
}

fn parse_translations(
    document: &Html,
    kind: RezkaMediaKind,
    initializations: &[PlayerInitialization],
) -> Result<Vec<Translation>, RezkaError> {
    let translation_selector =
        selector("#translators-list [data-translator_id], .b-translator__item");
    let elements = document.select(&translation_selector);
    let mut translations = Vec::new();
    let mut identities = HashSet::new();
    for element in elements {
        let id = element
            .attr("data-translator_id")
            .ok_or_else(|| invalid_catalog("translation missing ID"))?;
        let id = TranslationId::new(parse_positive_decimal(id, "invalid translation ID")?)?;
        let series_key = (kind == RezkaMediaKind::Series).then_some(TranslationKey::Series { id });
        if series_key.is_some_and(|key| identities.contains(&key)) {
            continue;
        }
        let name = normalized_text(element.text())?
            .ok_or_else(|| invalid_catalog("translation missing name"))?;
        let is_camrip = parse_flag(element.attr("data-camrip"))?;
        let has_ads = parse_flag(element.attr("data-ads"))?;
        let is_director = parse_flag(element.attr("data-director"))?;
        let is_premium = element
            .value()
            .classes()
            .any(|class| class == "b-prem_translator");
        let key = match kind {
            RezkaMediaKind::Movie => TranslationKey::Movie {
                id,
                is_camrip,
                has_ads,
                is_director,
            },
            RezkaMediaKind::Series => TranslationKey::Series { id },
        };
        if !identities.insert(key) {
            return Err(invalid_catalog("duplicate translation identity"));
        }
        if translations.len() == MAX_TRANSLATIONS {
            return Err(invalid_catalog("translation limit exceeded"));
        }
        translations.push(Translation::new(
            key,
            name,
            is_premium,
            is_director,
            is_camrip,
            has_ads,
        ));
    }

    if translations.is_empty() {
        let default_id = equal_value(
            initializations
                .iter()
                .map(|initialization| initialization.translation_id)
                .collect(),
            "default translation missing or conflicting",
        )?;
        let name = parse_displayed_translation_name(document)?
            .ok_or_else(|| invalid_catalog("translation missing"))?;
        let key = match kind {
            RezkaMediaKind::Movie => TranslationKey::Movie {
                id: default_id,
                is_camrip: false,
                has_ads: false,
                is_director: false,
            },
            RezkaMediaKind::Series => TranslationKey::Series { id: default_id },
        };
        translations.push(Translation::new(key, name, false, false, false, false));
    }

    Ok(translations)
}

fn parse_default_translation(
    translations: &[Translation],
    kind: RezkaMediaKind,
    initializations: &[PlayerInitialization],
) -> Result<Option<TranslationKey>, RezkaError> {
    if initializations.is_empty() {
        return Ok(None);
    }
    let default_id = equal_value(
        initializations
            .iter()
            .map(|initialization| initialization.translation_id)
            .collect(),
        "conflicting default translation IDs",
    )?;
    let matches = translations
        .iter()
        .filter(|translation| translation.id() == default_id)
        .map(|translation| *translation.key())
        .collect::<Vec<_>>();
    match kind {
        RezkaMediaKind::Movie if matches.len() == 1 => Ok(matches.first().copied()),
        RezkaMediaKind::Series => Ok(matches.first().copied()),
        RezkaMediaKind::Movie => Ok(None),
    }
}

fn parse_displayed_translation_name(document: &Html) -> Result<Option<String>, RezkaError> {
    let row_selector = selector(".b-post__info tr");
    let cell_selector = selector("td");
    for row in document.select(&row_selector) {
        let cells = row.select(&cell_selector).collect::<Vec<_>>();
        let Some(label) = cells.first() else {
            continue;
        };
        let label = normalized_text(label.text())?.unwrap_or_default();
        if label.to_lowercase().contains("переводе") {
            return cells
                .last()
                .map(|cell| normalized_text(cell.text()))
                .transpose()
                .map(Option::flatten);
        }
    }
    Ok(None)
}

fn parse_flag(value: Option<&str>) -> Result<bool, RezkaError> {
    match value {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(_) => Err(invalid_catalog("invalid translation flag")),
    }
}

fn parse_release_year(document: &Html) -> Result<Option<u16>, RezkaError> {
    let year_selector = selector(".b-content__main .b-post__info a[href*=\"/year/\"]");
    let Some(href) = document
        .select(&year_selector)
        .next()
        .and_then(|element| element.attr("href"))
    else {
        return Ok(None);
    };
    let year = href
        .as_bytes()
        .windows(4)
        .find(|window| window.iter().all(u8::is_ascii_digit))
        .and_then(|window| std::str::from_utf8(window).ok())
        .and_then(|value| value.parse::<u16>().ok());
    Ok(year)
}

fn parse_title_thumbnail(
    document: &Html,
    selected_origin: &Url,
) -> Result<Option<PublicImageUrl>, RezkaError> {
    let cover_selector = selector(".b-content__main .b-sidecover a[href]");
    let image_selector = selector(".b-content__main .b-sidecover img[src]");
    let source = document
        .select(&cover_selector)
        .next()
        .and_then(|element| element.attr("href"))
        .or_else(|| {
            document
                .select(&image_selector)
                .next()
                .and_then(|element| element.attr("src"))
        });
    // Thumbnails are optional: a non-public or malformed cover URL degrades to None rather than
    // failing the whole title page.
    Ok(source.and_then(|source| parse_thumbnail(source, selected_origin).ok()))
}

fn required_element_text(
    document: &Html,
    selector_value: &str,
    missing_reason: &'static str,
) -> Result<String, RezkaError> {
    optional_element_text(document, selector_value)?.ok_or_else(|| invalid_catalog(missing_reason))
}

fn optional_element_text(
    document: &Html,
    selector_value: &str,
) -> Result<Option<String>, RezkaError> {
    document
        .select(&selector(selector_value))
        .next()
        .map(|element| normalized_text(element.text()))
        .transpose()
        .map(Option::flatten)
}

fn parse_positive_decimal(value: &str, reason: &'static str) -> Result<u64, RezkaError> {
    let value = value.trim();
    if value.is_empty() || !value.as_bytes().iter().all(u8::is_ascii_digit) {
        return Err(invalid_catalog(reason));
    }
    let parsed = value.parse::<u64>().map_err(|_| invalid_catalog(reason))?;
    if parsed == 0 {
        return Err(invalid_catalog(reason));
    }
    Ok(parsed)
}

fn equal_value<T: Copy + Eq>(values: Vec<T>, reason: &'static str) -> Result<T, RezkaError> {
    let Some(first) = values.first().copied() else {
        return Err(invalid_catalog(reason));
    };
    if values.iter().all(|value| *value == first) {
        Ok(first)
    } else {
        Err(invalid_catalog(reason))
    }
}

fn parse_entry(
    item: ElementRef<'_>,
    title_selector: &Selector,
    description_selector: &Selector,
    info_selector: &Selector,
    image_selector: &Selector,
    selected_origin: &Url,
) -> Result<CatalogEntry, RezkaError> {
    let Some(link) = item.select(title_selector).next() else {
        return Err(invalid_catalog("catalog result missing title link"));
    };
    let Some(href) = link.attr("href") else {
        return Err(invalid_catalog("catalog result missing title locator"));
    };
    let Some(title) = normalized_text(link.text())? else {
        return Err(invalid_catalog("catalog result missing title"));
    };

    let description = item
        .select(description_selector)
        .next()
        .map(|element| normalized_text(element.text()))
        .transpose()?
        .flatten();
    let info = item
        .select(info_selector)
        .next()
        .map(|element| normalized_text(element.text()))
        .transpose()?
        .flatten();
    // Thumbnails are optional: a non-public or malformed poster URL degrades to None rather than
    // failing the whole catalog page.
    let thumbnail = item
        .select(image_selector)
        .next()
        .and_then(|image| image.attr("src"))
        .and_then(|source| parse_thumbnail(source, selected_origin).ok());

    Ok(CatalogEntry::new(
        parse_catalog_title_locator(href, selected_origin)?,
        title,
        description,
        info,
        thumbnail,
    ))
}

fn parse_catalog_title_locator(
    href: &str,
    selected_origin: &Url,
) -> Result<TitleLocator, RezkaError> {
    let locator = match Url::parse(href) {
        Ok(url)
            if same_origin(&url, selected_origin)
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none() =>
        {
            url.path().to_owned()
        }
        Ok(_) => return Err(invalid_catalog("invalid title locator")),
        Err(url::ParseError::RelativeUrlWithoutBase) => href.to_owned(),
        Err(_) => return Err(invalid_catalog("invalid title locator")),
    };
    TitleLocator::new(&locator)
}

fn parse_thumbnail(source: &str, selected_origin: &Url) -> Result<PublicImageUrl, RezkaError> {
    let url = Url::parse(source)
        .or_else(|_| selected_origin.join(source))
        .map_err(|_| invalid_catalog("invalid catalog thumbnail"))?;
    PublicImageUrl::new(url)
}

fn normalized_text<'a>(text: impl Iterator<Item = &'a str>) -> Result<Option<String>, RezkaError> {
    let mut normalized = String::new();
    for fragment in text {
        for word in fragment.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push_str(word);
            if normalized.len() > MAX_NORMALIZED_TEXT_BYTES {
                return Err(invalid_catalog("catalog text exceeds limit"));
            }
        }
    }

    Ok((!normalized.is_empty()).then_some(normalized))
}

fn parse_continuation<'a>(
    next_elements: impl Iterator<Item = ElementRef<'a>>,
    query: &CatalogQuery,
    selected_origin: &Url,
) -> Result<Option<CatalogContinuation>, RezkaError> {
    let mut continuation = None;
    for next in next_elements {
        let Some(anchor) = closest_anchor(next) else {
            return Err(invalid_catalog("catalog next link missing anchor"));
        };
        let Some(href) = anchor.attr("href") else {
            return Err(invalid_catalog("catalog next link missing href"));
        };
        let candidate = parse_continuation_target(href, query, selected_origin)?;
        if continuation
            .as_ref()
            .is_some_and(|existing: &CatalogContinuation| existing.as_str() != candidate.as_str())
        {
            return Err(invalid_catalog("catalog next links disagree"));
        }
        continuation = Some(candidate);
    }

    Ok(continuation)
}

fn closest_anchor(mut element: ElementRef<'_>) -> Option<ElementRef<'_>> {
    loop {
        if element.value().name() == "a" {
            return Some(element);
        }
        element = ElementRef::wrap(element.parent()?)?;
    }
}

fn parse_continuation_target(
    href: &str,
    query: &CatalogQuery,
    selected_origin: &Url,
) -> Result<CatalogContinuation, RezkaError> {
    let raw_path = href.split(['?', '#']).next().unwrap_or_default();
    if href.starts_with("//")
        || href.contains('\\')
        || href.chars().any(char::is_control)
        || has_malformed_percent_encoding(href)
        || path_has_prohibited_segment(raw_path)
    {
        return Err(invalid_catalog("invalid catalog continuation"));
    }

    let url = match Url::parse(href) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) => selected_origin
            .join(href)
            .map_err(|_| invalid_catalog("invalid catalog continuation"))?,
        Err(_) => return Err(invalid_catalog("invalid catalog continuation")),
    };
    if !same_origin(&url, selected_origin)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || path_has_prohibited_segment(url.path())
    {
        return Err(invalid_catalog("invalid catalog continuation"));
    }

    let (page, path_form) = match url.path() {
        "/search/" => (None, false),
        path => {
            let Some(number) = path
                .strip_prefix("/search/page/")
                .and_then(|value| value.strip_suffix('/'))
            else {
                return Err(invalid_catalog("invalid catalog continuation path"));
            };
            let Some(page) = canonical_page_number(number) else {
                return Err(invalid_catalog("invalid catalog continuation path"));
            };
            (Some(page), true)
        }
    };

    let Some(raw_query) = url.query() else {
        return Err(invalid_catalog("catalog continuation missing query"));
    };
    let mut do_value = None;
    let mut subaction = None;
    let mut requested_query = None;
    let mut query_page = None;
    for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        let slot = match key.as_ref() {
            "do" => &mut do_value,
            "subaction" => &mut subaction,
            "q" => &mut requested_query,
            "page" => &mut query_page,
            _ => return Err(invalid_catalog("unknown catalog continuation parameter")),
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(invalid_catalog("duplicate catalog continuation parameter"));
        }
    }

    if do_value.as_deref() != Some("search")
        || subaction.as_deref() != Some("search")
        || requested_query.as_deref() != Some(query.as_str())
    {
        return Err(invalid_catalog("catalog continuation query mismatch"));
    }

    let page = match (path_form, page, query_page) {
        (false, None, Some(page)) => canonical_page_number(&page)
            .map(str::to_owned)
            .ok_or_else(|| invalid_catalog("invalid catalog continuation pagination"))?,
        (true, Some(page), None) => page.to_owned(),
        _ => return Err(invalid_catalog("invalid catalog continuation pagination")),
    };

    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("do", "search");
    serializer.append_pair("subaction", "search");
    serializer.append_pair("q", query.as_str());
    if !path_form {
        serializer.append_pair("page", &page);
    }
    let path = if path_form {
        format!("/search/page/{page}/")
    } else {
        url.path().to_owned()
    };
    let target = format!("{path}?{}", serializer.finish());
    CatalogContinuation::from_normalized(target, query)
}

fn canonical_page_number(value: &str) -> Option<&str> {
    if !value.as_bytes().iter().all(u8::is_ascii_digit) {
        return None;
    }

    let canonical = value.trim_start_matches('0');
    (canonical.len() > 1
        || canonical
            .as_bytes()
            .first()
            .is_some_and(|byte| *byte > b'1'))
    .then_some(canonical)
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static catalog selector is valid")
}
