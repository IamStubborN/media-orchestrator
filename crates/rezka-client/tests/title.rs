use rezka_client::{
    ProviderFailureReason, RatingSource, RezkaError, RezkaErrorCode, RezkaMediaKind,
    SeriesLifecycleStatus, TitleLocator, TranslationId, TranslationKey,
    catalog::parser::parse_title_page,
    mirror::MirrorSet,
    session::{RezkaClient, RezkaClientConfig},
};
use time::Duration;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const ORIGIN: &str = "https://rezka.test/";

fn origin() -> Url {
    Url::parse(ORIGIN).unwrap()
}

fn locator(value: &str) -> TitleLocator {
    TitleLocator::new(value).unwrap()
}

fn parse(html: &str, path: &str) -> Result<rezka_client::TitleDetails, RezkaError> {
    parse_title_page(html, &locator(path), &origin())
}

fn assert_code(result: Result<impl std::fmt::Debug, RezkaError>, expected: RezkaErrorCode) {
    let error = result.unwrap_err();
    assert_eq!(error.code(), expected);
    assert!(!format!("{error:?}").contains(ORIGIN));
    assert!(!error.to_string().contains(ORIGIN));
}

fn valid_page(id_marker: &str, player: &str) -> String {
    format!(
        r#"<html><head><title>Valid</title><meta property="og:type" content="video.movie"></head>
        <body>{id_marker}<h1 class="b-post__title" itemprop="name">Title</h1>
        <ul id="translators-list"><li class="b-translator__item" data-translator_id="9">Studio</li></ul>
        <script>{player}</script></body></html>"#
    )
}

fn movie_page(translations: &str, default_id: u64) -> String {
    format!(
        r#"<html><head><title>Movie</title><meta property="og:type" content="video.movie"></head>
        <body><input id="post_id" value="55"><h1 class="b-post__title">Movie</h1>
        <ul id="translators-list">{translations}</ul>
        <script>sof.tv.initCDNMoviesEvents(55, {default_id}, {{}}, {{}});</script></body></html>"#
    )
}

fn series_page(info_rows: &str) -> String {
    format!(
        r#"<html><head><title>Series</title><meta property="og:type" content="video.tv_series"></head>
        <body><main class="b-content__main"><input id="post_id" value="56">
        <h1 class="b-post__title">Series</h1>
        <table class="b-post__info">{info_rows}</table>
        <ul id="translators-list"><li class="b-translator__item" data-translator_id="7">Studio</li></ul>
        <script>sof.tv.initCDNSeriesEvents(56, 7, {{}}, {{}});</script>
        </main></body></html>"#
    )
}

#[test]
fn series_lifecycle_parses_completed_russian_and_ukrainian_rows() {
    for row in [
        "<tr><td>Статус сериала:</td><td>Завершён</td></tr>",
        "<tr><td><h2>Статус серіалу</h2></td><td><span>Завершено</span></td></tr>",
    ] {
        let title = parse(&series_page(row), "/series/56-series.html").unwrap();
        assert_eq!(
            title.series_lifecycle_status(),
            SeriesLifecycleStatus::Completed,
            "row: {row}"
        );
    }
}

#[test]
fn series_lifecycle_parses_ongoing_russian_and_ukrainian_rows() {
    for row in [
        "<tr><td>Статус сериала</td><td>Онгоинг</td></tr>",
        "<tr><td>Статус серіалу:</td><td>Триває</td></tr>",
        "<tr><td>СТАТУС СЕРИАЛА:</td><td>Выходит</td></tr>",
        "<tr><td>Статус серіалу</td><td>Виходить</td></tr>",
        "<tr><td>Статус серіалу</td><td>Продовжується</td></tr>",
    ] {
        let title = parse(&series_page(row), "/series/56-series.html").unwrap();
        assert_eq!(
            title.series_lifecycle_status(),
            SeriesLifecycleStatus::Ongoing,
            "row: {row}"
        );
    }
}

#[test]
fn movie_does_not_expose_a_series_lifecycle_status() {
    let html = movie_page(
        r#"<li class="b-translator__item" data-translator_id="7">Studio</li>"#,
        7,
    )
    .replace(
        "<ul id=\"translators-list\">",
        "<table class=\"b-post__info\"><tr><td>Статус сериала</td><td>Завершён</td></tr></table><ul id=\"translators-list\">",
    );
    let title = parse(&html, "/films/55-movie.html").unwrap();

    assert_eq!(
        title.series_lifecycle_status(),
        SeriesLifecycleStatus::Unknown
    );
}

