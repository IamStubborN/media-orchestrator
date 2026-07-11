use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::{
    PublicImageUrl, RezkaError,
    catalog::{
        CatalogContinuation, CatalogEntry, CatalogPage, CatalogQuery, MAX_CATALOG_ENTRIES,
        TitleLocator, invalid_catalog, path_has_prohibited_segment,
    },
    mirror::same_origin,
};

const MAX_NORMALIZED_TEXT_BYTES: usize = 4_096;

pub fn parse_catalog_page(
    html: &str,
    query: &CatalogQuery,
    selected_origin: &Url,
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

    let continuation = parse_continuation(document.select(&next_selector), query, selected_origin)?;
    Ok(CatalogPage::new(entries, continuation))
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
    let thumbnail = item
        .select(image_selector)
        .next()
        .and_then(|image| image.attr("src"))
        .map(|source| parse_thumbnail(source, selected_origin))
        .transpose()?;

    Ok(CatalogEntry::new(
        TitleLocator::new(href)?,
        title,
        description,
        info,
        thumbnail,
    ))
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
            if number.contains('/') || !is_page_number(number) {
                return Err(invalid_catalog("invalid catalog continuation path"));
            }
            (Some(number), true)
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
        (false, None, Some(page)) if is_page_number(&page) => page,
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
    let target = format!("{}?{}", url.path(), serializer.finish());
    CatalogContinuation::from_normalized(target)
}

fn is_page_number(value: &str) -> bool {
    value.as_bytes().iter().all(u8::is_ascii_digit)
        && value.parse::<u64>().is_ok_and(|number| number > 1)
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static catalog selector is valid")
}
