use rezka_client::{
    SessionSnapshot,
    error::{RezkaError, RezkaErrorCode},
    redaction::{redact_url, sanitize_provider_text},
    session::{RezkaCredentials, SessionValidationProbe},
};
use secrecy::{SecretBox, SecretString};
use url::Url;

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

#[test]
fn constructed_security_types_never_leak_debug_material() {
    let credentials = RezkaCredentials {
        username: SecretString::from("rezka-user"),
        password: SecretString::from("rezka-password"),
    };
    let probe = SessionValidationProbe::new(
        Url::parse("https://rezka.invalid/account/probe").unwrap(),
        vec!["opaque-a".to_owned()],
        vec!["opaque-b".to_owned()],
    )
    .unwrap();
    let snapshot =
        SessionSnapshot::from_secret_bytes(SecretBox::new(Box::new(b"opaque-a:opaque-b".to_vec())));
    let debug = format!("{credentials:?} {probe:?} {snapshot:?}");

    for forbidden in ["rezka-user", "rezka-password", "opaque-a", "opaque-b"] {
        assert!(!debug.contains(forbidden), "Debug leaked {forbidden}");
    }
}
