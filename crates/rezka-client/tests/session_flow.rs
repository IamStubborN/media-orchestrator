use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use rezka_client::{
    RezkaErrorCode,
    mirror::MirrorSet,
    session::{
        RezkaClient, RezkaClientConfig, SessionValidation, SessionValidationProbe,
        cookie::SessionJar,
    },
};
use time::Duration;
use url::Url;
use wiremock::{
    Match, Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{header, method, path},
};

fn config(base: Url) -> RezkaClientConfig {
    RezkaClientConfig {
        mirrors: MirrorSet::new(vec![base]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(10),
        max_retries: 0,
        anubis_max_nonce: 100_000,
        proxy_url: None,
    }
}

fn probe(base: &Url) -> SessionValidationProbe {
    SessionValidationProbe::new(
        base.join("/account/probe").unwrap(),
        vec!["data-authenticated=\"true\"".to_owned()],
        vec!["name=\"login_name\"".to_owned()],
    )
    .unwrap()
}

async fn mount_login_never_called(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
}

#[tokio::test]
async fn restored_valid_cookie_jar_skips_anubis() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();

    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(header("cookie", "PHPSESSID=valid"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html data-authenticated=\"true\"></html>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_login_never_called(&server).await;

    let mut jar = SessionJar::empty();
    jar.store_response_cookies(["PHPSESSID=valid; Path=/; HttpOnly"].iter().copied(), &base);
    let snapshot = jar.export().unwrap();
    let mut restored = RezkaClient::from_snapshot(config(base.clone()), &snapshot).unwrap();

    let result = restored.ensure_session(&probe(&base)).await.unwrap();

    assert_eq!(result, SessionValidation::Valid);
}

#[derive(Clone)]
struct ProbeSequence {
    calls: Arc<AtomicUsize>,
}

impl Respond for ProbeSequence {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let cookie = request
            .headers
            .get("cookie")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();

        match call {
            0 => ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/anubis_challenge.html")),
            1 => {
                assert!(cookie.contains("anubis=opaque"));
                ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">")
            }
            _ => panic!("probe fetched more than twice"),
        }
    }
}

struct AnubisPassQuery {
    redir: String,
}

#[derive(Clone)]
struct ExpensiveChallenge;

impl Respond for ExpensiveChallenge {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        // Difficulty 5 sits at the accepted parse ceiling and still forces a large sweep: this
        // random_data has no solution below the 1_000_000 nonce budget used here, so the proof runs
        // the full sweep on a blocking thread and exhausts into ChallengeFailed deterministically.
        ResponseTemplate::new(200).set_body_string(
            r#"<script id="anubis_challenge">{"challenge":{"id":"expensive","randomData":"deliberately-expensive-proof-that-never-resolves"},"rules":{"difficulty":5}}</script>"#,
        )
    }
}

#[tokio::test(flavor = "current_thread")]
async fn expensive_proof_does_not_block_other_current_thread_tasks() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ExpensiveChallenge)
        .expect(1)
        .mount(&server)
        .await;

    let mut expensive_config = config(base.clone());
    expensive_config.anubis_max_nonce = 1_000_000;
    let mut client = RezkaClient::new(expensive_config).unwrap();
    let probe = probe(&base);
    let ticks = Arc::new(std::sync::Mutex::new(Vec::new()));
    let heartbeat_ticks = Arc::clone(&ticks);
    let heartbeat = tokio::spawn(async move {
        loop {
            heartbeat_ticks
                .lock()
                .unwrap()
                .push(std::time::Instant::now());
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let error = client.ensure_session(&probe).await.unwrap_err();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    heartbeat.abort();
    assert_eq!(error.code(), RezkaErrorCode::ChallengeFailed);

    let ticks = ticks.lock().unwrap();
    let max_gap = ticks
        .windows(2)
        .map(|window| window[1].duration_since(window[0]))
        .max()
        .unwrap();
    assert!(
        max_gap < std::time::Duration::from_millis(250),
        "current-thread heartbeat stalled for {max_gap:?}"
    );
}

impl Match for AnubisPassQuery {
    fn matches(&self, request: &Request) -> bool {
        let query: std::collections::HashMap<_, _> =
            request.url.query_pairs().into_owned().collect();
        query
            .get("id")
            .is_some_and(|value| value == "challenge-123")
            && query.get("nonce").is_some_and(|value| value == "1322")
            && query.get("response").is_some_and(|value| {
                value == "000213955c51ad382c14a1634987938c793bb005b6106a3943a16795b65227cd"
            })
            && query.get("redir").is_some_and(|value| value == &self.redir)
            && query
                .get("elapsedTime")
                .and_then(|value| value.parse::<u128>().ok())
                .is_some()
    }
}

#[tokio::test]
async fn anubis_then_anonymous_session_is_ready_without_login() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let probe_calls = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(header("user-agent", "media-orchestrator-test"))
        .respond_with(ProbeSequence {
            calls: Arc::clone(&probe_calls),
        })
        .expect(2)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .and(header("user-agent", "media-orchestrator-test"))
        .and(AnubisPassQuery {
            redir: base.join("/account/probe").unwrap().to_string(),
        })
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/must-not-follow")
                .insert_header("set-cookie", "anubis=opaque; Path=/; HttpOnly"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_login_never_called(&server).await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let result = client.ensure_session(&probe(&base)).await.unwrap();

    assert_eq!(result, SessionValidation::Invalid);
    assert_eq!(probe_calls.load(Ordering::SeqCst), 2);
    let restored = SessionJar::import(&client.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "anubis"));
}

#[tokio::test]
async fn anonymous_invalid_probe_is_accepted_without_login() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();

    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;
    mount_login_never_called(&server).await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let result = client.ensure_session(&probe(&base)).await.unwrap();

    assert_eq!(result, SessionValidation::Invalid);
}

