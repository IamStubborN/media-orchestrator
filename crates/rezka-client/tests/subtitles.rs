use rezka_client::{RezkaErrorCode, parse_subtitle_fields};

fn assert_invalid(wrapper: &str) {
    assert_eq!(
        parse_subtitle_fields(wrapper).unwrap_err().code(),
        RezkaErrorCode::ProviderResponseInvalid
    );
}

#[test]
fn empty_provider_forms_produce_no_tracks() {
    for subtitle in ["false", "null", "\"\""] {
        for languages in ["false", "null", "\"\"", "{}"] {
            let wrapper = format!(r#"{{"subtitle":{subtitle},"subtitle_lns":{languages}}}"#);
            assert!(parse_subtitle_fields(&wrapper).unwrap().is_empty());
        }
    }
    for wrapper in [
        r#"{"subtitle":[],"subtitle_lns":{}}"#,
        r#"{"subtitle":"[English]https://sub.example.com/a.vtt","subtitle_lns":[]}"#,
        r#"{"subtitle":{},"subtitle_lns":{}}"#,
    ] {
        assert_invalid(wrapper);
    }
}

#[test]
fn duplicate_labels_and_languages_keep_distinct_ordinal_identity() {
    let tracks = parse_subtitle_fields(
        r#"{"subtitle":"[English]https://sub.example.com/a.vtt or https://sub.example.com/a.vtt or https://sub.example.com/b.vtt,[English]https://sub.example.com/c.vtt,[Signs]https://sub.example.com/d.vtt","subtitle_lns":{"English":" EN_us ","Signs":"ua","Unused":"fr"}}"#,
    )
    .unwrap();
    assert_eq!(tracks.len(), 3);
    assert_eq!(tracks[0].id().provider_label(), "English");
    assert_eq!(tracks[0].id().ordinal(), 0);
    assert_eq!(tracks[1].id().ordinal(), 1);
    assert_eq!(tracks[0].language().unwrap().as_str(), "en-us");
    assert_eq!(tracks[1].language().unwrap().as_str(), "en-us");
    assert_eq!(tracks[2].language().unwrap().as_str(), "ua");
    assert_eq!(tracks[0].alternatives().len(), 2);
}

#[test]
fn track_with_an_insecure_alternative_is_skipped_while_valid_tracks_survive() {
    let tracks = parse_subtitle_fields(
        r#"{"subtitle":"[English]https://sub.example.com/a.vtt or http://sub.example.com/b.vtt,[Spanish]https://sub.example.com/c.vtt","subtitle_lns":{}}"#,
    )
    .unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].id().provider_label(), "Spanish");
    assert_eq!(tracks[0].id().ordinal(), 0);

    // When every track carries an invalid URL, the whole listing degrades to no tracks.
    assert!(
        parse_subtitle_fields(
            r#"{"subtitle":"[English]http://sub.example.com/b.vtt","subtitle_lns":{}}"#,
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn malformed_alternatives_languages_and_duplicate_json_keys_fail_atomically() {
    for wrapper in [
        r#"{"subtitle":"[English]https://sub.example.com/a.vtt","subtitle_lns":{"English":"-en"}}"#,
        r#"{"subtitle":"[English]https://sub.example.com/a.vtt","subtitle_lns":{"English":"en","English":"fr"}}"#,
        r#"{"subtitle":"","subtitle":"","subtitle_lns":{}}"#,
        r#"{"subtitle":"[English]https://sub.example.com/a.vtt","subtitle_lns":{"English":"abcdefghijklmnopqrstuvwxyzabcdefghij"}}"#,
    ] {
        assert_invalid(wrapper);
    }
}

#[test]
fn subtitle_track_and_alternative_budgets_are_exact() {
    let alternatives = (0..4)
        .map(|index| format!("https://sub.example.com/{index}.vtt"))
        .collect::<Vec<_>>()
        .join(" or ");
    let wrapper =
        serde_json::json!({"subtitle": format!("[English]{alternatives}"), "subtitle_lns": {}})
            .to_string();
    assert_eq!(
        parse_subtitle_fields(&wrapper).unwrap()[0]
            .alternatives()
            .len(),
        4
    );
    let over = format!("{alternatives} or https://sub.example.com/4.vtt");
    let wrapper =
        serde_json::json!({"subtitle": format!("[English]{over}"), "subtitle_lns": {}}).to_string();
    assert_invalid(&wrapper);

    let listing = (0..64)
        .map(|index| format!("[Track {index}]https://sub.example.com/{index}.vtt"))
        .collect::<Vec<_>>()
        .join(",");
    let wrapper = serde_json::json!({"subtitle": listing, "subtitle_lns": {}}).to_string();
    assert_eq!(parse_subtitle_fields(&wrapper).unwrap().len(), 64);
    let listing = format!("{listing},[Extra]https://sub.example.com/extra.vtt");
    let wrapper = serde_json::json!({"subtitle": listing, "subtitle_lns": {}}).to_string();
    assert_invalid(&wrapper);
}

#[test]
fn debug_is_redacted_and_values_are_read_only() {
    let track = parse_subtitle_fields(
        r#"{"subtitle":"[English]https://secret.example.com/sub.vtt?token=opaque","subtitle_lns":{"English":"en"}}"#,
    )
    .unwrap()
    .remove(0);
    let debug = format!("{track:?}");
    for forbidden in ["secret", "example", "sub.vtt", "token", "opaque", "https"] {
        assert!(
            !debug.contains(forbidden),
            "Debug leaked {forbidden}: {debug}"
        );
    }
}
