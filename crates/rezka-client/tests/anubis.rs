use rezka_client::session::anubis::{detect_challenge, parse_challenge, solve_challenge};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct StaticBrowserFallback;

impl rezka_client::session::anubis::BrowserChallengeFallback for StaticBrowserFallback {
    fn solve<'a>(
        &'a mut self,
        _challenge: &'a rezka_client::session::anubis::AnubisChallenge,
        _origin: &'a url::Url,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<String>, rezka_client::RezkaError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async {
            Ok(vec![
                "techaro.lol-anubis-auth=browser-clearance; Path=/; HttpOnly".to_owned(),
            ])
        })
    }
}

struct CountingBrowserFallback(Arc<AtomicUsize>);

impl rezka_client::session::anubis::BrowserChallengeFallback for CountingBrowserFallback {
    fn solve<'a>(
        &'a mut self,
        _challenge: &'a rezka_client::session::anubis::AnubisChallenge,
        _origin: &'a url::Url,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<String>, rezka_client::RezkaError>>
                + Send
                + 'a,
        >,
    > {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(vec![
                "techaro.lol-anubis-auth=browser-clearance; Path=/; HttpOnly".to_owned(),
            ])
        })
    }
}

#[derive(Clone)]
struct RepeatedChallengeSequence {
    calls: Arc<AtomicUsize>,
    challenge: String,
}

impl wiremock::Respond for RepeatedChallengeSequence {
    fn respond(&self, request: &wiremock::Request) -> wiremock::ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let cookie = request
            .headers
            .get("cookie")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        match call {
            0 => {
                assert!(cookie.is_empty());
                wiremock::ResponseTemplate::new(200).set_body_string(self.challenge.clone())
            }
            1 => {
                assert!(cookie.contains("techaro.lol-anubis-auth=native"));
                wiremock::ResponseTemplate::new(200).set_body_string(self.challenge.clone())
            }
            2 => {
                assert!(cookie.contains("techaro.lol-anubis-auth=browser-clearance"));
                wiremock::ResponseTemplate::new(200).set_body_string("provider-content")
            }
            _ => panic!("original request retried more than twice"),
        }
    }
}

fn challenge_html(id: &str, random_data: &str, difficulty: u8) -> String {
    format!(
        r#"<script id="anubis_challenge">{{"challenge":{{"id":"{id}","randomData":"{random_data}"}},"rules":{{"difficulty":{difficulty}}}}}</script>"#
    )
}

#[test]
fn detects_and_parses_anubis_challenge_from_html_200_body() {
    let html = include_str!("fixtures/anubis_challenge.html");

    let challenge = parse_challenge(html).unwrap();

    assert!(detect_challenge(html));
    assert_eq!(challenge.id, "challenge-123");
    assert_eq!(challenge.random_data, "abc");
    assert_eq!(challenge.difficulty, 3);
}

#[test]
fn detection_requires_an_element_with_the_challenge_id() {
    let marker_only = r#"<script>const marker = 'id="anubis_challenge"';</script>"#;
    let non_script_element = r#"<div id="anubis_challenge"></div>"#;

    assert!(!detect_challenge(marker_only));
    assert!(detect_challenge(non_script_element));
}

#[test]
fn solves_even_and_odd_leading_zero_nibble_difficulties() {
    let html = include_str!("fixtures/anubis_challenge.html");
    let mut challenge = parse_challenge(html).unwrap();

    challenge.difficulty = 2;
    let even = solve_challenge(&challenge, 100_000).unwrap();
    assert!(even.response_hex.starts_with("00"));

    challenge.difficulty = 3;
    let odd = solve_challenge(&challenge, 100_000).unwrap();
    assert!(odd.response_hex.starts_with("000"));
}

