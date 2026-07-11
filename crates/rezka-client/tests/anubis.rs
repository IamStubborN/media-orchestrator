use rezka_client::session::anubis::{detect_challenge, parse_challenge, solve_challenge};

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
        r#"{"challenge":{"id":"secret-id","randomData":"secret-random-data"},"rules":{"difficulty":33}}"#,
    ] {
        let html = format!(r#"<script id="anubis_challenge">{raw_json}</script>"#);
        let error = parse_challenge(&html).unwrap_err();
        let rendered = format!("{error:?}: {error}");

        assert!(!rendered.contains(raw_json));
        assert!(!rendered.contains("secret-id"));
        assert!(!rendered.contains("secret-random-data"));
    }

    let upper_bound = r#"<script id="anubis_challenge">{"challenge":{"id":"id","randomData":"data"},"rules":{"difficulty":32}}</script>"#;
    assert_eq!(parse_challenge(upper_bound).unwrap().difficulty, 32);
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
