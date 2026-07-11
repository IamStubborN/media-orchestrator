use std::{
    io::Read as _,
    path::{Path, PathBuf},
};

const MAX_LIVE_USERNAME_BYTES: usize = 256;
const MAX_LIVE_PASSWORD_BYTES: usize = 1024;

#[tokio::test]
#[ignore = "requires REZKA_LIVE_PROBE=1 and real Rezka secret files; never run in normal CI"]
async fn live_probe_rezka_session_authentication_contract() {
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
    let username_file =
        std::env::var_os("REZKA_LIVE_USERNAME_FILE").expect("REZKA_LIVE_USERNAME_FILE is required");
    let password_file =
        std::env::var_os("REZKA_LIVE_PASSWORD_FILE").expect("REZKA_LIVE_PASSWORD_FILE is required");

    let username = read_live_secret(Path::new(&username_file), LiveSecretKind::Username)
        .expect("username secret file is unreadable or invalid");
    let password = read_live_secret(Path::new(&password_file), LiveSecretKind::Password)
        .expect("password secret file is unreadable or invalid");

    let config = rezka_client::session::RezkaClientConfig {
        mirrors: rezka_client::MirrorSet::new(vec![mirror]).unwrap(),
        user_agent: "media-orchestrator-live-probe".to_owned(),
        request_timeout: time::Duration::seconds(30),
        max_retries: 1,
        anubis_max_nonce: 5_000_000,
    };
    let probe = rezka_client::session::SessionValidationProbe::new(
        probe_url,
        valid_markers,
        invalid_markers,
    )
    .unwrap();
    let credentials = rezka_client::session::RezkaCredentials {
        username: secrecy::SecretString::from(username),
        password: secrecy::SecretString::from(password),
    };

    let mut client = rezka_client::session::RezkaClient::new(config).unwrap();
    let validation = client
        .ensure_authenticated(&credentials, &probe)
        .await
        .unwrap();

    assert_eq!(validation, rezka_client::session::SessionValidation::Valid);
}

#[test]
fn credentialed_live_probe_accepts_only_https_urls() {
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

#[test]
fn live_secret_reader_strips_one_final_line_ending_with_bounded_sizes() {
    let username_path = live_secret_fixture("username", &[b'u'; MAX_LIVE_USERNAME_BYTES], b"\n");
    let password_path = live_secret_fixture("password", &[b'p'; MAX_LIVE_PASSWORD_BYTES], b"\r\n");

    let username = read_live_secret(&username_path, LiveSecretKind::Username).unwrap();
    let password = read_live_secret(&password_path, LiveSecretKind::Password).unwrap();

    assert_eq!(username.len(), MAX_LIVE_USERNAME_BYTES);
    assert_eq!(password.len(), MAX_LIVE_PASSWORD_BYTES);
    std::fs::remove_file(username_path).unwrap();
    std::fs::remove_file(password_path).unwrap();
}

#[test]
fn live_secret_reader_rejects_oversized_or_invalid_data_without_leaking_details() {
    let path = live_secret_fixture("forbidden-path", &[b'x'; MAX_LIVE_USERNAME_BYTES + 1], b"");

    let error = read_live_secret(&path, LiveSecretKind::Username).unwrap_err();
    let debug = format!("{error:?}");

    assert!(!debug.contains("forbidden-path"));
    assert!(!debug.contains(&"x".repeat(MAX_LIVE_USERNAME_BYTES + 1)));
    std::fs::remove_file(path).unwrap();
}

fn live_secret_fixture(name: &str, contents: &[u8], suffix: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "media-orchestrator-live-probe-{name}-{}",
        std::process::id()
    ));
    let mut bytes = contents.to_vec();
    bytes.extend_from_slice(suffix);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn read_live_secret(path: &Path, kind: LiveSecretKind) -> Result<String, LiveSecretError> {
    let max_bytes = kind.max_bytes();
    let file = std::fs::File::open(path).map_err(|_| LiveSecretError::Unreadable)?;
    let mut contents = Vec::with_capacity(max_bytes + 2);
    file.take((max_bytes + 3) as u64)
        .read_to_end(&mut contents)
        .map_err(|_| LiveSecretError::Unreadable)?;
    if contents.len() > max_bytes + 2 {
        return Err(LiveSecretError::Invalid);
    }
    strip_one_final_line_ending(&mut contents);

    let valid = match kind {
        LiveSecretKind::Username => {
            !contents.is_empty()
                && contents.len() <= max_bytes
                && contents.iter().all(|byte| (0x21..=0x7e).contains(byte))
        }
        LiveSecretKind::Password => {
            !contents.is_empty() && contents.len() <= max_bytes && !contents.contains(&0)
        }
    };
    if !valid {
        return Err(LiveSecretError::Invalid);
    }
    String::from_utf8(contents).map_err(|_| LiveSecretError::Invalid)
}

fn strip_one_final_line_ending(contents: &mut Vec<u8>) {
    if contents.ends_with(b"\r\n") {
        contents.truncate(contents.len() - 2);
    } else if contents.ends_with(b"\n") {
        contents.pop();
    }
}

#[derive(Debug, Copy, Clone)]
enum LiveSecretKind {
    Username,
    Password,
}

impl LiveSecretKind {
    const fn max_bytes(self) -> usize {
        match self {
            Self::Username => MAX_LIVE_USERNAME_BYTES,
            Self::Password => MAX_LIVE_PASSWORD_BYTES,
        }
    }
}

#[derive(Debug)]
enum LiveSecretError {
    Unreadable,
    Invalid,
}

fn parse_live_https_url(value: &str) -> Option<url::Url> {
    url::Url::parse(value)
        .ok()
        .filter(|url| url.scheme() == "https")
}