#[test]
fn proof_uses_lowercase_hex_and_includes_max_nonce() {
    let html = include_str!("fixtures/anubis_challenge.html");
    let mut challenge = parse_challenge(html).unwrap();
    challenge.difficulty = 1;

    assert!(solve_challenge(&challenge, 25).is_err());

    let proof = solve_challenge(&challenge, 26).unwrap();
    assert_eq!(proof.nonce, 26);
    assert_eq!(
        proof.response_hex,
        "0d56d5ce616422904a584cf3735a45ae611817d96c4717b17369ef6778025848"
    );
    assert!(
        proof
            .response_hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn malformed_challenge_and_excessive_work_are_sanitized_failures() {
    let malformed = include_str!("fixtures/anubis_malformed.html");
    let malformed_error = parse_challenge(malformed).unwrap_err();
    assert!(!format!("{malformed_error:?}: {malformed_error}").contains("randomData"));

    let html = include_str!("fixtures/anubis_challenge.html");
    let mut challenge = parse_challenge(html).unwrap();
    challenge.difficulty = 64;
    let bounded_error = solve_challenge(&challenge, 10).unwrap_err();
    assert!(format!("{bounded_error}").contains("challenge failed"));
}

#[test]
fn rejects_empty_values_and_out_of_range_difficulties_without_raw_json() {
    for raw_json in [
        r#"{"challenge":{"id":"","randomData":"secret-random-data"},"rules":{"difficulty":3}}"#,
        r#"{"challenge":{"id":"secret-id","randomData":""},"rules":{"difficulty":3}}"#,
        r#"{"challenge":{"id":"secret-id","randomData":"secret-random-data"},"rules":{"difficulty":0}}"#,
    ] {
        let html = format!(r#"<script id="anubis_challenge">{raw_json}</script>"#);
        let error = parse_challenge(&html).unwrap_err();
        let rendered = format!("{error:?}: {error}");

        assert_eq!(
            error.code(),
            rezka_client::RezkaErrorCode::ProviderResponseInvalid
        );
        assert!(!rendered.contains(raw_json));
        assert!(!rendered.contains("secret-id"));
        assert!(!rendered.contains("secret-random-data"));
    }

    // Difficulty counts leading zero nibbles, so the accepted ceiling is 5 (~2^20 ~= 1M expected
    // hashes, solvable under the production 5M nonce budget); 6 (~2^24 ~= 16M) is unreachable.
    let upper_bound = r#"<script id="anubis_challenge">{"challenge":{"id":"id","randomData":"data"},"rules":{"difficulty":5}}</script>"#;
    assert_eq!(parse_challenge(upper_bound).unwrap().difficulty, 5);
    let above_ceiling = r#"<script id="anubis_challenge">{"challenge":{"id":"id","randomData":"data"},"rules":{"difficulty":6}}</script>"#;
    assert_eq!(
        parse_challenge(above_ceiling).unwrap_err().code(),
        rezka_client::RezkaErrorCode::AnubisExcessiveDifficulty
    );
}

#[test]
fn unsupported_algorithm_and_excessive_difficulty_are_typed() {
    let unsupported = r#"<script id="anubis_challenge">{"challenge":{"id":"id","randomData":"data"},"rules":{"algorithm":"preact","difficulty":2}}</script>"#;
    let excessive = r#"<script id="anubis_challenge">{"challenge":{"id":"id","randomData":"data"},"rules":{"algorithm":"fast","difficulty":6}}</script>"#;
    let excessive_without_algorithm = r#"<script id="anubis_challenge">{"challenge":{"id":"id","randomData":"data"},"rules":{"difficulty":6}}</script>"#;

    assert_eq!(
        parse_challenge(unsupported).unwrap_err().code(),
        rezka_client::RezkaErrorCode::AnubisUnsupportedAlgorithm
    );
    assert_eq!(
        parse_challenge(excessive).unwrap_err().code(),
        rezka_client::RezkaErrorCode::AnubisExcessiveDifficulty
    );
    assert_eq!(
        parse_challenge(excessive_without_algorithm)
            .unwrap_err()
            .code(),
        rezka_client::RezkaErrorCode::AnubisExcessiveDifficulty
    );
}

#[tokio::test]
async fn browser_fallback_handles_known_and_future_unsupported_algorithms_before_one_retry() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    for algorithm in ["preact", "metarefresh", "future-v99"] {
        let server = MockServer::start().await;
        let base = Url::parse(&server.uri()).unwrap();
        let unsupported = format!(
            r#"<script id="anubis_challenge">{{"challenge":{{"id":"id","randomData":"data"}},"rules":{{"algorithm":"{algorithm}","difficulty":2}}}}</script>"#
        );
        Mock::given(method("GET"))
            .and(path("/title"))
            .respond_with(ResponseTemplate::new(200).set_body_string(unsupported))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/title"))
            .and(header(
                "cookie",
                "techaro.lol-anubis-auth=browser-clearance",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("provider-content"))
            .expect(1)
            .mount(&server)
            .await;

        let mut transport = Transport::new(
            MirrorSet::new(vec![base.clone()]).unwrap(),
            SessionJar::empty(),
            "media-orchestrator-test".to_owned(),
            time::Duration::seconds(10),
            0,
        )
        .unwrap()
        .with_browser_fallback(Box::new(StaticBrowserFallback));

        let response = transport
            .get_first(base.join("/title").unwrap(), None)
            .await
            .unwrap();
        assert_eq!(response.body, "provider-content");
    }
}

#[tokio::test]
async fn access_denied_challenge_is_solved_before_http_status_classification() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(ResponseTemplate::new(403).set_body_string(challenge_html("id", "data", 1)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/title")
                .insert_header(
                    "set-cookie",
                    "techaro.lol-anubis-auth=accepted; Path=/; HttpOnly",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/title"))
        .and(header("cookie", "techaro.lol-anubis-auth=accepted"))
        .respond_with(ResponseTemplate::new(200).set_body_string("provider-content"))
        .expect(1)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap();

    let response = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(response.body, "provider-content");
}

#[tokio::test]
async fn browser_fallback_handles_a_native_pass_without_clearance() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(ResponseTemplate::new(200).set_body_string(challenge_html("id", "data", 1)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/title"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/title"))
        .and(header(
            "cookie",
            "techaro.lol-anubis-auth=browser-clearance",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("provider-content"))
        .expect(1)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(StaticBrowserFallback));

    let response = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(response.body, "provider-content");
}

#[tokio::test]
async fn browser_fallback_handles_an_explicit_native_pass_rejection() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(ResponseTemplate::new(200).set_body_string(challenge_html("id", "data", 1)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/title"))
        .and(header(
            "cookie",
            "techaro.lol-anubis-auth=browser-clearance",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("provider-content"))
        .expect(1)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(StaticBrowserFallback));

    let response = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(response.body, "provider-content");
}

#[tokio::test]
async fn repeated_native_clearance_challenge_dispatches_fallback_once_then_stops() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let challenge = challenge_html("id", "data", 1);
    let original_calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(RepeatedChallengeSequence {
            calls: Arc::clone(&original_calls),
            challenge,
        })
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/title")
                .insert_header("set-cookie", "techaro.lol-anubis-auth=native; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(CountingBrowserFallback(fallback_calls.clone())));

    let response = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(response.body, "provider-content");
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    assert_eq!(original_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn clearance_cookie_is_reused_across_two_searches_and_two_download_requests() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/search-one"))
        .respond_with(ResponseTemplate::new(200).set_body_string(challenge_html("id", "data", 1)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/search-one")
                .insert_header(
                    "set-cookie",
                    "techaro.lol-anubis-auth=reused; Path=/; HttpOnly",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    for endpoint in [
        "/search-one",
        "/search-two",
        "/download-one",
        "/download-two",
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .and(header("cookie", "techaro.lol-anubis-auth=reused"))
            .respond_with(ResponseTemplate::new(200).set_body_string("provider-content"))
            .expect(1)
            .mount(&server)
            .await;
    }

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap();

    for endpoint in [
        "/search-one",
        "/search-two",
        "/download-one",
        "/download-two",
    ] {
        let response = transport
            .get_first(base.join(endpoint).unwrap(), None)
            .await
            .unwrap();
        assert_eq!(response.body, "provider-content");
    }
}

#[test]
fn challenge_field_byte_limits_are_inclusive_and_sanitized() {
    const MAX_CHALLENGE_ID_BYTES: usize = 1_024;
    const MAX_RANDOM_DATA_BYTES: usize = 4_096;

    let max_id = "i".repeat(MAX_CHALLENGE_ID_BYTES);
    let max_random_data = "r".repeat(MAX_RANDOM_DATA_BYTES);
    assert_eq!(
        parse_challenge(&challenge_html(&max_id, "random", 1))
            .unwrap()
            .id
            .len(),
        MAX_CHALLENGE_ID_BYTES
    );
    assert_eq!(
        parse_challenge(&challenge_html("id", &max_random_data, 1))
            .unwrap()
            .random_data
            .len(),
        MAX_RANDOM_DATA_BYTES
    );

    for (id, random_data, secret) in [
        (
            format!("oversized-id-secret{}", "i".repeat(MAX_CHALLENGE_ID_BYTES)),
            "random".to_owned(),
            "oversized-id-secret",
        ),
        (
            "id".to_owned(),
            format!(
                "oversized-random-secret{}",
                "r".repeat(MAX_RANDOM_DATA_BYTES)
            ),
            "oversized-random-secret",
        ),
    ] {
        let error = parse_challenge(&challenge_html(&id, &random_data, 1)).unwrap_err();
        let rendered = format!("{error:?}: {error}");

        assert_eq!(
            error.code(),
            rezka_client::RezkaErrorCode::ProviderResponseInvalid
        );
        assert!(!rendered.contains(secret));
    }
}

#[test]
fn challenge_and_proof_debug_output_are_redacted() {
    let challenge = parse_challenge(include_str!("fixtures/anubis_challenge.html")).unwrap();
    let proof = solve_challenge(&challenge, 100_000).unwrap();
    let challenge_debug = format!("{challenge:?}");
    let proof_debug = format!("{proof:?}");

    assert!(!challenge_debug.contains(&challenge.id));
    assert!(!challenge_debug.contains(&challenge.random_data));
    assert!(!proof_debug.contains(&proof.response_hex));
    assert!(!proof_debug.contains(&proof.nonce.to_string()));
}

#[tokio::test]
async fn submits_pass_challenge_with_same_user_agent_and_retains_cookie() {
    use rezka_client::{
        mirror::MirrorSet,
        session::anubis::{parse_challenge, solve_challenge, submit_challenge},
        session::cookie::SessionJar,
        transport::Transport,
    };
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path, query_param},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let title = base.join("/series/1-title.html").unwrap();
    let challenge = parse_challenge(include_str!("fixtures/anubis_challenge.html")).unwrap();
    let proof = solve_challenge(&challenge, 100_000).unwrap();

    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .and(query_param("id", "challenge-123"))
        .and(query_param("nonce", proof.nonce.to_string()))
        .and(query_param("response", proof.response_hex.clone()))
        .and(query_param("redir", title.as_str()))
        .and(query_param("elapsedTime", "42"))
        .and(header("user-agent", "media-orchestrator-test"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/must-not-be-followed")
                .insert_header("set-cookie", "anubis=opaque; Path=/"),
        )
        .mount(&server)
        .await;

    let mirrors = MirrorSet::new(vec![base.clone()]).unwrap();
    let mut transport = Transport::new(
        mirrors,
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap();

    submit_challenge(&mut transport, &challenge, &proof, title.clone(), 42)
        .await
        .unwrap();

    let snapshot = transport.export_session().unwrap();
    let restored = SessionJar::import(&snapshot).unwrap();
    assert!(restored.contains_cookie_for_url(&title, "anubis"));
}

#[tokio::test]
async fn rejects_http_200_pass_response_after_storing_its_cookie() {
    use rezka_client::{
        RezkaErrorCode,
        mirror::MirrorSet,
        session::anubis::{parse_challenge, solve_challenge, submit_challenge},
        session::cookie::SessionJar,
        transport::Transport,
    };
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let mut title = base.join("/series/1-title.html").unwrap();
    title.set_query(Some("token=redir-query-secret"));
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "anubis=from-2xx; Path=/")
                .set_body_string("raw-pass-response-secret"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let challenge = parse_challenge(include_str!("fixtures/anubis_challenge.html")).unwrap();
    let proof = solve_challenge(&challenge, 100_000).unwrap();
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap();

    let error = submit_challenge(&mut transport, &challenge, &proof, title.clone(), 42)
        .await
        .unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
    assert!(!rendered.contains("redir-query-secret"));
    assert!(!rendered.contains("raw-pass-response-secret"));
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&title, "anubis"));
}

#[tokio::test]
async fn rejects_cross_origin_redir_without_requesting_or_leaking_it() {
    use rezka_client::{
        RezkaErrorCode,
        mirror::MirrorSet,
        session::anubis::{parse_challenge, solve_challenge, submit_challenge},
        session::cookie::SessionJar,
        transport::Transport,
    };
    use url::Url;
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let redir =
        Url::parse("https://private-user:private-password@unrelated.test/title?token=query-secret")
            .unwrap();
    let challenge = parse_challenge(include_str!("fixtures/anubis_challenge.html")).unwrap();
    let proof = solve_challenge(&challenge, 100_000).unwrap();
    let mut transport = Transport::new(
        MirrorSet::new(vec![base]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap();

    let error = submit_challenge(&mut transport, &challenge, &proof, redir, 42)
        .await
        .unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
    for secret in [
        "private-user",
        "private-password",
        "unrelated.test",
        "token",
        "query-secret",
    ] {
        assert!(!rendered.contains(secret));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn submits_pass_to_currently_selected_mirror_after_failover() {
    use rezka_client::{
        mirror::MirrorSet,
        session::anubis::{parse_challenge, solve_challenge, submit_challenge},
        session::cookie::SessionJar,
        transport::Transport,
    };
    use std::net::TcpListener;
    use url::Url;
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

    let selected_server = MockServer::start().await;
    let selected = Url::parse(&selected_server.uri()).unwrap();
    let title = selected.join("/series/1-title.html").unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&selected_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .and(query_param("redir", title.as_str()))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/must-not-be-followed")
                .insert_header("set-cookie", "anubis=selected-mirror; Path=/"),
        )
        .expect(1)
        .mount(&selected_server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![primary.clone(), selected.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(2),
        1,
    )
    .unwrap();
    transport
        .get_first_with_failover(primary.join("/account/probe").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(transport.selected_origin(), &selected);

    let challenge = parse_challenge(include_str!("fixtures/anubis_challenge.html")).unwrap();
    let proof = solve_challenge(&challenge, 100_000).unwrap();
    submit_challenge(&mut transport, &challenge, &proof, title.clone(), 42)
        .await
        .unwrap();

    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&title, "anubis"));
}

struct TimeoutBrowserFallback;

impl rezka_client::session::anubis::BrowserChallengeFallback for TimeoutBrowserFallback {
    fn solve<'a>(
        &'a mut self,
        _challenge: &'a rezka_client::session::anubis::AnubisChallenge,
        _origin: &'a url::Url,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<String>, rezka_client::RezkaError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async {
            Err(rezka_client::RezkaError::AnubisTimeout {
                context: rezka_client::redaction::sanitize_provider_text("browser timed out"),
            })
        })
    }
}

struct FailedBrowserFallback;

impl rezka_client::session::anubis::BrowserChallengeFallback for FailedBrowserFallback {
    fn solve<'a>(
        &'a mut self,
        _challenge: &'a rezka_client::session::anubis::AnubisChallenge,
        _origin: &'a url::Url,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<String>, rezka_client::RezkaError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async {
            Err(rezka_client::RezkaError::ChallengeFailed {
                context: rezka_client::redaction::sanitize_provider_text("browser failed"),
            })
        })
    }
}

struct EmptyCookieBrowserFallback;

impl rezka_client::session::anubis::BrowserChallengeFallback for EmptyCookieBrowserFallback {
    fn solve<'a>(
        &'a mut self,
        _challenge: &'a rezka_client::session::anubis::AnubisChallenge,
        _origin: &'a url::Url,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<String>, rezka_client::RezkaError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
}

struct HangingBrowserFallback;

impl rezka_client::session::anubis::BrowserChallengeFallback for HangingBrowserFallback {
    fn solve<'a>(
        &'a mut self,
        _challenge: &'a rezka_client::session::anubis::AnubisChallenge,
        _origin: &'a url::Url,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<String>, rezka_client::RezkaError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async {
            std::future::pending::<()>().await;
            Ok(Vec::new())
        })
    }
}

fn unsupported_challenge(algorithm: &str) -> String {
    format!(
        r#"<script id="anubis_challenge">{{"challenge":{{"id":"id","randomData":"data"}},"rules":{{"algorithm":"{algorithm}","difficulty":2}}}}</script>"#
    )
}

#[tokio::test]
async fn native_fast_sha256_path_does_not_invoke_browser_fallback() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(ResponseTemplate::new(200).set_body_string(challenge_html("id", "data", 1)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/title")
                .insert_header(
                    "set-cookie",
                    "techaro.lol-anubis-auth=native; Path=/; HttpOnly",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/title"))
        .and(header("cookie", "techaro.lol-anubis-auth=native"))
        .respond_with(ResponseTemplate::new(200).set_body_string("provider-content"))
        .expect(1)
        .mount(&server)
        .await;
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(CountingBrowserFallback(fallback_calls.clone())));

    let response = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(response.body, "provider-content");
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn browser_timeout_is_a_typed_anubis_timeout() {
    use rezka_client::{
        RezkaErrorCode, mirror::MirrorSet, session::cookie::SessionJar, transport::Transport,
    };
    use url::Url;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path};

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(ResponseTemplate::new(200).set_body_string(unsupported_challenge("preact")))
        .expect(1)
        .mount(&server)
        .await;
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(TimeoutBrowserFallback));

    let error = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::AnubisTimeout);
}

#[tokio::test]
async fn browser_process_failure_is_a_typed_challenge_failure() {
    use rezka_client::{
        RezkaErrorCode, mirror::MirrorSet, session::cookie::SessionJar, transport::Transport,
    };
    use url::Url;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path};

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(unsupported_challenge("metarefresh")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(FailedBrowserFallback));

    let error = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), RezkaErrorCode::ChallengeFailed);
}

