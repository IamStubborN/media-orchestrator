mod support;

use rezka_client::{
    mirror::MirrorSet,
    session::cookie::{SessionJar, SessionSnapshot},
};
use url::Url;

#[test]
fn mirror_rewrite_preserves_path_and_query_but_changes_only_origin() {
    let mirrors = MirrorSet::new(vec![Url::parse("https://rezka.test").unwrap()]).unwrap();
    let title = Url::parse("https://old.example/series/drama/42-title.html?season=1").unwrap();

    let rewritten = mirrors.rewrite_to_selected(&title).unwrap();

    assert_eq!(
        rewritten.as_str(),
        "https://rezka.test/series/drama/42-title.html?season=1"
    );
}

#[test]
fn mirror_origins_reject_credentials_paths_queries_and_fragments() {
    for invalid in [
        "https://user:pass@rezka.test",
        "https://rezka.test/path",
        "https://rezka.test?query=1",
        "https://rezka.test/#fragment",
        "ftp://rezka.test",
    ] {
        let error = MirrorSet::new(vec![Url::parse(invalid).unwrap()]).unwrap_err();
        assert!(!format!("{error:?}: {error}").contains("user:pass"));
    }
}

#[test]
fn mirror_set_debug_is_exactly_redacted() {
    let mut mirrors = MirrorSet::new(vec![
        Url::parse("https://private-origin.example/").unwrap(),
        Url::parse("http://127.0.0.42:43123/").unwrap(),
    ])
    .unwrap();
    assert!(mirrors.select_next());

    let debug = format!("{mirrors:?}");

    assert_eq!(debug, "MirrorSet { origins: [REDACTED], selected: 1 }");
    for forbidden in [
        "private-origin",
        "example",
        "127.0.0.42",
        "43123",
        "https://",
        "http://",
    ] {
        assert!(!debug.contains(forbidden), "Debug leaked {forbidden}");
    }
}

#[test]
fn cookie_snapshot_round_trips_and_remains_redacted_in_debug() {
    let mut jar = SessionJar::empty();
    let origin = Url::parse("https://rezka.test/").unwrap();
    jar.store_response_cookies(
        [
            "session_cookie=opaque-a; Path=/; HttpOnly",
            "persistent_cookie=opaque-b; Path=/; Max-Age=3600",
            "expired_cookie=opaque-c; Path=/; Max-Age=0",
        ]
        .iter()
        .copied(),
        &origin,
    );

    let jar_debug = format!("{jar:?}");
    assert!(jar_debug.contains("[REDACTED]"));
    assert!(!jar_debug.contains("session_cookie"));
    assert!(!jar_debug.contains("opaque-a"));

    let snapshot = jar.export().unwrap();
    let debug = format!("{snapshot:?}");
    assert_eq!(debug, "SessionSnapshot { bytes: [REDACTED] }");
    assert!(!debug.contains("session_cookie"));
    assert!(!debug.contains("opaque-a"));

    let restored = SessionJar::import(&snapshot).unwrap();
    assert!(restored.contains_cookie_for_url(&origin, "session_cookie"));
    assert!(restored.contains_cookie_for_url(&origin, "persistent_cookie"));
    assert!(!restored.contains_cookie_for_url(&origin, "expired_cookie"));
}

#[test]
fn session_jar_never_matches_cookies_outside_its_exact_origin() {
    let mut jar = SessionJar::empty();
    let origin = Url::parse("https://rezka.test/").unwrap();
    jar.store_response_cookies(
        ["site_session=opaque; Domain=rezka.test; Path=/; HttpOnly"]
            .iter()
            .copied(),
        &origin,
    );

    for forbidden in [
        "https://cdn.rezka.test/account/probe",
        "https://rezka.test:444/account/probe",
        "http://rezka.test/account/probe",
    ] {
        let forbidden = Url::parse(forbidden).unwrap();
        assert!(!jar.contains_cookie_for_url(&forbidden, "site_session"));
    }
}

