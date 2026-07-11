use rezka_client::{
    RezkaError, RezkaErrorCode, TitleLocator, TranslationKey,
    catalog::parser::parse_title_page,
    mirror::MirrorSet,
    playback::parser::parse_series_availability,
    session::{RezkaClient, RezkaClientConfig},
};
use time::Duration;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string, header, header_exists, method, path},
};

fn selected_series() -> rezka_client::SelectedTranslation {
    let origin = Url::parse("https://rezka.test/").unwrap();
    let locator = TitleLocator::new("/series/drama/202-fixture.html").unwrap();
    let title = parse_title_page(
        include_str!("fixtures/title_series.html"),
        &locator,
        &origin,
    )
    .unwrap();
    title
        .select_translation(&TranslationKey::Series {
            id: rezka_client::TranslationId::new(17).unwrap(),
        })
        .unwrap()
}

fn selected_movie() -> rezka_client::SelectedTranslation {
    let origin = Url::parse("https://rezka.test/").unwrap();
    let locator = TitleLocator::new("/films/drama/101-fixture.html").unwrap();
    let title =
        parse_title_page(include_str!("fixtures/title_movie.html"), &locator, &origin).unwrap();
    title
        .select_translation(title.translations()[0].key())
        .unwrap()
}

fn assert_invalid(result: Result<impl std::fmt::Debug, RezkaError>) {
    assert_eq!(
        result.unwrap_err().code(),
        RezkaErrorCode::ProviderResponseInvalid
    );
}

#[test]
fn parser_sorts_and_binds_all_seasons_and_episodes() {
    let availability = parse_series_availability(
        include_str!("fixtures/episodes_success.json"),
        selected_series(),
    )
    .unwrap();

    assert_eq!(availability.seasons().len(), 2);
    assert_eq!(availability.seasons()[0].number(), 1);
    assert_eq!(availability.seasons()[0].episodes()[0].number(), 1);
    assert_eq!(availability.seasons()[0].episodes()[1].number(), 2);
    assert_eq!(availability.seasons()[1].number(), 2);
    assert_eq!(availability.seasons()[1].episodes()[0].number(), 3);

    let request = availability
        .select_episode(1, 2)
        .unwrap()
        .playback_request();
    assert_eq!(request.target().season_episode(), Some((1, 2)));
    assert_eq!(
        request.translation_key(),
        selected_series().translation().key()
    );
}

#[test]
fn parser_accepts_explicit_empty_but_rejects_missing_wrong_or_orphan_fields() {
    let empty = parse_series_availability(
        include_str!("fixtures/episodes_empty.json"),
        selected_series(),
    )
    .unwrap();
    assert!(empty.seasons().is_empty());

    for malformed in [
        include_str!("fixtures/episodes_malformed.json"),
        r#"{"success":true,"seasons":""}"#,
        r#"{"success":1,"seasons":"","episodes":""}"#,
        r#"{"success":false,"seasons":"","episodes":""}"#,
        r#"{"success":true,"seasons":[],"episodes":""}"#,
    ] {
        assert_invalid(parse_series_availability(malformed, selected_series()));
    }
}

#[test]
fn parser_rejects_duplicate_zero_overflow_and_duplicate_json_fields() {
    for malformed in [
        r#"{"success":true,"seasons":"<li data-tab_id='1'>One</li><li data-tab_id='1'>Again</li>","episodes":""}"#,
        r#"{"success":true,"seasons":"<li data-tab_id='1'>One</li>","episodes":"<li data-season_id='1' data-episode_id='1'>One</li><li data-season_id='1' data-episode_id='1'>Again</li>"}"#,
        r#"{"success":true,"seasons":"<li data-tab_id='0'>Zero</li>","episodes":""}"#,
        r#"{"success":true,"seasons":"<li data-tab_id='2147483648'>Large</li>","episodes":""}"#,
        r#"{"success":true,"success":true,"seasons":"","episodes":""}"#,
        r#"{"success":true,"seasons":"","seasons":"","episodes":""}"#,
    ] {
        assert_invalid(parse_series_availability(malformed, selected_series()));
    }
}

fn availability_json(seasons: usize, episodes_per_season: usize) -> String {
    let mut season_html = String::new();
    let mut episode_html = String::new();
    for season in 1..=seasons {
        season_html.push_str(&format!("<li data-tab_id='{season}'>S{season}</li>"));
        for episode in 1..=episodes_per_season {
            episode_html.push_str(&format!(
                "<li data-season_id='{season}' data-episode_id='{episode}'>E{episode}</li>"
            ));
        }
    }
    serde_json::json!({"success": true, "seasons": season_html, "episodes": episode_html})
        .to_string()
}

#[test]
fn parser_enforces_series_resource_budgets_atomically() {
    assert!(parse_series_availability(&availability_json(256, 0), selected_series()).is_ok());
    assert_invalid(parse_series_availability(
        &availability_json(257, 0),
        selected_series(),
    ));
    assert!(parse_series_availability(&availability_json(1, 4096), selected_series()).is_ok());
    assert_invalid(parse_series_availability(
        &availability_json(1, 4097),
        selected_series(),
    ));
    assert!(parse_series_availability(&availability_json(4, 4096), selected_series()).is_ok());
    assert_invalid(parse_series_availability(
        &availability_json(5, 4096),
        selected_series(),
    ));
}

#[test]
fn absent_episode_is_typed_and_snapshot_bound() {
    let availability = parse_series_availability(
        include_str!("fixtures/episodes_success.json"),
        selected_series(),
    )
    .unwrap();
    for target in [(9, 1), (1, 9)] {
        assert_eq!(
            availability
                .select_episode(target.0, target.1)
                .unwrap_err()
                .code(),
            RezkaErrorCode::EpisodeUnavailable
        );
    }
}

fn config(origin: Url) -> RezkaClientConfig {
    RezkaClientConfig {
        mirrors: MirrorSet::new(vec![origin]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: 0,
        anubis_max_nonce: 1,
    }
}

#[tokio::test]
async fn series_availability_sends_exact_ajax_multimap_referer_xhr_and_cookie() {
    let server = MockServer::start().await;
    let origin = Url::parse(&server.uri()).unwrap();
    let title_path = "/series/drama/202-fixture.html";
    Mock::given(method("GET"))
        .and(path(title_path))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "rezka_session=opaque; Path=/")
                .set_body_string(include_str!("fixtures/title_series.html")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/get_cdn_series/"))
        .and(body_string("id=202&translator_id=17&action=get_episodes"))
        .and(header(
            "referer",
            format!("{origin}series/drama/202-fixture.html"),
        ))
        .and(header("x-requested-with", "XMLHttpRequest"))
        .and(header("cookie", "rezka_session=opaque"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/episodes_success.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(origin)).unwrap();
    let title = client
        .title(&TitleLocator::new(title_path).unwrap())
        .await
        .unwrap();
    let selection = title
        .select_translation(title.translations()[0].key())
        .unwrap();
    let availability = client.series_availability(&selection).await.unwrap();
    assert_eq!(availability.seasons().len(), 2);
}

#[tokio::test]
async fn movie_selection_is_rejected_without_network() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header_exists("x-requested-with"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).unwrap();
    let mut client = RezkaClient::new(config(origin)).unwrap();

    let error = client
        .series_availability(&selected_movie())
        .await
        .unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::TranslationUnavailable);
}
