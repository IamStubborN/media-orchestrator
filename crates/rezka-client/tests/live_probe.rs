#[tokio::test]
#[ignore = "requires REZKA_LIVE_PROBE=1; never run in normal CI"]
async fn live_probe_rezka_session_contract() {
    let opt_in =
        std::env::var("REZKA_LIVE_PROBE").expect("explicit live probe requires REZKA_LIVE_PROBE=1");
    assert_eq!(
        opt_in, "1",
        "explicit live probe requires REZKA_LIVE_PROBE=1"
    );

    let mirror = parse_live_https_url(
        &std::env::var("REZKA_LIVE_MIRROR").expect("REZKA_LIVE_MIRROR is required"),
    )
    .expect("REZKA_LIVE_MIRROR must be HTTPS");
    let probe_url = parse_live_https_url(
        &std::env::var("REZKA_LIVE_SESSION_PROBE_URL")
            .expect("REZKA_LIVE_SESSION_PROBE_URL is required"),
    )
    .expect("REZKA_LIVE_SESSION_PROBE_URL must be HTTPS");
    let valid_markers: Vec<String> = serde_json::from_str(
        &std::env::var("REZKA_LIVE_SESSION_VALID_MARKERS_JSON")
            .expect("REZKA_LIVE_SESSION_VALID_MARKERS_JSON is required"),
    )
    .expect("REZKA_LIVE_SESSION_VALID_MARKERS_JSON must be a JSON string array");
    let invalid_markers: Vec<String> = serde_json::from_str(
        &std::env::var("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON")
            .expect("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON is required"),
    )
    .expect("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON must be a JSON string array");
    let config = rezka_client::session::RezkaClientConfig {
        mirrors: rezka_client::MirrorSet::new(vec![mirror]).unwrap(),
        user_agent: "media-orchestrator-live-probe".to_owned(),
        request_timeout: time::Duration::seconds(30),
        max_retries: 1,
        anubis_max_nonce: 5_000_000,
        proxy_url: None,
    };
    let probe = rezka_client::session::SessionValidationProbe::new(
        probe_url,
        valid_markers,
        invalid_markers,
    )
    .unwrap();

    let mut client = rezka_client::session::RezkaClient::new(config).unwrap();
    let validation = client.ensure_session(&probe, "").await.unwrap();

    assert!(
        matches!(
            validation,
            rezka_client::session::SessionValidation::Valid
                | rezka_client::session::SessionValidation::Invalid
        ),
        "anonymous session must classify as valid or invalid, got {validation:?}"
    );
}