#[test]
fn cookie_header_count_boundary_is_atomic() {
    const MAX_HEADERS: usize = 64;
    let origin = Url::parse("https://rezka.test/").unwrap();
    let mut accepted = SessionJar::empty();
    let boundary = (0..MAX_HEADERS)
        .map(|index| format!("cookie_{index}=value; Path=/"))
        .collect::<Vec<_>>();
    accepted.store_response_cookies(boundary.iter().map(String::as_str), &origin);
    for index in 0..MAX_HEADERS {
        assert!(accepted.contains_cookie_for_url(&origin, &format!("cookie_{index}")));
    }

    let mut rejected = SessionJar::empty();
    let over_limit = (0..=MAX_HEADERS)
        .map(|index| format!("rejected_{index}=value; Path=/"))
        .collect::<Vec<_>>();
    rejected.store_response_cookies(over_limit.iter().map(String::as_str), &origin);
    for index in 0..=MAX_HEADERS {
        assert!(!rejected.contains_cookie_for_url(&origin, &format!("rejected_{index}")));
    }
}

#[test]
fn cookie_header_byte_boundary_is_atomic() {
    const MAX_HEADER_BYTES: usize = 8 * 1024;
    let origin = Url::parse("https://rezka.test/").unwrap();
    let header = |name: &str, len: usize| {
        let fixed = name.len() + "=; Path=/".len();
        format!("{name}={}; Path=/", "x".repeat(len - fixed))
    };
    let mut jar = SessionJar::empty();
    let boundary = header("boundary", MAX_HEADER_BYTES);
    jar.store_response_cookies([boundary.as_str()].into_iter(), &origin);
    assert!(jar.contains_cookie_for_url(&origin, "boundary"));

    let oversized = header("oversized", MAX_HEADER_BYTES + 1);
    jar.store_response_cookies([oversized.as_str()].into_iter(), &origin);
    assert!(jar.contains_cookie_for_url(&origin, "boundary"));
    assert!(!jar.contains_cookie_for_url(&origin, "oversized"));
}

#[test]
fn accepted_cookie_count_boundary_rejects_the_whole_response() {
    const MAX_COOKIES: usize = 64;
    let origin = Url::parse("https://rezka.test/").unwrap();
    let mut jar = SessionJar::empty();
    let boundary = (0..MAX_COOKIES)
        .map(|index| format!("cookie_{index}=value; Path=/"))
        .collect::<Vec<_>>();
    jar.store_response_cookies(boundary.iter().map(String::as_str), &origin);

    jar.store_response_cookies(["cookie_64=must-not-commit; Path=/"].into_iter(), &origin);

    assert!(!jar.contains_cookie_for_url(&origin, "cookie_64"));
    for index in 0..MAX_COOKIES {
        assert!(jar.contains_cookie_for_url(&origin, &format!("cookie_{index}")));
    }
}

#[test]
fn oversized_snapshot_candidate_rejects_all_response_cookies_atomically() {
    let origin = Url::parse("https://rezka.test/").unwrap();
    let mut jar = SessionJar::empty();
    jar.store_response_cookies(["baseline=retained; Path=/"].into_iter(), &origin);
    let oversized = (0..63)
        .map(|index| format!("large_{index}={}; Path=/", "x".repeat(2_100)))
        .collect::<Vec<_>>();

    jar.store_response_cookies(oversized.iter().map(String::as_str), &origin);

    assert!(jar.contains_cookie_for_url(&origin, "baseline"));
    for index in 0..63 {
        assert!(!jar.contains_cookie_for_url(&origin, &format!("large_{index}")));
    }
}

