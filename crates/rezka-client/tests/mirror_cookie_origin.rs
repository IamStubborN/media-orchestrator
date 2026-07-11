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
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("session_cookie"));
    assert!(!debug.contains("opaque-a"));

    let restored = SessionJar::import(&snapshot).unwrap();
    assert!(restored.contains_cookie_for_url(&origin, "session_cookie"));
    assert!(restored.contains_cookie_for_url(&origin, "persistent_cookie"));
    assert!(!restored.contains_cookie_for_url(&origin, "expired_cookie"));
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