#[test]
fn series_lifecycle_is_unknown_when_missing_unrecognized_or_ambiguous() {
    for rows in [
        "",
        "<tr><td>Статус сериала</td><td>Скоро</td></tr>",
        "<tr><td>Комментарий</td><td>Сериал завершён</td></tr>",
        "<tr><td>Статус сериала</td><td>Завершён</td></tr><tr><td>Статус серіалу</td><td>Триває</td></tr>",
    ] {
        let title = parse(&series_page(rows), "/series/56-series.html").unwrap();
        assert_eq!(
            title.series_lifecycle_status(),
            SeriesLifecycleStatus::Unknown,
            "rows: {rows}"
        );
    }
}

#[test]
fn movie_fixture_preserves_flag_specific_identity_and_ambiguous_default() {
    let title = parse(
        include_str!("fixtures/title_movie.html"),
        "/films/drama/101-fixture.html",
    )
    .unwrap();

    assert_eq!(title.id().get(), 101);
    assert_eq!(title.locator().as_str(), "/films/drama/101-fixture.html");
    assert_eq!(title.title(), "Fixture Movie");
    assert_eq!(title.original_title(), Some("Original Fixture Movie"));
    assert_eq!(title.release_year(), Some(2024));
    assert_eq!(title.description(), Some("A fixture movie description."));
    assert_eq!(title.countries(), ["USA", "Canada"]);
    assert_eq!(title.genres(), ["Drama", "Thriller"]);
    assert_eq!(title.duration_minutes(), Some(102));
    assert_eq!(title.age_rating(), Some(16));
    assert_eq!(title.ratings().len(), 2);
    assert_eq!(title.ratings()[0].source(), RatingSource::Kinopoisk);
    assert_eq!(title.ratings()[0].value(), 7.8);
    assert_eq!(title.ratings()[1].source(), RatingSource::Imdb);
    assert_eq!(title.ratings()[1].value(), 8.1);
    assert_eq!(title.franchise().len(), 3);
    assert_eq!(title.franchise()[0].title(), "Fixture Prequel");
    assert_eq!(
        title.franchise()[0].locator().as_str(),
        "/films/drama/100-fixture-prequel.html"
    );
    assert!(!title.franchise()[0].is_current());
    assert!(title.franchise()[1].is_current());
    assert_eq!(title.franchise()[2].title(), "Fixture Sequel");
    assert_eq!(title.kind(), RezkaMediaKind::Movie);
    assert_eq!(title.translations().len(), 3);
    assert_eq!(title.translations()[0].id().get(), 7);
    assert!(!title.translations()[0].is_camrip());
    assert!(title.translations()[1].is_camrip());
    assert!(title.translations()[1].is_premium());
    assert!(title.translations()[2].has_ads());
    assert!(title.translations()[2].is_director());
    assert_ne!(title.translations()[0].key(), title.translations()[1].key());
    assert_eq!(title.default_translation(), None);
    assert_eq!(
        title.thumbnail().unwrap().url().as_str(),
        "https://images.example.com/movie.jpg"
    );
    assert!(!format!("{title:?}").contains("Fixture Movie"));
    assert!(!format!("{title:?}").contains("101-fixture"));
}

#[test]
fn series_fixture_has_series_keys_and_unique_default() {
    let title = parse(
        include_str!("fixtures/title_series.html"),
        "/series/drama/202-fixture.html",
    )
    .unwrap();

    assert_eq!(title.kind(), RezkaMediaKind::Series);
    assert_eq!(
        title.default_translation(),
        Some(title.translations()[1].key())
    );
    assert!(matches!(
        title.translations()[0].key(),
        TranslationKey::Series { id } if id.get() == 17
    ));
}

#[test]
fn player_initialization_can_supply_the_only_translation() {
    let title = parse(
        include_str!("fixtures/title_single_translation.html"),
        "/films/drama/303-single.html",
    )
    .unwrap();

    assert_eq!(title.translations().len(), 1);
    assert_eq!(title.translations()[0].name(), "Only Studio");
    assert_eq!(
        title.default_translation(),
        Some(title.translations()[0].key())
    );
    assert!(!title.translations()[0].is_camrip());
    assert!(!title.translations()[0].has_ads());
    assert!(!title.translations()[0].is_director());
}