#[tokio::test]
async fn failover_to_another_port_discards_the_previous_origin_jar() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header_exists, method, path},
    };

    let first = MockServer::start().await;
    let first_origin = Url::parse(&first.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("set-cookie", "origin_session=must-not-leak; Path=/"),
        )
        .expect(1)
        .mount(&first)
        .await;

    let second = MockServer::start().await;
    let second_origin = Url::parse(&second.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(header_exists("cookie"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&second)
        .await;
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid-marker"))
        .expect(1)
        .mount(&second)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![first_origin.clone(), second_origin.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        1,
    )
    .unwrap();

    let response = transport
        .get_first_with_failover(first_origin.join("/account/probe").unwrap(), None)
        .await
        .unwrap();

    assert_eq!(response.url, second_origin.join("/account/probe").unwrap());
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(!restored.contains_cookie_for_url(&second_origin, "origin_session"));
}

#[tokio::test]
async fn snapshot_restart_selects_its_configured_alternate_origin() {
    use rezka_client::session::{
        RezkaClient, RezkaClientConfig, SessionValidation, SessionValidationProbe,
    };
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    let primary = MockServer::start().await;
    let primary_origin = Url::parse(&primary.uri()).unwrap();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&primary)
        .await;

    let alternate = MockServer::start().await;
    let alternate_origin = Url::parse(&alternate.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(header("cookie", "PHPSESSID=alternate"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid-marker"))
        .expect(1)
        .mount(&alternate)
        .await;

    let mut jar = SessionJar::empty();
    jar.store_response_cookies(
        ["PHPSESSID=alternate; Path=/; HttpOnly"].iter().copied(),
        &alternate_origin,
    );
    let snapshot = jar.export().unwrap();
    let config = RezkaClientConfig {
        mirrors: MirrorSet::new(vec![primary_origin, alternate_origin.clone()]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: 1,
        anubis_max_nonce: 1,
    };
    let probe = SessionValidationProbe::new(
        alternate_origin.join("/account/probe").unwrap(),
        vec!["valid-marker".to_owned()],
        vec!["invalid-marker".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::from_snapshot(config, &snapshot).unwrap();

    let response = client.fetch_probe(&probe).await.unwrap();

    assert_eq!(
        RezkaClient::classify_probe(&probe, &response),
        SessionValidation::Valid
    );
    assert_eq!(response.url.origin(), alternate_origin.origin());
}

#[tokio::test]
async fn restored_last_origin_can_fail_over_to_the_former_primary_without_cookie_leakage() {
    use rezka_client::session::{RezkaClient, RezkaClientConfig, SessionValidationProbe};
    use std::net::TcpListener;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header_exists, method, path},
    };

    let primary = MockServer::start().await;
    let primary_origin = Url::parse(&primary.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(header_exists("cookie"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&primary)
        .await;
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid-marker"))
        .expect(1)
        .mount(&primary)
        .await;

    let unavailable_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let last_origin = Url::parse(&format!(
        "http://{}",
        unavailable_listener.local_addr().unwrap()
    ))
    .unwrap();
    drop(unavailable_listener);
    let mut jar = SessionJar::empty();
    jar.store_response_cookies(
        ["last_origin_session=must-not-leak; Path=/"].into_iter(),
        &last_origin,
    );
    let snapshot = jar.export().unwrap();
    let config = RezkaClientConfig {
        mirrors: MirrorSet::new(vec![primary_origin.clone(), last_origin.clone()]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: 1,
        anubis_max_nonce: 1,
    };
    let probe = SessionValidationProbe::new(
        last_origin.join("/account/probe").unwrap(),
        vec!["valid-marker".to_owned()],
        vec!["invalid-marker".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::from_snapshot(config, &snapshot).unwrap();

    let response = client.fetch_probe(&probe).await.unwrap();

    assert_eq!(response.url.origin(), primary_origin.origin());
}

#[derive(Clone)]
struct MirrorResponseSequence {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    statuses: std::sync::Arc<Vec<u16>>,
}

impl wiremock::Respond for MirrorResponseSequence {
    fn respond(&self, _request: &wiremock::Request) -> wiremock::ResponseTemplate {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let status = self.statuses[call];
        let response = wiremock::ResponseTemplate::new(status);
        if status == 200 {
            response.set_body_string("valid-marker")
        } else {
            response
        }
    }
}

#[tokio::test]
async fn successful_failovers_promote_origins_for_repeated_non_wrapping_operations() {
    use rezka_client::transport::Transport;
    use std::sync::{Arc, atomic::AtomicUsize};
    use time::Duration;
    use wiremock::{Mock, MockServer, matchers::path};

    let first = MockServer::start().await;
    let first_origin = Url::parse(&first.uri()).unwrap();
    Mock::given(path("/account/probe"))
        .respond_with(MirrorResponseSequence {
            calls: Arc::new(AtomicUsize::new(0)),
            statuses: Arc::new(vec![503, 200, 200]),
        })
        .expect(3)
        .mount(&first)
        .await;

    let second = MockServer::start().await;
    let second_origin = Url::parse(&second.uri()).unwrap();
    Mock::given(path("/account/probe"))
        .respond_with(MirrorResponseSequence {
            calls: Arc::new(AtomicUsize::new(0)),
            statuses: Arc::new(vec![200, 503]),
        })
        .expect(2)
        .mount(&second)
        .await;

    let third = MockServer::start().await;
    let third_origin = Url::parse(&third.uri()).unwrap();
    Mock::given(path("/account/probe"))
        .respond_with(MirrorResponseSequence {
            calls: Arc::new(AtomicUsize::new(0)),
            statuses: Arc::new(vec![503]),
        })
        .expect(1)
        .mount(&third)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![
            first_origin.clone(),
            second_origin.clone(),
            third_origin,
        ])
        .unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        2,
    )
    .unwrap();
    let probe = first_origin.join("/account/probe").unwrap();

    assert_eq!(
        transport
            .get_first_with_failover(probe.clone(), None)
            .await
            .unwrap()
            .url
            .origin(),
        second_origin.origin()
    );
    assert_eq!(
        transport
            .get_first_with_failover(probe.clone(), None)
            .await
            .unwrap()
            .url
            .origin(),
        first_origin.origin()
    );
    assert_eq!(
        transport
            .get_first_with_failover(probe, None)
            .await
            .unwrap()
            .url
            .origin(),
        first_origin.origin()
    );
}

#[derive(Clone)]
struct LoggedFailoverResponse {
    label: &'static str,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    statuses: std::sync::Arc<Vec<u16>>,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl wiremock::Respond for LoggedFailoverResponse {
    fn respond(&self, request: &wiremock::Request) -> wiremock::ResponseTemplate {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cookie = request
            .headers
            .get("cookie")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("none");
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{cookie}", self.label));
        let response = wiremock::ResponseTemplate::new(self.statuses[call]);
        if call == 0 {
            response.insert_header(
                "set-cookie",
                format!("{}_session=private; Path=/", self.label),
            )
        } else {
            response
        }
    }
}

#[tokio::test]
async fn terminal_full_failover_promotes_last_attempt_for_the_next_operation() {
    use rezka_client::transport::Transport;
    use std::sync::{Arc, Mutex, atomic::AtomicUsize};
    use time::Duration;
    use wiremock::{Mock, MockServer, matchers::path};

    let log = Arc::new(Mutex::new(Vec::new()));
    let start = |label: &'static str, statuses: Vec<u16>| {
        let log = Arc::clone(&log);
        async move {
            let server = MockServer::start().await;
            Mock::given(path("/account/probe"))
                .respond_with(LoggedFailoverResponse {
                    label,
                    calls: Arc::new(AtomicUsize::new(0)),
                    statuses: Arc::new(statuses),
                    log,
                })
                .mount(&server)
                .await;
            server
        }
    };
    let first = start("A", vec![503, 200]).await;
    let second = start("B", vec![503]).await;
    let third = start("C", vec![503, 503]).await;
    let first_origin = Url::parse(&first.uri()).unwrap();
    let second_origin = Url::parse(&second.uri()).unwrap();
    let third_origin = Url::parse(&third.uri()).unwrap();
    let mut transport = Transport::new(
        MirrorSet::new(vec![
            first_origin.clone(),
            second_origin,
            third_origin.clone(),
        ])
        .unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        2,
    )
    .unwrap();
    let probe = first_origin.join("/account/probe").unwrap();

    let first_error = transport
        .get_first_with_failover(probe.clone(), None)
        .await
        .unwrap_err();
    assert_eq!(first_error.code(), rezka_client::RezkaErrorCode::Transport);
    assert_eq!(transport.selected_origin(), &third_origin);

    let response = transport
        .get_first_with_failover(probe, None)
        .await
        .unwrap();

    assert_eq!(response.url.origin(), first_origin.origin());
    assert_eq!(
        *log.lock().unwrap(),
        [
            "A:none",
            "B:none",
            "C:none",
            "C:C_session=private",
            "A:none",
        ]
    );
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(!restored.contains_cookie_for_url(&first_origin, "C_session"));
    assert!(!restored.contains_cookie_for_url(&third_origin, "C_session"));
}

#[test]
fn snapshot_origin_absent_from_configured_mirrors_fails_closed() {
    use rezka_client::session::{RezkaClient, RezkaClientConfig};
    use time::Duration;

    let snapshot_origin = Url::parse("https://snapshot.rezka.test/").unwrap();
    let mut jar = SessionJar::empty();
    jar.store_response_cookies(
        ["PHPSESSID=opaque; Path=/; HttpOnly"].iter().copied(),
        &snapshot_origin,
    );
    let snapshot = jar.export().unwrap();
    let config = RezkaClientConfig {
        mirrors: MirrorSet::new(vec![Url::parse("https://configured.rezka.test/").unwrap()])
            .unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: 0,
        anubis_max_nonce: 1,
    };

    let error = match RezkaClient::from_snapshot(config, &snapshot) {
        Ok(_) => panic!("unconfigured snapshot origin must fail closed"),
        Err(error) => error,
    };

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::Configuration);
    assert_eq!(
        error.to_string(),
        "configuration invalid: snapshot origin is not a configured Rezka mirror"
    );
}

#[tokio::test]
async fn transport_rejects_unrelated_and_same_site_cross_origin_cookie_targets() {
    use rezka_client::transport::Transport;
    use time::Duration;

    let mut jar = SessionJar::empty();
    let site = Url::parse("https://rezka.test/").unwrap();
    jar.store_response_cookies(
        ["site_session=opaque; Domain=rezka.test; Path=/; HttpOnly"]
            .iter()
            .copied(),
        &site,
    );
    let jar = SessionJar::import(&jar.export().unwrap()).unwrap();
    let mirrors = MirrorSet::new(vec![site.clone()]).unwrap();
    let mut transport = Transport::new(
        mirrors,
        jar,
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    )
    .unwrap();

    for forbidden in [
        "https://cdn.test/video.mp4?sig=x",
        "https://cdn.rezka.test/video.mp4?sig=x",
        "https://rezka.test:444/video.mp4?sig=x",
        "http://rezka.test/video.mp4?sig=x",
    ] {
        let error = transport
            .get_first(Url::parse(forbidden).unwrap(), None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), rezka_client::RezkaErrorCode::Configuration);
    }
}

#[test]
fn invalid_cookie_snapshot_does_not_expose_plaintext() {
    use secrecy::SecretBox;

    let snapshot =
        SessionSnapshot::from_secret_bytes(SecretBox::new(Box::new(vec![0xff, 0x00, 0x7f, 0x01])));
    let error = SessionJar::import(&snapshot).unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert_eq!(
        error.code(),
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
    );
    assert!(!rendered.contains("255"));
}

#[tokio::test]
async fn explicit_bounded_redirects_store_cookies_from_every_hop() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/middle")
                .insert_header("set-cookie", "hop_a=opaque-a; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/middle"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/done")
                .insert_header("set-cookie", "hop_b=opaque-b; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/done"))
        .respond_with(ResponseTemplate::new(200).set_body_string("done"))
        .expect(1)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    )
    .unwrap();
    let response = transport
        .get_following(base.join("/start").unwrap(), None, 2)
        .await
        .unwrap();
    assert_eq!(response.url.path(), "/done");
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "hop_a"));
    assert!(restored.contains_cookie_for_url(&base, "hop_b"));
}

