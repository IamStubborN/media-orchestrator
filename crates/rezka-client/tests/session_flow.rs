use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use rezka_client::{
    RezkaErrorCode,
    mirror::MirrorSet,
    session::{
        RezkaClient, RezkaClientConfig, RezkaCredentials, SessionValidation,
        SessionValidationProbe, cookie::SessionJar,
    },
};
use secrecy::SecretString;
use time::Duration;
use url::Url;
use wiremock::{
    Match, Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

fn config(base: Url) -> RezkaClientConfig {
    RezkaClientConfig {
        mirrors: MirrorSet::new(vec![base]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(10),
        max_retries: 0,
        anubis_max_nonce: 100_000,
    }
}

fn credentials() -> RezkaCredentials {
    RezkaCredentials {
        username: SecretString::from("rezka-user"),
        password: SecretString::from("rezka-password"),
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

#[tokio::test]
async fn restored_valid_cookie_jar_skips_anubis_and_dle_login() {
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
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let mut jar = SessionJar::empty();
    jar.store_response_cookies(["PHPSESSID=valid; Path=/; HttpOnly"].iter().copied(), &base);
    let snapshot = jar.export().unwrap();
    let mut restored = RezkaClient::from_snapshot(config(base.clone()), &snapshot).unwrap();

    let result = restored
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap();

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
            2 => {
                assert!(cookie.contains("anubis=opaque"));
                assert!(cookie.contains("PHPSESSID=logged-in"));
                ResponseTemplate::new(200)
                    .set_body_string("<html data-authenticated=\"true\"></html>")
            }
            _ => panic!("probe fetched more than three times"),
        }
    }
}

struct AnubisPassQuery {
    redir: String,
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
async fn anubis_then_invalid_session_runs_dle_and_final_probe_using_three_total_fetches() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let probe_calls = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(header("user-agent", "media-orchestrator-test"))
        .respond_with(ProbeSequence {
            calls: Arc::clone(&probe_calls),
        })
        .expect(3)
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

    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .and(header("x-requested-with", "XMLHttpRequest"))
        .and(header("referer", format!("{}/", server.uri())))
        .and(header("user-agent", "media-orchestrator-test"))
        .and(body_string_contains("login_name=rezka-user"))
        .and(body_string_contains("login_password=rezka-password"))
        .and(body_string_contains("login_not_save=0"))
        .and(body_string_contains("login=submit"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/")
                .insert_header("set-cookie", "PHPSESSID=logged-in; Path=/; HttpOnly"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let result = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap();

    assert_eq!(result, SessionValidation::Valid);
    assert_eq!(probe_calls.load(Ordering::SeqCst), 3);
    let restored = SessionJar::import(&client.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "anubis"));
    assert!(restored.contains_cookie_for_url(&base, "PHPSESSID"));
}

#[tokio::test]
async fn failed_login_is_sanitized() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();

    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/dle_login_failed.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert!(rendered.contains("authentication failed"));
    assert!(!rendered.contains("rezka-password"));
    assert!(!rendered.contains("rezka-user"));
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
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ChallengeFailed);
}

#[tokio::test]
async fn dle_redirect_without_session_cookie_is_provider_response_invalid() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
    );
}

#[test]
fn credentials_debug_is_fully_redacted() {
    let rendered = format!("{:?}", credentials());

    assert_eq!(
        rendered,
        "RezkaCredentials { username: [REDACTED], password: [REDACTED] }"
    );
    assert!(!rendered.contains("rezka-user"));
    assert!(!rendered.contains("rezka-password"));
}

#[tokio::test]
async fn dle_http_200_success_with_session_cookie_reaches_final_valid_probe() {
    #[derive(Clone)]
    struct InvalidThenValid {
        calls: Arc<AtomicUsize>,
    }

    impl Respond for InvalidThenValid {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"),
                1 => {
                    let cookie = request.headers.get("cookie").unwrap().to_str().unwrap();
                    assert!(cookie.contains("PHPSESSID=from-json-success"));
                    ResponseTemplate::new(200)
                        .set_body_string("<html data-authenticated=\"true\"></html>")
                }
                _ => panic!("probe fetched more than twice"),
            }
        }
    }

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(InvalidThenValid {
            calls: Arc::clone(&calls),
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "set-cookie",
                    "PHPSESSID=from-json-success; Path=/; HttpOnly",
                )
                .set_body_string(include_str!("fixtures/dle_login_success.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let result = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap();

    assert_eq!(result, SessionValidation::Valid);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn dle_http_200_success_without_session_cookie_is_rejected() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    mount_invalid_probe(&server).await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/dle_login_success.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap_err();

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
}

#[tokio::test]
async fn dle_non_200_success_json_with_current_session_cookie_is_rejected() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("set-cookie", "PHPSESSID=current; Path=/; HttpOnly")
                .set_body_string(include_str!("fixtures/dle_login_success.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap_err();

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
}

#[tokio::test]
async fn dle_http_200_success_rejects_preexisting_stale_session_cookie() {
    assert_stale_cookie_does_not_authenticate(
        ResponseTemplate::new(200).set_body_string(include_str!("fixtures/dle_login_success.json")),
    )
    .await;
}

#[tokio::test]
async fn dle_http_302_rejects_preexisting_stale_session_cookie() {
    assert_stale_cookie_does_not_authenticate(
        ResponseTemplate::new(302).insert_header("location", "/"),
    )
    .await;
}

async fn assert_stale_cookie_does_not_authenticate(login_response: ResponseTemplate) {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    mount_invalid_probe(&server).await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(login_response)
        .expect(1)
        .mount(&server)
        .await;

    let mut jar = SessionJar::empty();
    jar.store_response_cookies(
        ["PHPSESSID=opaque-stale; Path=/; HttpOnly"].iter().copied(),
        &base,
    );
    let snapshot = jar.export().unwrap();
    let mut client = RezkaClient::from_snapshot(config(base.clone()), &snapshot).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
    assert!(!rendered.contains("opaque-stale"));
}

async fn mount_invalid_probe(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(server)
        .await;
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
    assert_inconclusive_never_sends_credentials("logged-in logged-out").await;
}

#[tokio::test]
async fn response_with_no_markers_is_inconclusive_and_never_sends_credentials() {
    assert_inconclusive_never_sends_credentials("neutral account page").await;
}

async fn assert_inconclusive_never_sends_credentials(body: &'static str) {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let probe = SessionValidationProbe::new(
        base.join("/account/probe").unwrap(),
        vec!["logged-in".to_owned()],
        vec!["logged-out".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::new(config(base)).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe)
        .await
        .unwrap_err();

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
}

#[tokio::test]
async fn final_invalid_probe_is_authentication_required_with_two_total_probes() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/")
                .insert_header("set-cookie", "PHPSESSID=current; Path=/; HttpOnly"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap_err();

    assert_eq!(error.code(), RezkaErrorCode::AuthenticationRequired);
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
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client
        .ensure_authenticated(&credentials(), &probe(&base))
        .await
        .unwrap_err();

    assert_eq!(error.code(), RezkaErrorCode::ChallengeFailed);
}
