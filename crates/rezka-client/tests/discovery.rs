use rezka_client::{
    PremiumStatus, QuickSearchQuery, RezkaErrorCode,
    discovery::{MAX_QUICK_SEARCH_ENTRIES, parse_premium_status, parse_quick_search},
    mirror::MirrorSet,
    session::{RezkaClient, RezkaClientConfig},
};
use time::Duration;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

const ORIGIN: &str = "https://rezka.test/";

fn origin() -> Url {
    Url::parse(ORIGIN).unwrap()
}

fn query() -> QuickSearchQuery {
    QuickSearchQuery::new("query").unwrap()
}

fn client(origin: Url) -> RezkaClient {
    RezkaClient::new(RezkaClientConfig {
        mirrors: MirrorSet::new(vec![origin]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: 0,
        anubis_max_nonce: 1,
    })
    .unwrap()
}

fn assert_invalid(result: Result<impl std::fmt::Debug, rezka_client::RezkaError>) {
    let error = result.unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
    assert!(!format!("{error:?}").contains(ORIGIN));
    assert!(!error.to_string().contains(ORIGIN));
}

#[test]
fn premium_status_depends_on_the_premium_body_marker() {
    assert_eq!(
        parse_premium_status(r#"<body class="layout b-premium_user__body theme">"#).unwrap(),
        PremiumStatus::Active
    );
    assert_eq!(
        parse_premium_status(r#"<body class="layout">"#).unwrap(),
        PremiumStatus::Inactive
    );
}

#[tokio::test]
async fn premium_status_fetches_the_selected_origin_root() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("user-agent", "media-orchestrator-test"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body class="b-premium_user__body"></body></html>"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = client(Url::parse(&server.uri()).unwrap());
    assert_eq!(
        client.premium_status().await.unwrap(),
        PremiumStatus::Active
    );
}

#[tokio::test]
async fn quick_search_uses_the_ajax_endpoint_and_encoded_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/engine/ajax/search.php"))
        .and(query_param("q", "title & sequel"))
        .and(header("user-agent", "media-orchestrator-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<div class="b-search__live_section"><ul><li><a href="/films/1-title.html"><span class="b-search__section_list_title">Title</span></a></li></ul></div>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = client(Url::parse(&server.uri()).unwrap());
    let entries = client
        .quick_search(&QuickSearchQuery::new("title & sequel").unwrap())
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title(), "Title");
}

#[test]
fn quick_search_query_enforces_normalized_exact_boundaries() {
    let scalars = "a".repeat(200);
    assert_eq!(QuickSearchQuery::new(&scalars).unwrap().as_str(), scalars);
    assert_invalid(QuickSearchQuery::new(&"a".repeat(201)));

    let bytes = format!("{}aa", "€".repeat(170));
    assert_eq!(QuickSearchQuery::new(&bytes).unwrap().as_str(), bytes);
    assert_invalid(QuickSearchQuery::new(&format!("{}aaa", "€".repeat(170))));

    assert_eq!(
        QuickSearchQuery::new("  query  ").unwrap().as_str(),
        "query"
    );
    for invalid in ["", " \t\n", "query\u{0000}", "\u{200b}\u{2060}"] {
        assert_invalid(QuickSearchQuery::new(invalid));
    }
}

#[test]
fn quick_search_parses_provider_order_and_normalizes_text() {
    let html = r#"
        <div class="b-search__live_section"><ul>
          <li><a href="/films/drama/1-first.html">
            <span class="b-search__section_list_title"> First   title </span>
            <span class="b-search__section_list_info"> 2024,   Drama </span>
          </a></li>
          <li><a href="https://rezka.test/series/2-second.html">
            <span class="b-search__section_list_title">Second title</span>
          </a></li>
        </ul></div>"#;

    let entries = parse_quick_search(html, &query(), &origin()).unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].title(), "First title");
    assert_eq!(entries[0].info(), Some("2024, Drama"));
    assert_eq!(entries[0].locator().as_str(), "/films/drama/1-first.html");
    assert_eq!(entries[1].title(), "Second title");
    assert_eq!(entries[1].info(), None);
    assert_eq!(entries[1].locator().as_str(), "/series/2-second.html");
}

#[test]
fn quick_search_accepts_the_legacy_live_result_fields() {
    let html = r#"
        <div class="b-search__live_section"><ul><li>
          <a href="/animation/comedy/1-show.html">
            <span class="enty">Show title</span>
            <span class="rating">8.7</span>
          </a>
        </li></ul></div>"#;

    let entries = parse_quick_search(html, &query(), &origin()).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title(), "Show title");
    assert_eq!(entries[0].info(), Some("8.7"));
}

#[test]
fn empty_quick_search_section_is_valid() {
    assert!(
        parse_quick_search(
            r#"<div class="b-search__live_section"><ul></ul></div>"#,
            &query(),
            &origin(),
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn malformed_or_foreign_quick_search_entries_fail_atomically() {
    for html in [
        r#"<div class="b-search__live_section"><ul><li>Missing link</li></ul></div>"#,
        r#"<div class="b-search__live_section"><ul><li><a href="/films/1-title.html"></a></li></ul></div>"#,
        r#"<div class="b-search__live_section"><ul><li><a href="https://foreign.test/films/1-title.html"><span class="b-search__section_list_title">Title</span></a></li></ul></div>"#,
        r#"<div><ul><li><a href="/films/1-title.html"><span class="b-search__section_list_title">Title</span></a></li></ul></div>"#,
    ] {
        assert_invalid(parse_quick_search(html, &query(), &origin()));
    }
}

#[test]
fn quick_search_entry_budget_is_exact_and_atomic() {
    let entries = (0..MAX_QUICK_SEARCH_ENTRIES)
        .map(|index| format!(r#"<li><a href="/films/{index}.html"><span class="b-search__section_list_title">Title {index}</span></a></li>"#))
        .collect::<String>();
    let accepted = format!(r#"<div class="b-search__live_section"><ul>{entries}</ul></div>"#);
    assert_eq!(
        parse_quick_search(&accepted, &query(), &origin())
            .unwrap()
            .len(),
        MAX_QUICK_SEARCH_ENTRIES
    );

    let rejected = accepted.replace(
        "</ul>",
        r#"<li><a href="/films/extra.html"><span class="b-search__section_list_title">Extra</span></a></li></ul>"#,
    );
    assert_invalid(parse_quick_search(&rejected, &query(), &origin()));
}

#[test]
fn discovery_debug_output_is_redacted() {
    let query = QuickSearchQuery::new("secret query").unwrap();
    let entries = parse_quick_search(
        r#"<div class="b-search__live_section"><ul><li><a href="/films/1-secret-title.html"><span class="b-search__section_list_title">Secret title</span><span class="b-search__section_list_info">Secret info</span></a></li></ul></div>"#,
        &query,
        &origin(),
    )
    .unwrap();

    for debug in [format!("{query:?}"), format!("{:?}", entries[0])] {
        for forbidden in ["secret", "Secret", "films", "rezka.test"] {
            assert!(
                !debug.contains(forbidden),
                "Debug leaked {forbidden}: {debug}"
            );
        }
    }
}