#[tokio::test]
async fn explicit_redirect_follower_stops_at_the_configured_bound() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/middle"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/middle"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/must-not-follow"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/must-not-follow"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    )
    .unwrap();
    let error = transport
        .get_following(base.join("/start").unwrap(), None, 1)
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
    );
    assert!(!format!("{error:?}: {error}").contains("must-not-follow"));
}

#[tokio::test]
async fn eligible_connect_failure_selects_next_mirror_within_retry_bound() {
    use rezka_client::transport::Transport;
    use std::net::TcpListener;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    let unavailable_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let unavailable = Url::parse(&format!(
        "http://{}",
        unavailable_listener.local_addr().unwrap()
    ))
    .unwrap();
    drop(unavailable_listener);

    let server = MockServer::start().await;
    let available = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(query_param("source", "configured"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid-marker"))
        .expect(1)
        .mount(&server)
        .await;

    let mirrors = MirrorSet::new(vec![unavailable.clone(), available.clone()]).unwrap();
    let mut transport = Transport::new(
        mirrors,
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        1,
    )
    .unwrap();
    let configured_probe = unavailable
        .join("/account/probe?source=configured")
        .unwrap();

    let response = transport
        .get_first_with_failover(configured_probe, None)
        .await
        .unwrap();

    assert_eq!(
        response.url.as_str(),
        available
            .join("/account/probe?source=configured")
            .unwrap()
            .as_str()
    );
    assert_eq!(transport.selected_origin(), &available);
}

#[tokio::test]
async fn later_failover_invocation_starts_at_the_currently_selected_mirror() {
    use rezka_client::transport::Transport;
    use std::net::TcpListener;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    let unavailable_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let primary = Url::parse(&format!(
        "http://{}",
        unavailable_listener.local_addr().unwrap()
    ))
    .unwrap();
    drop(unavailable_listener);

    let second = MockServer::start().await;
    let selected = Url::parse(&second.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(query_param("source", "configured"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid-marker"))
        .expect(2)
        .mount(&second)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![primary.clone(), selected.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        1,
    )
    .unwrap();
    let configured_probe = primary.join("/account/probe?source=configured").unwrap();

    let first = transport
        .get_first_with_failover(configured_probe.clone(), None)
        .await
        .unwrap();
    let second = transport
        .get_first_with_failover(configured_probe, None)
        .await
        .unwrap();

    let selected_probe = selected.join("/account/probe?source=configured").unwrap();
    assert_eq!(first.url, selected_probe);
    assert_eq!(second.url, selected_probe);
    assert_eq!(transport.selected_origin(), &selected);
}

#[tokio::test]
async fn zero_retry_budget_never_contacts_or_selects_second_mirror() {
    use rezka_client::transport::Transport;
    use std::net::TcpListener;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let unavailable_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let unavailable = Url::parse(&format!(
        "http://{}",
        unavailable_listener.local_addr().unwrap()
    ))
    .unwrap();
    drop(unavailable_listener);
    let second = MockServer::start().await;
    let available = Url::parse(&second.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&second)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![unavailable.clone(), available]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();

    let error = transport
        .get_first_with_failover(unavailable.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::Transport);
    assert_eq!(transport.selected_origin(), &unavailable);
}

#[tokio::test]
async fn exhausted_eligible_upstream_status_returns_transport_error() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();

    let error = transport
        .get_first_with_failover(base.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::Transport);
}

#[tokio::test]
async fn truncated_eligible_status_fails_over_before_body_read() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let (unavailable, raw_server) = support::spawn_truncated_http_response(
        "503 Service Unavailable",
        &["Set-Cookie: first_status_cookie=opaque; Path=/"],
    );
    let second = MockServer::start().await;
    let available = Url::parse(&second.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid-marker"))
        .expect(1)
        .mount(&second)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![unavailable.clone(), available.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        1,
    )
    .unwrap();

    let response = transport
        .get_first_with_failover(unavailable.join("/account/probe").unwrap(), None)
        .await
        .unwrap();

    raw_server.join().unwrap();
    assert_eq!(response.url, available.join("/account/probe").unwrap());
    assert_eq!(transport.selected_origin(), &available);
}

#[tokio::test]
async fn truncated_rate_limit_preserves_retry_after_and_response_cookie() {
    use rezka_client::{RezkaError, transport::Transport};
    use time::Duration;

    let (base, raw_server) = support::spawn_truncated_http_response(
        "429 Too Many Requests",
        &[
            "Retry-After: 17",
            "Set-Cookie: rate_limit_cookie=opaque; Path=/",
        ],
    );
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();

    let error = transport
        .get_first(base.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    raw_server.join().unwrap();
    match error {
        RezkaError::RateLimited {
            retry_after_seconds,
        } => assert_eq!(retry_after_seconds, Some(17)),
        other => panic!("expected rate limit, got {other:?}"),
    }
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "rate_limit_cookie"));
}

#[tokio::test]
async fn truncated_terminal_status_is_classified_before_body_read() {
    use rezka_client::transport::Transport;
    use time::Duration;

    let (base, raw_server) = support::spawn_truncated_http_response(
        "401 Unauthorized",
        &["Set-Cookie: terminal_status_cookie=opaque; Path=/"],
    );
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();

    let error = transport
        .get_first(base.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    raw_server.join().unwrap();
    assert_eq!(
        error.code(),
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
    );
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "terminal_status_cookie"));
}

#[tokio::test]
async fn provider_response_body_accepts_the_exact_two_mib_boundary() {
    use rezka_client::transport::Transport;
    use time::Duration;

    const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
    let (base, raw_server) = support::spawn_http_body_response(&[], MAX_BODY_BYTES, false);
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    )
    .unwrap();

    let response = transport
        .get_first(base.join("/account/probe").unwrap(), None)
        .await
        .unwrap();

    raw_server.join().unwrap();
    assert_eq!(response.body.len(), MAX_BODY_BYTES);
}

#[tokio::test]
async fn declared_oversized_provider_body_is_rejected_before_reading() {
    use rezka_client::transport::Transport;
    use time::Duration;

    const OVERSIZED_BODY_BYTES: usize = 2 * 1024 * 1024 + 1;
    let (base, raw_server) = support::spawn_declared_http_body_response(OVERSIZED_BODY_BYTES);
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    )
    .unwrap();

    let error = transport
        .get_first(base.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    raw_server.join().unwrap();
    assert_eq!(
        error.code(),
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
    );
}

#[tokio::test]
async fn chunked_oversized_provider_body_is_rejected_at_one_byte_overflow() {
    use rezka_client::transport::Transport;
    use time::Duration;

    const OVERSIZED_BODY_BYTES: usize = 2 * 1024 * 1024 + 1;
    let (base, raw_server) = support::spawn_http_body_response(&[], OVERSIZED_BODY_BYTES, true);
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    )
    .unwrap();

    let error = transport
        .get_first(base.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    raw_server.join().unwrap();
    assert_eq!(
        error.code(),
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
    );
}

#[tokio::test]
async fn response_cookie_is_stored_before_oversized_body_failure() {
    use rezka_client::transport::Transport;
    use time::Duration;

    const OVERSIZED_BODY_BYTES: usize = 2 * 1024 * 1024 + 1;
    let (base, raw_server) = support::spawn_http_body_response(
        &["Set-Cookie: oversize_cookie=opaque; Path=/"],
        OVERSIZED_BODY_BYTES,
        true,
    );
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    )
    .unwrap();

    let error = transport
        .get_first(base.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    raw_server.join().unwrap();
    assert_eq!(
        error.code(),
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
    );
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "oversize_cookie"));
}