#[tokio::test]
#[ignore = "requires REZKA_LIVE_PROBE=1; never run in normal CI"]
async fn explicit_catalog_playback_live_probe() {
    // Negative guard: without the explicit opt-in this panics here, before any
    // DNS resolution, catalog query construction, or network access.
    let opt_in =
        std::env::var("REZKA_LIVE_PROBE").expect("explicit live probe requires REZKA_LIVE_PROBE=1");
    assert_eq!(
        opt_in, "1",
        "explicit live probe requires REZKA_LIVE_PROBE=1"
    );

    let mirror = parse_live_https_url(
        &std::env::var("REZKA_LIVE_MIRROR").expect("REZKA_LIVE_MIRROR is required"),
    )
    .expect("REZKA_LIVE_MIRROR must be HTTPS");
    let probe_url = parse_live_https_url(
        &std::env::var("REZKA_LIVE_SESSION_PROBE_URL")
            .expect("REZKA_LIVE_SESSION_PROBE_URL is required"),
    )
    .expect("REZKA_LIVE_SESSION_PROBE_URL must be HTTPS");
    let valid_markers: Vec<String> = serde_json::from_str(
        &std::env::var("REZKA_LIVE_SESSION_VALID_MARKERS_JSON")
            .expect("REZKA_LIVE_SESSION_VALID_MARKERS_JSON is required"),
    )
    .expect("REZKA_LIVE_SESSION_VALID_MARKERS_JSON must be a JSON string array");
    let invalid_markers: Vec<String> = serde_json::from_str(
        &std::env::var("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON")
            .expect("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON is required"),
    )
    .expect("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON must be a JSON string array");

    // Caller-supplied query and title, validated by the same bounded value
    // constructors that normal callers use, before any request is made.
    let query = rezka_client::CatalogQuery::new(
        &std::env::var("REZKA_LIVE_SEARCH_QUERY").expect("REZKA_LIVE_SEARCH_QUERY is required"),
    )
    .expect("REZKA_LIVE_SEARCH_QUERY must be a valid catalog query");
    let locator = rezka_client::TitleLocator::new(
        &std::env::var("REZKA_LIVE_TITLE_LOCATOR").expect("REZKA_LIVE_TITLE_LOCATOR is required"),
    )
    .expect("REZKA_LIVE_TITLE_LOCATOR must be a valid title locator");

    let config = rezka_client::session::RezkaClientConfig {
        mirrors: rezka_client::MirrorSet::new(vec![mirror]).unwrap(),
        user_agent: "media-orchestrator-live-probe".to_owned(),
        request_timeout: time::Duration::seconds(30),
        max_retries: 1,
        anubis_max_nonce: 5_000_000,
        proxy_url: None,
    };
    let probe = rezka_client::session::SessionValidationProbe::new(
        probe_url,
        valid_markers,
        invalid_markers,
    )
    .unwrap();

    let mut client = rezka_client::session::RezkaClient::new(config).unwrap();
    let validation = client.ensure_session(&probe, "").await.unwrap();
    assert!(
        matches!(
            validation,
            rezka_client::session::SessionValidation::Valid
                | rezka_client::session::SessionValidation::Invalid
        ),
        "anonymous session must classify as valid or invalid, got {validation:?}"
    );

    // Catalog search: assert structural invariants only, never content.
    let page = client
        .search(&query)
        .await
        .expect("catalog search must succeed");
    assert!(
        !page.entries().is_empty(),
        "catalog search returned no entries"
    );
    for entry in page.entries() {
        assert!(
            !entry.title().trim().is_empty(),
            "catalog entry title is blank"
        );
        let locator = entry.locator().as_str();
        assert!(
            locator.starts_with('/') && locator.ends_with(".html"),
            "catalog locator is not an absolute title page path"
        );
    }

    // Title: translations must be discoverable.
    let title = client
        .title(&locator)
        .await
        .expect("title fetch must succeed");
    assert!(!title.title().trim().is_empty(), "title name is blank");
    assert!(
        !title.translations().is_empty(),
        "title exposes no translations"
    );
    let translation_key = title
        .default_translation()
        .copied()
        .unwrap_or_else(|| *title.translations()[0].key());
    let selection = title
        .select_translation(&translation_key)
        .expect("selected translation must belong to the title");

    // Playback request depends on the media kind.
    let request = match title.kind() {
        rezka_client::RezkaMediaKind::Movie => selection
            .movie_request()
            .expect("movie selection must yield a movie request"),
        rezka_client::RezkaMediaKind::Series => {
            let availability = client
                .series_availability(&selection)
                .await
                .expect("series availability must resolve");
            assert!(
                !availability.seasons().is_empty(),
                "series exposes no seasons"
            );
            let mut previous = 0;
            for season in availability.seasons() {
                assert!(
                    season.number() > previous,
                    "seasons are not strictly ascending"
                );
                previous = season.number();
                assert!(!season.episodes().is_empty(), "season exposes no episodes");
                let mut previous_episode = 0;
                for episode in season.episodes() {
                    assert!(
                        episode.number() > previous_episode,
                        "episodes are not strictly ascending"
                    );
                    previous_episode = episode.number();
                }
            }
            let first_season = &availability.seasons()[0];
            let first_episode = &first_season.episodes()[0];
            availability
                .select_episode(first_season.number(), first_episode.number())
                .expect("first advertised episode must be selectable")
                .playback_request()
        }
    };

    // Playback manifest resolution: assert structural invariants only.
    let manifest = client
        .resolve(request)
        .await
        .expect("playback manifest must resolve");
    assert!(
        !manifest.variants().is_empty(),
        "manifest exposes no stream variants"
    );
    assert!(
        manifest.preferred_variant_index() < manifest.variants().len(),
        "preferred variant index is out of bounds"
    );
    for variant in manifest.variants() {
        assert!(
            !variant.endpoints().is_empty(),
            "stream variant exposes no endpoints"
        );
        for endpoint in variant.endpoints() {
            endpoint.url().with_url(|url| {
                assert_eq!(url.scheme(), "https", "stream endpoint is not HTTPS");
            });
        }
    }
    for track in manifest.subtitles() {
        assert!(
            !track.alternatives().is_empty(),
            "subtitle track exposes no URLs"
        );
        for alternative in track.alternatives() {
            alternative.with_url(|url| {
                assert_eq!(url.scheme(), "https", "subtitle URL is not HTTPS");
            });
        }
    }

    // Redacted summary: counts, ids, kinds, and language codes only.
    let languages: Vec<&str> = manifest
        .subtitles()
        .iter()
        .filter_map(|track| track.language().map(|language| language.as_str()))
        .collect();
    eprintln!(
        "live probe ok: entries={} title_id={} kind={:?} translations={} target={:?} \
         variants={} preferred={} subtitles={} languages={:?}",
        page.entries().len(),
        title.id().get(),
        title.kind(),
        title.translations().len(),
        manifest.target(),
        manifest.variants().len(),
        manifest.preferred_variant_index(),
        manifest.subtitles().len(),
        languages,
    );
}

