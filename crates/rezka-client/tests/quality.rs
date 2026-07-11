use base64::{Engine as _, engine::general_purpose::STANDARD};
use rezka_client::{QualityTier, RezkaErrorCode, StreamKind, parse_stream_variants};

fn endpoint_url(endpoint: &rezka_client::StreamEndpoint) -> String {
    endpoint.url().with_url(|url| url.as_str().to_owned())
}

fn obfuscate(plain: &str, salt: &str) -> String {
    let encoded = STANDARD.encode(plain);
    let midpoint = encoded.len() / 2;
    format!(
        "#h{}//_//{salt}{}",
        &encoded[..midpoint],
        &encoded[midpoint..]
    )
}

fn assert_invalid(payload: &str) {
    assert_eq!(
        parse_stream_variants(payload).unwrap_err().code(),
        RezkaErrorCode::ProviderResponseInvalid
    );
}

#[test]
fn plain_and_known_or_fixed_salt_obfuscation_are_equivalent() {
    let plain = include_str!("fixtures/stream_plain.txt").trim();
    let expected = parse_stream_variants(plain).unwrap();
    let fixture =
        parse_stream_variants(include_str!("fixtures/stream_obfuscated.txt").trim()).unwrap();
    assert_eq!(expected.len(), fixture.len());

    for known in ["@#", "!^$", "$$", "#@!"] {
        let salt = STANDARD.encode(known);
        assert_eq!(
            parse_stream_variants(&obfuscate(plain, &salt))
                .unwrap()
                .len(),
            2
        );
    }
    assert_eq!(
        parse_stream_variants(&obfuscate(plain, "0123456789abcdef"))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn decoder_enforces_marker_and_decoded_size_budgets() {
    let listing = "[360p]https://cdn.example.com/video.mp4";
    let encoded = STANDARD.encode(listing);
    let with_markers = |count: usize| {
        let mut payload = format!("#h{encoded}");
        for _ in 0..count {
            payload.push_str("//_//QEAh");
        }
        payload
    };
    assert!(parse_stream_variants(&with_markers(60)).is_ok());
    assert_invalid(&with_markers(61));
    assert_invalid("#hnot-base64***");
    assert_invalid(&format!(
        "[360p]https://cdn.example.com/{}.mp4",
        "x".repeat(1024 * 1024)
    ));
    assert_invalid("[360p]https://cdn.example.com/video.mp4 trailing");
}

#[test]
fn endpoint_classification_is_strict_ordered_and_duplicate_aware() {
    let modern = parse_stream_variants(
        "[720p]https://cdn.example.com/a.mp4:hls:manifest.m3u8 or https://cdn.example.com/a.mp4 or https://cdn.example.com/a.mp4",
    )
    .unwrap();
    assert_eq!(modern[0].endpoints().len(), 2);
    assert_eq!(modern[0].endpoints()[0].kind(), StreamKind::Hls);
    assert_eq!(modern[0].endpoints()[1].kind(), StreamKind::Mp4);

    let legacy = parse_stream_variants(
        "[480p]https://cdn.example.com/legacy-hls.mp4 or https://cdn.example.com/legacy.mp4",
    )
    .unwrap();
    assert_eq!(legacy[0].endpoints()[0].kind(), StreamKind::Hls);
    assert_eq!(legacy[0].endpoints()[1].kind(), StreamKind::Mp4);

    let ordinary = parse_stream_variants(
        "[360p]https://cdn.example.com/video.m3u8 or https://cdn.example.com/video.mp4",
    )
    .unwrap();
    assert_eq!(
        endpoint_url(&ordinary[0].endpoints()[0]),
        "https://cdn.example.com/video.m3u8"
    );

    for invalid in [
        "[720p]http://cdn.example.com/video.mp4",
        "[720p]https://127.0.0.1/video.mp4",
        "[720p]https://cdn.example.com/video.bin",
        "[720p]https://cdn.example.com/a.mp4 or bad-url",
        "[720p]https://cdn.example.com/a.mp4 or https://cdn.example.com/b.mp4 or https://cdn.example.com/c.mp4 or https://cdn.example.com/d.mp4 or https://cdn.example.com/e.mp4",
    ] {
        assert_invalid(invalid);
    }
}

#[test]
fn quality_normalization_merges_identity_and_ranks_highest_first() {
    let variants = parse_stream_variants(
        "[720p]https://cdn.example.com/a.mp4,[<span class='premium'>1080p Ultra</span>]https://cdn.example.com/u.mp4,[1080p]https://cdn.example.com/b.mp4,[720p]https://cdn.example.com/c.mp4",
    )
    .unwrap();
    assert_eq!(variants.len(), 3);
    assert_eq!(variants[0].advertised_quality().label(), "1080p Ultra");
    assert_eq!(variants[0].advertised_quality().vertical_hint(), Some(1080));
    assert_eq!(
        variants[0].advertised_quality().tier(),
        QualityTier::Premium
    );
    assert_eq!(variants[1].advertised_quality().label(), "1080p");
    assert_eq!(variants[2].endpoints().len(), 2);
}

#[test]
fn parser_enforces_variant_budget_and_redacts_debug() {
    let boundary = (0..32)
        .map(|index| format!("[{}p]https://cdn.example.com/{index}.mp4", index + 1))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(parse_stream_variants(&boundary).unwrap().len(), 32);
    assert_invalid(&format!(
        "{boundary},[99p]https://cdn.example.com/extra.mp4"
    ));

    let variant = parse_stream_variants("[720p]https://secret.example.com/path.mp4?token=opaque")
        .unwrap()
        .remove(0);
    let debug = format!("{variant:?}");
    for forbidden in ["secret", "example", "path", "token", "opaque", "https"] {
        assert!(
            !debug.contains(forbidden),
            "Debug leaked {forbidden}: {debug}"
        );
    }
}