#[tokio::test]
async fn terminal_http_status_context_is_useful_and_structurally_redacted() {
    use rezka_client::transport::Transport;
    use time::Duration;

    let mut rendered_errors = Vec::new();

    for (status_line, status_code) in [
        ("401 Unauthorized", 401),
        ("403 Forbidden", 403),
        ("500 Internal Server Error", 500),
    ] {
        let (base, raw_server) = support::spawn_truncated_http_response(status_line, &[]);
        let ip_literal = base.host_str().unwrap().to_owned();
        let mut request_url = base.join("/private/account").unwrap();
        request_url.set_username("private-user").unwrap();
        request_url.set_password(Some("private-password")).unwrap();
        request_url.set_query(Some("token=query-secret"));
        let mut transport = Transport::new(
            MirrorSet::new(vec![base]).unwrap(),
            SessionJar::empty(),
            "media-orchestrator-test".to_owned(),
            Duration::seconds(2),
            0,
        )
        .unwrap();

        let error = transport.get_first(request_url, None).await.unwrap_err();
        raw_server.join().unwrap();
        assert_eq!(
            error.code(),
            rezka_client::RezkaErrorCode::ProviderResponseInvalid
        );

        let rendered = format!("{error:?}: {error}");
        assert!(rendered.contains(&format!("HTTP {status_code}")));
        assert!(rendered.contains("redacted.invalid"));
        assert!(rendered.contains("/private/account"));
        for forbidden in [
            "private-user",
            "private-password",
            "token",
            "query-secret",
            ip_literal.as_str(),
            "raw-body-secret",
        ] {
            assert!(!rendered.contains(forbidden), "leaked {forbidden}");
        }
        rendered_errors.push(rendered);
    }

    assert_ne!(rendered_errors[0], rendered_errors[1]);
    assert_ne!(rendered_errors[1], rendered_errors[2]);
    assert_ne!(rendered_errors[0], rendered_errors[2]);
}

