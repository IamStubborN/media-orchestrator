use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use rezka_client::{
    PlaybackRequest, ProviderFailureReason, ResolvedTarget, RezkaErrorCode, TitleLocator,
    TranslationKey,
    catalog::parser::parse_title_page,
    mirror::MirrorSet,
    playback::parser::{parse_playback_manifest, parse_series_availability},
    session::{RezkaClient, RezkaClientConfig},
};
use time::Duration;
use url::Url;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{body_string, header, header_exists, method, path},
};

fn movie_request() -> PlaybackRequest {
    let origin = Url::parse("https://rezka.test/").unwrap();
    let locator = TitleLocator::new("/films/drama/101-fixture.html").unwrap();
    let title =
        parse_title_page(include_str!("fixtures/title_movie.html"), &locator, &origin).unwrap();
    title
        .select_translation(title.translations()[2].key())
        .unwrap()
        .movie_request()
        .unwrap()
}

fn episode_request() -> PlaybackRequest {
    let origin = Url::parse("https://rezka.test/").unwrap();
    let locator = TitleLocator::new("/series/drama/202-fixture.html").unwrap();
    let title = parse_title_page(
        include_str!("fixtures/title_series.html"),
        &locator,
        &origin,
    )
    .unwrap();
    let selection = title
        .select_translation(&TranslationKey::Series {
            id: rezka_client::TranslationId::new(17).unwrap(),
        })
        .unwrap();
    parse_series_availability(include_str!("fixtures/episodes_success.json"), selection)
        .unwrap()
        .select_episode(1, 2)
        .unwrap()
        .playback_request()
}

fn config(origins: Vec<Url>, retries: u8) -> RezkaClientConfig {
    RezkaClientConfig {
        mirrors: MirrorSet::new(origins).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: retries,
        anubis_max_nonce: 1,
    }
}

#[test]
fn parser_builds_complete_redacted_movie_and_episode_manifests() {
    let movie = parse_playback_manifest(
        include_str!("fixtures/playback_movie.json"),
        movie_request(),
    )
    .unwrap();
    assert_eq!(movie.title().id().get(), 101);
    assert_eq!(movie.target(), ResolvedTarget::Movie);
    assert_eq!(movie.variants().len(), 2);
    assert_eq!(movie.preferred_variant_index(), 0);
    assert_eq!(movie.subtitles().len(), 2);

    let episode = parse_playback_manifest(
        include_str!("fixtures/playback_episode.json"),
        episode_request(),
    )
    .unwrap();
    assert_eq!(
        episode.target(),
        ResolvedTarget::Episode {
            season: 1,
            episode: 2
        }
    );
    assert!(episode.subtitles().is_empty());

    let debug = format!("{movie:?} {episode:?}");
    for forbidden in [
        "cdn.example",
        "sub.example",
        "movie.mp4",
        "episode.m3u8",
        "https",
    ] {
        assert!(
            !debug.contains(forbidden),
            "manifest Debug leaked {forbidden}: {debug}"
        );
    }
}

#[test]
fn parser_requires_strict_success_and_non_empty_stream_payload() {
    for invalid in [
        r#"{}"#,
        r#"{"success":1,"url":"[720p]https://cdn.example.com/a.mp4"}"#,
        r#"{"success":true}"#,
        r#"{"success":true,"url":""}"#,
        r#"{"success":true,"success":true,"url":"[720p]https://cdn.example.com/a.mp4"}"#,
        r#"{"success":true,"url":"[720p]https://cdn.example.com/a.mp4","url":"[720p]https://cdn.example.com/b.mp4"}"#,
    ] {
        assert_eq!(
            parse_playback_manifest(invalid, movie_request())
                .unwrap_err()
                .code(),
            RezkaErrorCode::ProviderResponseInvalid
        );
    }
}

#[test]
fn failed_provider_messages_map_only_to_allowlisted_static_reasons() {
    let error = parse_playback_manifest(
        include_str!("fixtures/playback_failed.json"),
        episode_request(),
    )
    .unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::QualityUnavailable);
    let rendered = format!("{error:?}: {error}");
    for forbidden in ["provider-secret", "secret.invalid", "https://", "/path"] {
        assert!(!rendered.contains(forbidden));
    }

    let premium = parse_playback_manifest(
        r#"{"success":false,"message":"premium required"}"#,
        movie_request(),
    )
    .unwrap_err();
    assert!(matches!(
        premium,
        rezka_client::RezkaError::QualityUnavailable {
            reason: ProviderFailureReason::PremiumRequired
        }
    ));
}

