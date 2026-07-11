use rezka_client::{
    CatalogQuery, RezkaErrorCode, TitleLocator, catalog::parser::parse_catalog_page,
};
use url::Url;

const ORIGIN: &str = "https://rezka.test/";

fn query() -> CatalogQuery {
    CatalogQuery::new("query").unwrap()
}

fn origin() -> Url {
    Url::parse(ORIGIN).unwrap()
}

fn page_with_next(hrefs: &[&str]) -> String {
    let links = hrefs
        .iter()
        .map(|href| format!(r#"<a href="{href}"><span class="b-navigation__next">Next</span></a>"#))
        .collect::<String>();

    format!(
        r#"<div class="b-content__inline_items">
              <div class="b-content__inline_item">
                <div class="b-content__inline_item-link"><a href="/films/drama/1-title.html">Title</a></div>
              </div>
            </div>{links}"#
    )
}

fn assert_invalid(result: Result<impl std::fmt::Debug, rezka_client::RezkaError>) {
    let error = result.unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
}

#[test]
fn catalog_query_enforces_normalized_text_and_exact_boundaries() {
    let scalars = "a".repeat(200);
    assert_eq!(CatalogQuery::new(&scalars).unwrap().as_str(), scalars);
    assert_invalid(CatalogQuery::new(&"a".repeat(201)));

    let bytes = format!("{}aa", "€".repeat(170));
    assert_eq!(CatalogQuery::new(&bytes).unwrap().as_str(), bytes);
    assert_invalid(CatalogQuery::new(&format!("{}aaa", "€".repeat(170))));

    assert_eq!(CatalogQuery::new("  query  ").unwrap().as_str(), "query");
    for invalid in ["", " \t\n", "query\u{0000}", "query\u{001f}"] {
        assert_invalid(CatalogQuery::new(invalid));
    }
}

#[test]
fn catalog_query_requires_a_visible_unicode_scalar() {
    for invisible in [
        "\u{00ad}",
        "\u{200b}",
        "\u{2060}",
        "\u{fe0f}",
        "\u{e0100}",
        "\u{200b}\u{2060}\u{feff}",
    ] {
        assert_invalid(CatalogQuery::new(invisible));
    }

    for blank in ["\u{115f}", "\u{2800}", "\u{3164}", "\u{ffa0}"] {
        assert_invalid(CatalogQuery::new(blank));

        let visible_query = format!("query{blank}");
        assert_eq!(
            CatalogQuery::new(&visible_query).unwrap().as_str(),
            visible_query
        );
    }

    let visible_query = "query\u{200b}\u{fe0f}";
    assert_eq!(
        CatalogQuery::new(visible_query).unwrap().as_str(),
        visible_query
    );
}

#[test]
fn title_locator_enforces_exact_boundary_and_rejects_unsafe_paths() {
    let boundary = format!("/{}.html", "a".repeat(2_048 - "/.html".len()));
    assert_eq!(TitleLocator::new(&boundary).unwrap().as_str(), boundary);
    assert_invalid(TitleLocator::new(&format!(
        "/{}.html",
        "a".repeat(2_048 - "/.html".len() + 1)
    )));

    for invalid in [
        "/films/title.html?secret=query",
        "/films/title.html#fragment",
        "/films\\title.html",
        "/films/../title.html",
        "/films/%2e%2e/title.html",
        "/films/%252e%252e/title.html",
        "/films/%2E/title.html",
        "/films/%2e%2e%5cprivate.html",
        "/films/%252e%252e%255cprivate.html",
        "/films/%zz/title.html",
        "/films/title%.html",
        "/films/title%2.html",
        "/films/title",
        "https://foreign.test/films/title.html",
        "/films/title\u{0000}.html",
    ] {
        assert_invalid(TitleLocator::new(invalid));
    }

    let encoded_percent = "/films/100%25-title.html";
    assert_eq!(
        TitleLocator::new(encoded_percent).unwrap().as_str(),
        encoded_percent
    );
}

#[test]
fn fixture_page_preserves_provider_order_and_normalizes_display_fields() {
    let page = parse_catalog_page(
        include_str!("fixtures/catalog_results.html"),
        &query(),
        &origin(),
    )
    .unwrap();

    assert_eq!(page.entries().len(), 2);
    assert_eq!(page.entries()[0].title(), "First title");
    assert_eq!(
        page.entries()[0].locator().as_str(),
        "/films/drama/1-first.html"
    );
    assert_eq!(
        page.entries()[0].description(),
        Some("A spaced description")
    );
    assert_eq!(page.entries()[0].info(), Some("2024 Drama"));
    assert_eq!(
        page.entries()[0].thumbnail().unwrap().url().as_str(),
        "https://images.example.invalid/posters/1.jpg"
    );
    assert_eq!(page.entries()[1].title(), "Second title");
    assert_eq!(
        page.entries()[1].locator().as_str(),
        "/series/drama/2-second.html"
    );
    assert_eq!(
        page.continuation().unwrap().as_str(),
        "/search/?do=search&subaction=search&q=query&page=2"
    );
}

#[test]
fn empty_fixture_is_a_valid_empty_catalog_page() {
    let page = parse_catalog_page(
        include_str!("fixtures/catalog_empty.html"),
        &query(),
        &origin(),
    )
    .unwrap();

    assert!(page.entries().is_empty());
    assert!(page.continuation().is_none());
}

#[test]
fn result_containers_without_valid_entries_fail() {
    assert_invalid(parse_catalog_page(
        include_str!("fixtures/catalog_malformed.html"),
        &query(),
        &origin(),
    ));
}

#[test]
fn entry_budget_is_atomic_at_sixty_four_entries() {
    let entries = (0..64)
        .map(|index| {
            format!(
                r#"<div class="b-content__inline_item"><div class="b-content__inline_item-link"><a href="/films/{index}.html">Title {index}</a></div></div>"#
            )
        })
        .collect::<String>();
    let accepted = format!(r#"<div class="b-content__inline_items">{entries}</div>"#);
    assert_eq!(
        parse_catalog_page(&accepted, &query(), &origin())
            .unwrap()
            .entries()
            .len(),
        64
    );

    let rejected_entries = (0..65)
        .map(|index| {
            format!(
                r#"<div class="b-content__inline_item"><div class="b-content__inline_item-link"><a href="/films/{index}.html">Title {index}</a></div></div>"#
            )
        })
        .collect::<String>();
    let rejected = format!(r#"<div class="b-content__inline_items">{rejected_entries}</div>"#);
    assert_invalid(parse_catalog_page(&rejected, &query(), &origin()));
}

#[test]
fn continuation_accepts_exact_pagination_encodings_and_decoded_query_equality() {
    let query = CatalogQuery::new("query two").unwrap();
    for (href, expected) in [
        (
            "/search/?do=search&subaction=search&q=%71%75%65%72%79%20%74%77%6f&page=2",
            "/search/?do=search&subaction=search&q=query+two&page=2",
        ),
        (
            "/search/page/2/?do=search&subaction=search&q=query+two",
            "/search/page/2/?do=search&subaction=search&q=query+two",
        ),
    ] {
        let page = parse_catalog_page(&page_with_next(&[href]), &query, &origin()).unwrap();
        assert_eq!(page.continuation().unwrap().as_str(), expected);
    }
}

#[test]
fn continuation_accepts_large_page_tokens_and_canonicalizes_leading_zeroes() {
    let large_page = "9".repeat(1_900);
    for (href, expected) in [
        (
            "/search/?do=search&subaction=search&q=query&page=0002".to_owned(),
            "/search/?do=search&subaction=search&q=query&page=2".to_owned(),
        ),
        (
            "/search/page/0002/?do=search&subaction=search&q=query".to_owned(),
            "/search/page/2/?do=search&subaction=search&q=query".to_owned(),
        ),
        (
            format!("/search/?do=search&subaction=search&q=query&page={large_page}"),
            format!("/search/?do=search&subaction=search&q=query&page={large_page}"),
        ),
    ] {
        let page = parse_catalog_page(&page_with_next(&[&href]), &query(), &origin()).unwrap();
        assert_eq!(page.continuation().unwrap().as_str(), expected);
    }
}

#[test]
fn continuation_normalizes_same_origin_absolute_links_and_allows_identical_targets() {
    let relative = "/search/?do=search&subaction=search&q=query&page=2";
    let absolute = "https://rezka.test/search/?do=search&subaction=search&q=query&page=2";
    let page =
        parse_catalog_page(&page_with_next(&[relative, absolute]), &query(), &origin()).unwrap();

    assert_eq!(page.continuation().unwrap().as_str(), relative);
}

#[test]
fn continuation_rejects_unsafe_or_noncanonical_targets() {
    let valid = "/search/?do=search&subaction=search&q=query&page=2";
    for invalid in [
        "/search/?do=search&subaction=search&q=query&page=0",
        "/search/?do=search&subaction=search&q=query&page=1",
        "/search/page/0/?do=search&subaction=search&q=query",
        "/search/page/1/?do=search&subaction=search&q=query",
        "/search/?do=search&do=search&subaction=search&q=query&page=2",
        "/search/?do=search&subaction=search&q=query&page=2&extra=value",
        "/search/?do=search&subaction=search&q=other&page=2",
        "https://foreign.test/search/?do=search&subaction=search&q=query&page=2",
        "/search/%2e%2e/?do=search&subaction=search&q=query&page=2",
        "/search/%252e%252e/?do=search&subaction=search&q=query&page=2",
        "/search%5c/page/2/?do=search&subaction=search&q=query",
        "/search%255c/page/2/?do=search&subaction=search&q=query",
        "/search/page/two/?do=search&subaction=search&q=query",
        "/search/page/2?do=search&subaction=search&q=query",
        "/search/?do=search&subaction=search&q=query&page=2#fragment",
        "/search\\?do=search&subaction=search&q=query&page=2",
    ] {
        assert_invalid(parse_catalog_page(
            &page_with_next(&[invalid]),
            &query(),
            &origin(),
        ));
    }

    assert_invalid(parse_catalog_page(
        &page_with_next(&[valid, "/search/?do=search&subaction=search&q=query&page=3"]),
        &query(),
        &origin(),
    ));
}

#[test]
fn continuation_rejects_malformed_percent_encodings() {
    for (query, href) in [
        (
            "query%zz",
            "/search/?do=search&subaction=search&q=query%zz&page=2",
        ),
        (
            "query%",
            "/search/?do=search&subaction=search&q=query%&page=2",
        ),
        (
            "query%2",
            "/search/?do=search&subaction=search&q=query%2&page=2",
        ),
    ] {
        assert_invalid(parse_catalog_page(
            &page_with_next(&[href]),
            &CatalogQuery::new(query).unwrap(),
            &origin(),
        ));
    }

    for href in [
        "/search/%zz/?do=search&subaction=search&q=query&page=2",
        "/search/%/?do=search&subaction=search&q=query&page=2",
        "/search/%2/?do=search&subaction=search&q=query&page=2",
    ] {
        assert_invalid(parse_catalog_page(
            &page_with_next(&[href]),
            &query(),
            &origin(),
        ));
    }
}

#[test]
fn catalog_debug_and_errors_do_not_leak_provider_data() {
    let query = CatalogQuery::new("query-secret").unwrap();
    let locator = TitleLocator::new("/films/title-secret.html").unwrap();
    let page = parse_catalog_page(
        &page_with_next(&["/search/?do=search&subaction=search&q=query-secret&page=2"]),
        &query,
        &origin(),
    )
    .unwrap();
    let debug = format!("{query:?} {locator:?} {page:?}");
    for forbidden in ["query-secret", "title-secret", "https://", "rezka.test"] {
        assert!(
            !debug.contains(forbidden),
            "Debug leaked {forbidden}: {debug}"
        );
    }

    let error = parse_catalog_page(
        &page_with_next(&[
            "https://foreign-secret.test/search/?do=search&subaction=search&q=query-secret&page=2",
        ]),
        &query,
        &origin(),
    )
    .unwrap_err();
    let rendered = format!("{error:?}: {error}");
    for forbidden in ["query-secret", "foreign-secret", "https://"] {
        assert!(
            !rendered.contains(forbidden),
            "error leaked {forbidden}: {rendered}"
        );
    }
}
