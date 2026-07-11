use rezka_client::{
    error::{RezkaError, RezkaErrorCode},
    redaction::{redact_url, sanitize_provider_text},
};

#[test]
fn redacts_url_credentials_queries_and_ip_literals() {
    let url = redact_url("https://cdn.example/video.mp4?md5=secret-token&expires=123");
    assert_eq!(url.as_ref(), "https://cdn.example/video.mp4?[REDACTED]");

    let credentialed = redact_url("https://alice:hunter2@cdn.example/file");
    assert!(!credentialed.as_ref().contains("alice"));
    assert!(!credentialed.as_ref().contains("hunter2"));

    for literal in [
        "https://203.0.113.9/a?token=x",
        "https://[2001:db8::7]/a?token=x",
    ] {
        let rendered = redact_url(literal).to_string();
        assert!(!rendered.contains("203.0.113.9"));
        assert!(!rendered.contains("2001:db8::7"));
        assert!(!rendered.contains("token=x"));
    }
}

#[test]
fn sanitizes_headers_assignments_json_inline_urls_ips_and_raw_provider_text() {
    let samples = [
        "Cookie: PHPSESSID=header-secret",
        "Set-Cookie = dle_password=assignment-secret",
        r#"{\"Authorization\":\"Bearer json-secret\"}"#,
        r#"{\"password\":\"hunter2\",\"access_token\":\"token-secret\"}"#,
        "request failed at https://cdn.example/file?sig=query-secret",
        "upstream 203.0.113.9 and [2001:db8::7] refused",
        "unstructured raw provider body with unique-secret-fragment",
    ];

    for sample in samples {
        let rendered = sanitize_provider_text(sample).to_string();
        assert_eq!(rendered, "[REDACTED_PROVIDER_TEXT]");
        assert!(!rendered.contains(sample));
    }
}

#[test]
fn error_display_and_debug_never_include_provider_secret_material() {
    let context = sanitize_provider_text("raw provider snippet sig=abc login_password=hunter2");
    let error = RezkaError::ProviderResponseInvalid { context };
    let rendered = format!("{error:?}: {error}");

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
    assert!(rendered.contains("provider response invalid"));
    for forbidden in ["sig=abc", "hunter2", "PHPSESSID", "secret"] {
        assert!(!rendered.contains(forbidden), "leaked {forbidden}");
    }
}