#[tokio::test]
async fn resolve_sends_exact_movie_and_episode_forms_with_ajax_headers() {
    let server = MockServer::start().await;
    let origin = Url::parse(&server.uri()).unwrap();
    Mock::given(method("POST"))
        .and(path("/ajax/get_cdn_series/"))
        .and(body_string(
            "id=101&translator_id=8&is_camrip=0&is_ads=1&is_director=1&action=get_movie",
        ))
        .and(header(
            "referer",
            format!("{origin}films/drama/101-fixture.html"),
        ))
        .and(header("x-requested-with", "XMLHttpRequest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/playback_movie.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/get_cdn_series/"))
        .and(body_string(
            "id=202&translator_id=17&season=1&episode=2&action=get_stream",
        ))
        .and(header(
            "referer",
            format!("{origin}series/drama/202-fixture.html"),
        ))
        .and(header("x-requested-with", "XMLHttpRequest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/playback_episode.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(vec![origin], 0)).unwrap();
    assert_eq!(
        client
            .resolve(movie_request())
            .await
            .unwrap()
            .variants()
            .len(),
        2
    );
    assert_eq!(
        client
            .resolve(episode_request())
            .await
            .unwrap()
            .variants()
            .len(),
        1
    );
}

#[derive(Clone)]
struct LoggedResponse {
    label: &'static str,
    statuses: Arc<Vec<u16>>,
    calls: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
    success_body: &'static str,
}

impl Respond for LoggedResponse {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let status = self.statuses[call];
        let referer = request
            .headers
            .get("referer")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("none");
        let cookie = request
            .headers
            .get("cookie")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("none");
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{status}:{referer}:{cookie}", self.label));
        let response = ResponseTemplate::new(status);
        if status == 200 {
            response.set_body_string(self.success_body)
        } else {
            response.insert_header(
                "set-cookie",
                format!("{}_session=private; Path=/", self.label),
            )
        }
    }
}

#[tokio::test]
async fn idempotent_post_failover_is_bounded_rewrites_and_promotes_without_cookie_leakage() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let start = |label, statuses: Vec<u16>| {
        let log = Arc::clone(&log);
        async move {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/ajax/get_cdn_series/"))
                .respond_with(LoggedResponse {
                    label,
                    statuses: Arc::new(statuses),
                    calls: Arc::new(AtomicUsize::new(0)),
                    log,
                    success_body: include_str!("fixtures/playback_movie.json"),
                })
                .mount(&server)
                .await;
            server
        }
    };
    let first = start("A", vec![503]).await;
    let second = start("B", vec![503]).await;
    let third = start("C", vec![200, 200]).await;
    let origins = [&first, &second, &third].map(|server| Url::parse(&server.uri()).unwrap());
    let mut client = RezkaClient::new(config(origins.to_vec(), 2)).unwrap();

    client.resolve(movie_request()).await.unwrap();
    client.resolve(movie_request()).await.unwrap();

    let log = log.lock().unwrap();
    assert_eq!(log.len(), 4);
    assert!(log[0].starts_with(&format!(
        "A:503:{}",
        origins[0].join("/films/drama/101-fixture.html").unwrap()
    )));
    assert!(log[1].starts_with(&format!(
        "B:503:{}",
        origins[1].join("/films/drama/101-fixture.html").unwrap()
    )));
    assert!(log[2].starts_with(&format!(
        "C:200:{}",
        origins[2].join("/films/drama/101-fixture.html").unwrap()
    )));
    assert!(log[3].starts_with(&format!(
        "C:200:{}",
        origins[2].join("/films/drama/101-fixture.html").unwrap()
    )));
    assert!(log.iter().all(|entry| entry.ends_with(":none")));
}

#[tokio::test]
async fn rate_limit_is_terminal_and_next_operation_recovers_on_the_same_promoted_origin() {
    let first = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ajax/get_cdn_series/"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&first)
        .await;
    let second = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/ajax/get_cdn_series/"))
        .respond_with(LoggedResponse {
            label: "B",
            statuses: Arc::new(vec![429, 200]),
            calls: Arc::clone(&calls),
            log: Arc::new(Mutex::new(Vec::new())),
            success_body: include_str!("fixtures/playback_movie.json"),
        })
        .expect(2)
        .mount(&second)
        .await;
    let third = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header_exists("x-requested-with"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&third)
        .await;
    let origins = [&first, &second, &third].map(|server| Url::parse(&server.uri()).unwrap());
    let mut client = RezkaClient::new(config(origins.to_vec(), 2)).unwrap();

    assert_eq!(
        client.resolve(movie_request()).await.unwrap_err().code(),
        RezkaErrorCode::RateLimited
    );
    assert_eq!(
        client
            .resolve(movie_request())
            .await
            .unwrap()
            .variants()
            .len(),
        2
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