#[tokio::test]
async fn first_response_does_not_auto_redirect_and_debug_remains_redacted() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/must-not-follow?token=location-secret")
                .insert_header("set-cookie", "first_hop=opaque-cookie-value; Path=/")
                .set_body_string("raw-provider-secret"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/must-not-follow"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();

    let response = transport
        .get_first(base.join("/start?token=request-secret").unwrap(), None)
        .await
        .unwrap();

    assert_eq!(response.status, reqwest::StatusCode::FOUND);
    assert!(response.stored_cookie_names().contains("first_hop"));
    let debug = format!("{response:?}");
    for secret in [
        "request-secret",
        "location-secret",
        "opaque-cookie-value",
        "raw-provider-secret",
    ] {
        assert!(!debug.contains(secret));
    }
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "first_hop"));
}

#[tokio::test]
async fn rejected_response_cookie_is_absent_from_metadata_and_jar() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("set-cookie", "accepted_cookie=opaque-a; Path=/")
                .append_header(
                    "set-cookie",
                    "PHPSESSID=must-be-rejected; Domain=unrelated.test; Path=/",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();

    let response = transport
        .get_first(base.join("/login").unwrap(), None)
        .await
        .unwrap();

    assert!(response.stored_cookie_names().contains("accepted_cookie"));
    assert!(!response.stored_cookie_names().contains("PHPSESSID"));
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "accepted_cookie"));
    assert!(!restored.contains_cookie_for_url(&base, "PHPSESSID"));
}