// Architecture guard (Phase 4): the domain playback manifest and the secret CDN
// and subtitle URL wrappers must never gain a Serde implementation, so signed
// ephemeral URLs cannot be serialized into logs, wire payloads, or persisted
// state. `serde` is a normal dependency of `rezka-client`, so its traits are
// nameable in this integration test; the `media` crate does not depend on serde
// directly, so this type-level assertion lives here while
// `crates/media/tests/architecture.rs` guards the complementary crate-boundary
// invariant (provider types never reach the serializable domain/contract crates)
// via cargo metadata. The check uses inherent-vs-trait method resolution: the
// inherent `impl` method exists only when the bound (`Serialize` /
// `DeserializeOwned`) is satisfied and then shadows the always-present trait
// fallback, so the returned boolean observes the presence of a real impl at run
// time without needing the type to be constructible.
#[test]
fn playback_manifest_and_secret_url_types_have_no_serde_impls() {
    use std::marker::PhantomData;

    struct Probe<T>(PhantomData<T>);

    trait NotSerialize {
        fn is_serialize(&self) -> bool {
            false
        }
    }
    impl<T> NotSerialize for Probe<T> {}
    impl<T: serde::Serialize> Probe<T> {
        fn is_serialize(&self) -> bool {
            true
        }
    }

    trait NotDeserialize {
        fn is_deserialize(&self) -> bool {
            false
        }
    }
    impl<T> NotDeserialize for Probe<T> {}
    impl<T: serde::de::DeserializeOwned> Probe<T> {
        fn is_deserialize(&self) -> bool {
            true
        }
    }

    macro_rules! assert_no_serde {
        ($ty:ty) => {{
            assert!(
                !Probe::<$ty>(PhantomData).is_serialize(),
                concat!(stringify!($ty), " must not implement serde::Serialize"),
            );
            assert!(
                !Probe::<$ty>(PhantomData).is_deserialize(),
                concat!(stringify!($ty), " must not implement serde::Deserialize"),
            );
        }};
    }

    assert_no_serde!(rezka_client::PlaybackManifest);
    assert_no_serde!(rezka_client::SecretMediaUrl);
    assert_no_serde!(rezka_client::SecretSubtitleUrl);

    // Sanity anchor: the probe reports `true` for a type that does implement
    // Serde, proving the detector is not vacuously negative.
    assert!(Probe::<String>(PhantomData).is_serialize());
    assert!(Probe::<String>(PhantomData).is_deserialize());
}

#[test]
fn live_probe_accepts_only_https_urls() {
    assert!(parse_live_https_url("https://rezka.test/account/probe").is_some());
    for rejected in [
        "http://rezka.test/account/probe",
        "http://127.0.0.1/account/probe",
        "not-a-url",
    ] {
        assert!(parse_live_https_url(rejected).is_none());
    }
}

#[tokio::test]
async fn live_equivalent_client_path_rejects_probe_outside_configured_mirrors() {
    let configured_mirror = url::Url::parse("https://configured-rezka.invalid").unwrap();
    let unconfigured_probe =
        url::Url::parse("https://unconfigured-rezka.invalid/account/probe").unwrap();
    let config = rezka_client::session::RezkaClientConfig {
        mirrors: rezka_client::MirrorSet::new(vec![configured_mirror]).unwrap(),
        user_agent: "media-orchestrator-live-probe-test".to_owned(),
        request_timeout: time::Duration::seconds(1),
        max_retries: 0,
        anubis_max_nonce: 1,
        proxy_url: None,
    };
    let probe = rezka_client::session::SessionValidationProbe::new(
        unconfigured_probe,
        vec!["deployment-valid-marker".to_owned()],
        vec!["deployment-invalid-marker".to_owned()],
    )
    .unwrap();
    let mut client = rezka_client::session::RezkaClient::new(config).unwrap();

    let error = client.fetch_probe(&probe).await.unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::Configuration);
    assert_eq!(
        error.to_string(),
        "configuration invalid: probe origin is not a configured Rezka mirror"
    );
}

fn parse_live_https_url(value: &str) -> Option<url::Url> {
    url::Url::parse(value)
        .ok()
        .filter(|url| url.scheme() == "https")
}