#[tokio::test]
async fn browser_without_clearance_is_a_typed_rejection() {
    use rezka_client::{
        RezkaErrorCode, mirror::MirrorSet, session::cookie::SessionJar, transport::Transport,
    };
    use url::Url;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path};

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(unsupported_challenge("future-v99")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(EmptyCookieBrowserFallback));

    let error = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap_err();
    let rendered = format!("{error:?}: {error}");
    assert_eq!(error.code(), RezkaErrorCode::AnubisRejected);
    assert!(!rendered.contains("anubis_challenge"));
    assert!(!rendered.contains("future-v99"));
}

#[tokio::test]
async fn dropping_a_hanging_browser_fallback_cancels_promptly() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path};

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(ResponseTemplate::new(200).set_body_string(unsupported_challenge("preact")))
        .expect(1)
        .mount(&server)
        .await;
    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(HangingBrowserFallback));

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(150),
        transport.get_first(base.join("/title").unwrap(), None),
    )
    .await;
    assert!(result.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[tokio::test]
async fn browser_fallback_preserves_unrelated_anonymous_cookies() {
    use rezka_client::{mirror::MirrorSet, session::cookie::SessionJar, transport::Transport};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "provider_state=kept; Path=/; HttpOnly")
                .set_body_string(unsupported_challenge("preact")),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/title"))
        .respond_with(ResponseTemplate::new(200).set_body_string("provider-content"))
        .expect(1)
        .mount(&server)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap()
    .with_browser_fallback(Box::new(StaticBrowserFallback));

    let response = transport
        .get_first(base.join("/title").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(response.body, "provider-content");
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "provider_state"));
    assert!(restored.contains_cookie_for_url(&base, "techaro.lol-anubis-auth"));
}