#[tokio::test]
async fn over_budget_cookie_response_is_sanitized_and_does_not_mutate_the_jar() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let mut response = ResponseTemplate::new(200);
    for index in 0..65 {
        response = response.append_header(
            "set-cookie",
            format!("budget_cookie_{index}=secret-value; Path=/"),
        );
    }
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();
    let error = transport
        .get_first(base.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert_eq!(
        error.code(),
        rezka_client::RezkaErrorCode::ProviderResponseInvalid
    );
    assert!(!rendered.contains("budget_cookie"));
    assert!(!rendered.contains("secret-value"));
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    for index in 0..65 {
        assert!(!restored.contains_cookie_for_url(&base, &format!("budget_cookie_{index}")));
    }
}

#[tokio::test]
async fn rate_limit_is_terminal_and_does_not_select_the_next_mirror() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let first = MockServer::start().await;
    let first_origin = Url::parse(&first.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "17"))
        .expect(1)
        .mount(&first)
        .await;
    let second = MockServer::start().await;
    let second_origin = Url::parse(&second.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&second)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![first_origin.clone(), second_origin]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        1,
    )
    .unwrap();

    let error = transport
        .get_first_with_failover(first_origin.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::RateLimited);
    assert_eq!(transport.selected_origin(), &first_origin);
}