#[test]
fn all_six_title_id_sources_are_accepted() {
    let cases = [
        (r#"<input id="post_id" value="61">"#, "", "/films/x.html"),
        (
            r#"<div id="send-video-issue" data-id="61"></div>"#,
            "",
            "/films/x.html",
        ),
        (
            r#"<div id="user-favorites-holder" data-post_id="61"></div>"#,
            "",
            "/films/x.html",
        ),
        (
            r#"<div class="b-userset__fav_holder" data-post_id="61"></div>"#,
            "",
            "/films/x.html",
        ),
        (
            "",
            "sof.tv.initCDNMoviesEvents(61, 9, {}, {});",
            "/films/x.html",
        ),
        ("", "", "/films/61-fallback.html"),
    ];

    for (marker, player, path) in cases {
        let title = parse(&valid_page(marker, player), path).unwrap();
        assert_eq!(title.id().get(), 61, "source failed for {marker} {player}");
    }
}

#[test]
fn equal_title_id_candidates_pass_but_conflicts_fail() {
    let equal = valid_page(
        r#"<input id="post_id" value="61"><div id="send-video-issue" data-id="61"></div>"#,
        "sof.tv.initCDNMoviesEvents(61, 9, {}, {});",
    );
    assert_eq!(
        parse(&equal, "/films/999-hint.html").unwrap().id().get(),
        61
    );

    assert_code(
        parse(
            include_str!("fixtures/title_conflicting_ids.html"),
            "/films/401-conflict.html",
        ),
        RezkaErrorCode::ProviderResponseInvalid,
    );
}

#[test]
fn invalid_or_absent_title_ids_fail_atomically() {
    for value in ["0", "18446744073709551616", "not-an-id"] {
        let html = valid_page(&format!(r#"<input id="post_id" value="{value}">"#), "");
        assert_code(
            parse(&html, "/films/61-fallback.html"),
            RezkaErrorCode::ProviderResponseInvalid,
        );
    }
    assert_code(
        parse(&valid_page("", ""), "/films/no-id.html"),
        RezkaErrorCode::ProviderResponseInvalid,
    );
    assert_code(
        parse(&valid_page("", ""), "/films/61.html"),
        RezkaErrorCode::ProviderResponseInvalid,
    );
}

#[test]
fn player_initialization_ignores_non_call_javascript_and_visible_text() {
    let false_positives = [
        r#"// sof.tv.initCDNMoviesEvents(999, 999, {}, {});
            /* sof.tv.initCDNMoviesEvents(999, 999, {}, {}); */"#,
        r#"const quoted = "sof.tv.initCDNMoviesEvents(999, 999, {}, {});";"#,
        r#"const singleQuoted = 'sof.tv.initCDNMoviesEvents(999, 999, {}, {});';"#,
        r#"const template = `sof.tv.initCDNMoviesEvents(999, 999, {}, {})`;"#,
        r#"function initCDNMoviesEvents(titleId, translationId) {}"#,
        r#"const matcher = /[a/b]sof.tv.initCDNMoviesEvents(999, 999, payload)\/tail/gi;"#,
        r#"if (ready) /sof.tv.initCDNMoviesEvents(999,999,x)/.test(value);"#,
        r#"other.tv.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"sof.other.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"sof["tv"].initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"sof.tv["initCDNMoviesEvents"](999, 999, {}, {});"#,
        r#"sof?.tv.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"sof.tv?.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"sof.tv.initCDNMoviesEvents?.(999, 999, {}, {});"#,
        r#"window.sof.tv.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"window . sof.tv.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"window./*comment*/sof.tv.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"sof.tv.initCDNMoviesEventsSuffix(999, 999, {}, {});"#,
        r#"prefixsof.tv.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"πsof.tv.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"‿sof.tv.initCDNMoviesEvents(999, 999, {}, {});"#,
        r#"sof.tv.initCDNMoviesEventsπ(999, 999, {}, {});"#,
        r#"sof.tv.initCDNMoviesEvents‿(999, 999, {}, {});"#,
        "sof\u{0085}.tv.initCDNMoviesEvents(999, 999, {}, {});",
        r#"const название = 1;"#,
    ];
    for source in false_positives {
        let html = valid_page(r#"<input id="post_id" value="61">"#, source);
        let title = parse(&html, "/films/no-fallback.html")
            .unwrap_or_else(|error| panic!("unexpected error for {source}: {error:?}"));
        assert_eq!(title.id().get(), 61, "false positive source: {source}");
    }

    let visible = valid_page(
        r#"<input id="post_id" value="61">
        <div>sof.tv.initCDNMoviesEvents(999, 999, {}, {});</div>"#,
        "",
    );
    assert_eq!(
        parse(&visible, "/films/no-fallback.html")
            .unwrap()
            .id()
            .get(),
        61
    );
}

#[test]
fn player_initialization_accepts_an_exact_whitespace_separated_call() {
    let html = valid_page(
        "",
        "sof \n  .\t tv \n . initCDNMoviesEvents \n  (61, 9, {}, {});",
    );
    let title = parse(&html, "/films/no-fallback.html").unwrap();

    assert_eq!(title.id().get(), 61);
    assert_eq!(title.kind(), RezkaMediaKind::Movie);
    assert_eq!(
        title.default_translation(),
        Some(title.translations()[0].key())
    );
}

#[test]
fn player_initialization_accepts_exact_root_calls_with_javascript_trivia() {
    let movie = valid_page(
        "",
        "sof /* root */ . tv // member\n . initCDNMoviesEvents /* call */ (61, 9, {}, {});",
    );
    let title = parse(&movie, "/films/no-fallback.html").unwrap();
    assert_eq!(title.id().get(), 61);
    assert_eq!(title.kind(), RezkaMediaKind::Movie);

    let series = r#"<html><head><title>Series</title>
        <meta property="og:type" content="video.tv_series"></head><body>
        <h1 class="b-post__title">Series</h1>
        <ul id="translators-list"><li class="b-translator__item" data-translator_id="18">Studio</li></ul>
        <script>sof /* root */ . tv /* member */ . initCDNSeriesEvents /* call */ (202, 18, {}, {});</script>
        </body></html>"#;
    let title = parse(series, "/series/no-fallback.html").unwrap();
    assert_eq!(title.id().get(), 202);
    assert_eq!(title.kind(), RezkaMediaKind::Series);
}

#[test]
fn malformed_javascript_is_handled_without_panicking_or_forging_initialization() {
    for source in [
        "sof.tv.initCDNMoviesEvents(",
        "const broken = ; sof.tv.initCDNMoviesEvents(999, 999, {}, {});",
        "window./* unterminated",
    ] {
        let html = valid_page(r#"<input id="post_id" value="61">"#, source);
        let result = std::panic::catch_unwind(|| parse(&html, "/films/no-fallback.html"));
        assert!(result.is_ok(), "parser panicked for: {source}");
        let parsed = result
            .unwrap()
            .unwrap_or_else(|error| panic!("unexpected error for {source}: {error:?}"));
        assert_eq!(parsed.id().get(), 61, "malformed source: {source}");
    }
}

#[test]
fn player_initialization_requires_positive_decimal_u64_literals() {
    let maximum = valid_page(
        "",
        "sof.tv.initCDNMoviesEvents(18446744073709551615, 9, {}, {});",
    );
    assert_eq!(
        parse(&maximum, "/films/no-fallback.html")
            .unwrap()
            .id()
            .get(),
        u64::MAX
    );

    for arguments in [
        "0, 9",
        "61, 0",
        "18446744073709551616, 9",
        "61.0, 9",
        "6.1e1, 9",
        "'61', 9",
        "titleId, 9",
        "-61, 9",
        "...ids",
    ] {
        let html = valid_page(
            "",
            &format!("sof.tv.initCDNMoviesEvents({arguments}, {{}}, {{}});"),
        );
        assert_code(
            parse(&html, "/films/no-fallback.html"),
            RezkaErrorCode::ProviderResponseInvalid,
        );
    }
}

#[test]
fn duplicate_translation_identity_is_rejected_by_media_kind() {
    let duplicate_movie = movie_page(
        r#"<li class="b-translator__item" data-translator_id="7" data-camrip="1">A</li>
        <li class="b-translator__item" data-translator_id="7" data-camrip="1">B</li>"#,
        7,
    );
    assert_code(
        parse(&duplicate_movie, "/films/55-movie.html"),
        RezkaErrorCode::ProviderResponseInvalid,
    );

    let duplicate_series = r#"<html><head><title>Series</title><meta property="og:type" content="video.tv_series"></head>
        <body><input id="post_id" value="56"><h1 class="b-post__title">Series</h1>
        <ul><li class="b-translator__item" data-translator_id="7">A</li>
        <li class="b-translator__item" data-translator_id="7" data-ads="1">B</li></ul></body></html>"#;
    assert_code(
        parse(duplicate_series, "/series/56-series.html"),
        RezkaErrorCode::ProviderResponseInvalid,
    );
}

#[test]
fn movie_default_resolves_only_for_one_flag_variant() {
    let unique = movie_page(
        r#"<li class="b-translator__item" data-translator_id="7">A</li>
        <li class="b-translator__item" data-translator_id="8" data-camrip="1">B</li>"#,
        7,
    );
    let title = parse(&unique, "/films/55-movie.html").unwrap();
    assert_eq!(
        title.default_translation(),
        Some(title.translations()[0].key())
    );

    let ambiguous = movie_page(
        r#"<li class="b-translator__item" data-translator_id="7">A</li>
        <li class="b-translator__item" data-translator_id="7" data-camrip="1">B</li>"#,
        7,
    );
    assert_eq!(
        parse(&ambiguous, "/films/55-movie.html")
            .unwrap()
            .default_translation(),
        None
    );
}

#[test]
fn translation_budget_accepts_128_and_rejects_129() {
    for (count, expected) in [(128, true), (129, false)] {
        let translations = (1..=count)
            .map(|id| {
                format!(r#"<li class="b-translator__item" data-translator_id="{id}">T{id}</li>"#)
            })
            .collect::<String>();
        let result = parse(&movie_page(&translations, 1), "/films/55-movie.html");
        assert_eq!(result.is_ok(), expected, "count {count}");
    }
}

#[test]
fn selection_validates_membership_and_media_kind() {
    let movie = parse(
        include_str!("fixtures/title_movie.html"),
        "/films/101-fixture.html",
    )
    .unwrap();
    let selected = movie
        .select_translation(movie.translations()[2].key())
        .unwrap();
    assert_eq!(selected.translation().key(), movie.translations()[2].key());
    assert_eq!(selected.title().kind(), RezkaMediaKind::Movie);
    assert!(selected.movie_request().is_ok());

    let foreign = TranslationKey::Movie {
        id: TranslationId::new(999).unwrap(),
        is_camrip: false,
        has_ads: false,
        is_director: false,
    };
    assert_code(
        movie.select_translation(&foreign),
        RezkaErrorCode::TranslationUnavailable,
    );

    let series = parse(
        include_str!("fixtures/title_series.html"),
        "/series/202-fixture.html",
    )
    .unwrap();
    let selected = series
        .select_translation(series.translations()[0].key())
        .unwrap();
    let error = selected.movie_request().unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::TranslationUnavailable);
    assert!(matches!(
        error,
        RezkaError::TranslationUnavailable {
            reason: ProviderFailureReason::TranslationUnavailable
        }
    ));
}

#[test]
fn title_capability_debug_matrix_is_exact_and_redacted() {
    let html = r#"<html><head><title>Safe document title</title>
        <meta property="og:type" content="video.movie"></head><body>
        <input id="post_id" value="901">
        <h1 class="b-post__title">Sensitive Movie 198.51.100.42</h1>
        <div class="b-post__origtitle">Original Secret</div>
        <div class="b-content__main"><div class="b-sidecover">
          <img src="https://media.secret.example/poster.jpg?token=debug-secret">
        </div></div>
        <ul id="translators-list"><li class="b-translator__item"
          data-translator_id="7">Secret Studio token=debug-secret</li></ul>
        </body></html>"#;
    let title = parse(html, "/films/private/901-secret-title.html").unwrap();
    let selected = title
        .select_translation(title.translations()[0].key())
        .unwrap();
    let request = selected.clone().movie_request().unwrap();
    let debug_values = [
        format!("{:?}", title.translations()[0]),
        format!("{:?}", selected.title()),
        format!("{selected:?}"),
        format!("{request:?}"),
        format!("{title:?}"),
    ];

    assert_eq!(
        debug_values,
        [
            "Translation { key: Movie { id: TranslationId(7), is_camrip: false, has_ads: false, is_director: false }, name: \"[REDACTED]\", is_premium: false, .. }",
            "TitlePlaybackRef { id: RezkaTitleId(901), locator: \"[REDACTED]\", kind: Movie }",
            "SelectedTranslation { title: TitlePlaybackRef { id: RezkaTitleId(901), locator: \"[REDACTED]\", kind: Movie }, translation: Translation { key: Movie { id: TranslationId(7), is_camrip: false, has_ads: false, is_director: false }, name: \"[REDACTED]\", is_premium: false, .. } }",
            "Movie(SelectedTranslation { title: TitlePlaybackRef { id: RezkaTitleId(901), locator: \"[REDACTED]\", kind: Movie }, translation: Translation { key: Movie { id: TranslationId(7), is_camrip: false, has_ads: false, is_director: false }, name: \"[REDACTED]\", is_premium: false, .. } })",
            "TitleDetails { id: RezkaTitleId(901), locator: \"[REDACTED]\", title: \"[REDACTED]\", original_title: Some(\"[REDACTED]\"), release_year: None, kind: Movie, series_lifecycle_status: Unknown, description: None, countries: 0, genres: 0, duration_minutes: None, age_rating: None, ratings: [], franchise: 0, thumbnail: Some(\"[REDACTED]\"), translations: 1, default_translation: None }",
        ]
    );
    for debug in debug_values {
        for forbidden in [
            "Sensitive Movie",
            "Original Secret",
            "Secret Studio",
            "/films/private/901-secret-title.html",
            "rezka.test",
            "media.secret.example",
            "198.51.100.42",
            "token=debug-secret",
            "debug-secret",
            "?token",
        ] {
            assert!(!debug.contains(forbidden), "leaked {forbidden} in {debug}");
        }
    }
}

#[test]
fn detailed_metadata_rejects_invalid_values_and_bounded_collections() {
    let base = movie_page(
        r#"<li class="b-translator__item" data-translator_id="7">Studio</li>"#,
        7,
    );
    for row in [
        "<tr><td>Duration:</td><td>999 hours</td></tr>",
        "<tr><td>Age:</td><td>99+</td></tr>",
    ] {
        let html = base.replace(
            "<ul id=\"translators-list\">",
            &format!("<div class=\"b-content__main\"><table class=\"b-post__info\">{row}</table></div><ul id=\"translators-list\">"),
        );
        assert_code(
            parse(&html, "/films/55-movie.html"),
            RezkaErrorCode::ProviderResponseInvalid,
        );
    }

    let countries = (0..65)
        .map(|index| format!(r#"<a href="/country/c{index}/">Country {index}</a>"#))
        .collect::<String>();
    let html = base.replace(
        "<ul id=\"translators-list\">",
        &format!("<div class=\"b-content__main\"><table class=\"b-post__info\"><tr><td>Country:</td><td>{countries}</td></tr></table></div><ul id=\"translators-list\">"),
    );
    assert_code(
        parse(&html, "/films/55-movie.html"),
        RezkaErrorCode::ProviderResponseInvalid,
    );
}

fn test_client(server: &MockServer) -> RezkaClient {
    RezkaClient::new(RezkaClientConfig {
        mirrors: MirrorSet::new(vec![Url::parse(&server.uri()).unwrap()]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: 0,
        anubis_max_nonce: 1,
        proxy_url: None,
    })
    .unwrap()
}

#[tokio::test]
async fn title_maps_access_states_and_terminal_statuses() {
    let cases = [
        (
            200,
            r#"<div id="anubis_challenge"></div>"#,
            RezkaErrorCode::ChallengeRequired,
        ),
        (
            200,
            "<title> Sign In </title>",
            RezkaErrorCode::AuthenticationRequired,
        ),
        (
            200,
            "<title> Verify </title>",
            RezkaErrorCode::ChallengeRequired,
        ),
        (
            200,
            r#"<div class="b-player__restricted__block_message">Private title<span class="b-restricted__suggest">Suggestion</span></div>"#,
            RezkaErrorCode::TranslationUnavailable,
        ),
        (404, "private missing body", RezkaErrorCode::TitleNotFound),
        (410, "private gone body", RezkaErrorCode::TitleNotFound),
        (
            200,
            "<title>Ordinary page</title>",
            RezkaErrorCode::ProviderResponseInvalid,
        ),
    ];

    for (status, body, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/films/77-title.html"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        let mut client = test_client(&server);
        assert_code(
            client.title(&locator("/films/77-title.html")).await,
            expected,
        );
    }
}

#[tokio::test]
async fn title_uses_selected_origin_and_rejects_cross_origin_redirects() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/films/101-fixture.html"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", "https://foreign.example/title"),
        )
        .mount(&server)
        .await;

    let mut client = test_client(&server);
    assert_code(
        client.title(&locator("/films/101-fixture.html")).await,
        RezkaErrorCode::ProviderResponseInvalid,
    );
}

#[tokio::test]
async fn title_classifies_access_states_before_redirect_rejection() {
    let cases = [
        (
            r#"<div id="anubis_challenge"></div>"#,
            RezkaErrorCode::ChallengeRequired,
        ),
        (
            "<title> Sign In </title>",
            RezkaErrorCode::AuthenticationRequired,
        ),
        ("<title> Verify </title>", RezkaErrorCode::ChallengeRequired),
        (
            r#"<div class="b-player__restricted__block_message">Private title</div>"#,
            RezkaErrorCode::TranslationUnavailable,
        ),
    ];

    for (body, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/films/77-title.html"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", "https://foreign.example/title")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;
        let mut client = test_client(&server);
        assert_code(
            client.title(&locator("/films/77-title.html")).await,
            expected,
        );
    }
}

#[tokio::test]
async fn empty_same_origin_and_cross_origin_redirects_are_invalid() {
    for location in ["/other-title", "https://foreign.example/title"] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/films/77-title.html"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", location))
            .mount(&server)
            .await;
        let mut client = test_client(&server);
        assert_code(
            client.title(&locator("/films/77-title.html")).await,
            RezkaErrorCode::ProviderResponseInvalid,
        );
    }
}

#[tokio::test]
async fn restricted_state_ignores_all_suggestion_subtrees_structurally() {
    let suggestion_only = r#"<div class="b-player__restricted__block_message">
        <span class="b-restricted__suggest"> First <strong>nested</strong> suggestion </span>

        <span class="b-restricted__suggest"> Second suggestion </span>
      </div>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/films/77-title.html"))
        .respond_with(ResponseTemplate::new(200).set_body_string(suggestion_only))
        .mount(&server)
        .await;
    let mut client = test_client(&server);
    assert_code(
        client.title(&locator("/films/77-title.html")).await,
        RezkaErrorCode::ProviderResponseInvalid,
    );

    let real_text = r#"<div class="b-player__restricted__block_message">
        <span class="b-restricted__suggest"> First suggestion </span>
        Actual restriction
        <span class="b-restricted__suggest"> Second suggestion </span>
      </div>"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/films/77-title.html"))
        .respond_with(ResponseTemplate::new(200).set_body_string(real_text))
        .mount(&server)
        .await;
    let mut client = test_client(&server);
    assert_code(
        client.title(&locator("/films/77-title.html")).await,
        RezkaErrorCode::TranslationUnavailable,
    );
}

#[tokio::test]
async fn title_rewrites_the_locator_to_the_failover_origin() {
    let unavailable = MockServer::start().await;
    let selected = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/films/61-title.html"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&unavailable)
        .await;
    Mock::given(method("GET"))
        .and(path("/films/61-title.html"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(valid_page(r#"<input id="post_id" value="61">"#, "")),
        )
        .expect(1)
        .mount(&selected)
        .await;

    let mirrors = vec![
        Url::parse(&unavailable.uri()).unwrap(),
        Url::parse(&selected.uri()).unwrap(),
    ];
    let mut client = RezkaClient::new(RezkaClientConfig {
        mirrors: MirrorSet::new(mirrors).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: 1,
        anubis_max_nonce: 1,
        proxy_url: None,
    })
    .unwrap();

    let title = client
        .title(&locator("/films/61-title.html"))
        .await
        .unwrap();
    assert_eq!(title.id().get(), 61);
    assert_eq!(title.locator().as_str(), "/films/61-title.html");
}