#[tokio::test]
async fn validate_session_accepts_anonymous_and_authenticated_markers() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    assert_eq!(
        client.validate_session(&probe(&base)).await.unwrap(),
        SessionValidation::Invalid
    );
}

#[tokio::test]
async fn repeated_anubis_after_pass_returns_challenge_failed_without_login_or_loop() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/anubis_challenge.html")),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("set-cookie", "anubis=opaque; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_login_never_called(&server).await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client.ensure_session(&probe(&base)).await.unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ChallengeFailed);
}

#[test]
fn validation_probe_rejects_invalid_urls_and_marker_boundaries() {
    let url = Url::parse("https://rezka.test/account/probe").unwrap();
    for (valid, invalid) in [
        (vec![], vec!["logged-out".to_owned()]),
        (vec!["logged-in".to_owned()], vec![]),
        (vec![String::new()], vec!["logged-out".to_owned()]),
        (vec!["logged-in".to_owned()], vec!["   ".to_owned()]),
    ] {
        assert!(SessionValidationProbe::new(url.clone(), valid, invalid).is_err());
    }

    for invalid_url in [
        "ftp://rezka.test/account/probe",
        "https://user@rezka.test/account/probe",
        "https://user:password@rezka.test/account/probe",
    ] {
        assert!(
            SessionValidationProbe::new(
                Url::parse(invalid_url).unwrap(),
                vec!["logged-in".to_owned()],
                vec!["logged-out".to_owned()],
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn direct_client_api_rejects_unconfigured_probe_origin_before_any_request() {
    let configured = MockServer::start().await;
    let unconfigured = MockServer::start().await;
    let configured_origin = Url::parse(&configured.uri()).unwrap();
    let unconfigured_origin = Url::parse(&unconfigured.uri()).unwrap();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&configured)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&unconfigured)
        .await;

    let probe = SessionValidationProbe::new(
        unconfigured_origin.join("/account/probe").unwrap(),
        vec!["valid-marker".to_owned()],
        vec!["invalid-marker".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::new(config(configured_origin)).unwrap();

    let error = client.fetch_probe(&probe).await.unwrap_err();

    assert_eq!(error.code(), RezkaErrorCode::Configuration);
    assert_eq!(
        error.to_string(),
        "configuration invalid: probe origin is not a configured Rezka mirror"
    );
}

#[tokio::test]
async fn configured_non_selected_probe_origin_is_checked_then_rewritten_once() {
    let selected = MockServer::start().await;
    let other_configured = MockServer::start().await;
    let selected_origin = Url::parse(&selected.uri()).unwrap();
    let other_origin = Url::parse(&other_configured.uri()).unwrap();

    Mock::given(method("GET"))
        .and(path("/deployment/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid-marker"))
        .expect(1)
        .mount(&selected)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&other_configured)
        .await;

    let mut config = config(selected_origin.clone());
    config.mirrors = MirrorSet::new(vec![selected_origin.clone(), other_origin.clone()]).unwrap();
    config.max_retries = 1;
    let probe = SessionValidationProbe::new(
        other_origin
            .join("/deployment/probe?source=caller")
            .unwrap(),
        vec!["valid-marker".to_owned()],
        vec!["invalid-marker".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::new(config).unwrap();

    let response = client.fetch_probe(&probe).await.unwrap();

    assert_eq!(
        response.url,
        selected_origin
            .join("/deployment/probe?source=caller")
            .unwrap()
    );
    assert_eq!(response.body(), "valid-marker");
}

#[tokio::test]
async fn response_with_both_valid_and_invalid_markers_is_inconclusive() {
    assert_inconclusive_never_posts_login("logged-in logged-out").await;
}

#[tokio::test]
async fn response_with_no_markers_is_inconclusive_and_never_posts_login() {
    assert_inconclusive_never_posts_login("neutral account page").await;
}

async fn assert_inconclusive_never_posts_login(body: &'static str) {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;
    mount_login_never_called(&server).await;

    let probe = SessionValidationProbe::new(
        base.join("/account/probe").unwrap(),
        vec!["logged-in".to_owned()],
        vec!["logged-out".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::new(config(base)).unwrap();
    let error = client.ensure_session(&probe).await.unwrap_err();

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
}

#[tokio::test]
async fn anubis_is_attempted_at_most_once() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/anubis_challenge.html")),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
        .expect(1)
        .mount(&server)
        .await;
    mount_login_never_called(&server).await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client.ensure_session(&probe(&base)).await.unwrap_err();

    assert_eq!(error.code(), RezkaErrorCode::ChallengeFailed);
}
