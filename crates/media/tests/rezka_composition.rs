use std::{
    collections::HashMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use media::config::{ConfigSource, RunnerConfig};

#[derive(Default)]
struct FakeSource {
    env: HashMap<&'static str, OsString>,
    files: HashMap<PathBuf, Vec<u8>>,
}

impl FakeSource {
    fn set_env(&mut self, name: &'static str, value: impl Into<OsString>) {
        self.env.insert(name, value.into());
    }

    fn set_secret(&mut self, name: &'static str, value: &[u8]) {
        let path = PathBuf::from(format!("/{name}.secret"));
        self.set_env(name, path.as_os_str());
        self.files.insert(path, value.to_vec());
    }
}

impl ConfigSource for FakeSource {
    fn var_os(&self, name: &'static str) -> Option<OsString> {
        self.env.get(name).cloned()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))
    }
}

#[test]
fn composition_constructs_typed_rezka_dependencies_without_network_calls() {
    let origin = "https://rezka-composition.invalid";
    let probe_url = "https://rezka-composition.invalid/account/probe";
    let store_path = "/tmp/rezka-session-composition-secret.bin";
    let encoded_key = STANDARD.encode([9_u8; 32]);

    let mut source = FakeSource::default();
    source.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
    source.set_secret("MEDIA_TOKEN_FILE", b"runner-token\n");
    source.set_env("MEDIA_REZKA_MIRRORS", origin);
    source.set_env("MEDIA_REZKA_SESSION_PROBE_URL", probe_url);
    source.set_env(
        "MEDIA_REZKA_SESSION_VALID_MARKERS_JSON",
        r#"["account-menu"]"#,
    );
    source.set_env(
        "MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON",
        r#"["login-form"]"#,
    );
    source.set_env("MEDIA_REZKA_SESSION_STORE_FILE", store_path);
    source.set_env("MEDIA_REZKA_USER_AGENT", "composition-test-agent/1.0");
    source.set_secret("MEDIA_REZKA_USERNAME_FILE", b"rezka-user");
    source.set_secret("MEDIA_REZKA_PASSWORD_FILE", b"rezka-password");
    source.set_env(
        "MEDIA_REZKA_CREDENTIAL_BROKER_URL",
        "https://broker.internal.example",
    );
    source.set_secret("MEDIA_REZKA_CREDENTIAL_BROKER_TOKEN_FILE", b"broker-token");
    source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", encoded_key.as_bytes());

    let config = RunnerConfig::load_from(&source).unwrap();
    let prepared = media::composition::prepare_runner_session(&config).unwrap();
    let _: &rezka_client::RezkaClient = &prepared.client;
    assert!(prepared.credentials.is_none());
    let _: &rezka_client::SessionValidationProbe = &prepared.probe;
    let _: &media_runner::EncryptedRezkaSessionStore = &prepared.store;
    let debug = format!("{prepared:?}");

    assert!(debug.contains("[REDACTED]"));
    for forbidden in [
        origin,
        probe_url,
        "account-menu",
        "login-form",
        "rezka-user",
        "rezka-password",
        encoded_key.as_str(),
        store_path,
    ] {
        assert!(
            !debug.contains(forbidden),
            "debug output exposed {forbidden}"
        );
    }
}

#[test]
fn composition_reads_the_encrypted_session_before_building_the_client() {
    let store_path = std::env::temp_dir().join(format!(
        "media-rezka-composition-{}.json",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&store_path, b"not-an-encrypted-session").unwrap();
    let encoded_key = STANDARD.encode([7_u8; 32]);
    let mut source = FakeSource::default();
    source.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
    source.set_secret("MEDIA_TOKEN_FILE", b"runner-token");
    source.set_env("MEDIA_REZKA_MIRRORS", "https://rezka.invalid");
    source.set_env(
        "MEDIA_REZKA_SESSION_PROBE_URL",
        "https://rezka.invalid/account/probe",
    );
    source.set_env(
        "MEDIA_REZKA_SESSION_VALID_MARKERS_JSON",
        r#"["account-menu"]"#,
    );
    source.set_env(
        "MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON",
        r#"["login-form"]"#,
    );
    source.set_env("MEDIA_REZKA_SESSION_STORE_FILE", store_path.as_os_str());
    source.set_env("MEDIA_REZKA_USER_AGENT", "composition-test-agent/1.0");
    source.set_secret("MEDIA_REZKA_USERNAME_FILE", b"rezka-user");
    source.set_secret("MEDIA_REZKA_PASSWORD_FILE", b"rezka-password");
    source.set_env(
        "MEDIA_REZKA_CREDENTIAL_BROKER_URL",
        "https://broker.internal.example",
    );
    source.set_secret("MEDIA_REZKA_CREDENTIAL_BROKER_TOKEN_FILE", b"broker-token");
    source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", encoded_key.as_bytes());

    let config = RunnerConfig::load_from(&source).unwrap();
    assert!(matches!(
        media::composition::prepare_runner_session(&config),
        Err(media::composition::RunnerCompositionError::Store)
    ));
    std::fs::remove_file(store_path).unwrap();
}