#[tokio::test]
async fn non_title_operations_continue_to_reject_not_found_statuses() {
    use rezka_client::{
        RezkaErrorCode,
        session::{
            RezkaClient, RezkaClientConfig, SessionValidationProbe,
            anubis::{AnubisChallenge, AnubisProof, submit_challenge},
        },
        transport::Transport,
    };
    use time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let origin = Url::parse(&server.uri()).unwrap();
    for (request_method, request_path) in [
        ("GET", "/generic"),
        ("GET", "/account/probe"),
        ("GET", "/.within.website/x/cmd/anubis/api/pass-challenge"),
    ] {
        Mock::given(method(request_method))
            .and(path(request_path))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
    }

    let mut transport = Transport::new(
        MirrorSet::new(vec![origin.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();
    let error = transport
        .get_first(origin.join("/generic").unwrap(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);

    let probe = SessionValidationProbe::new(
        origin.join("/account/probe").unwrap(),
        vec!["valid".to_owned()],
        vec!["invalid".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::new(RezkaClientConfig {
        mirrors: MirrorSet::new(vec![origin.clone()]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(2),
        max_retries: 0,
        anubis_max_nonce: 1,
    })
    .unwrap();
    let error = client.fetch_probe(&probe).await.unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);

    let error = submit_challenge(
        &mut transport,
        &AnubisChallenge {
            id: "challenge".to_owned(),
            random_data: "random".to_owned(),
            difficulty: 1,
        },
        &AnubisProof {
            response_hex: "proof".to_owned(),
            nonce: 0,
        },
        origin.join("/redir").unwrap(),
        0,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
}
