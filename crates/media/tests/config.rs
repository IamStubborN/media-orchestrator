use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use media::config::{ClientConfig, ConfigError, ConfigSource, DatabaseConfig, ServerConfig};
use secrecy::ExposeSecret;

#[derive(Default)]
struct FakeSource {
    env: HashMap<&'static str, OsString>,
    files: HashMap<PathBuf, Vec<u8>>,
    unreadable: HashSet<PathBuf>,
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

    fn valid_server() -> Self {
        let mut source = Self::default();
        source.set_secret(
            "MEDIA_DATABASE_URL_FILE",
            b"postgres://media:database-secret@localhost/media\n",
        );
        source.set_secret("MEDIA_PRIMARY_TOKEN_FILE", b"primary-secret\n");
        source.set_secret("MEDIA_SECONDARY_TOKEN_FILE", b"secondary-secret\r\n");
        source.set_secret("MEDIA_RUNNER_TOKEN_FILE", b"runner-secret");
        source
    }
}

impl ConfigSource for FakeSource {
    fn var_os(&self, name: &'static str) -> Option<OsString> {
        self.env.get(name).cloned()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if self.unreadable.contains(path) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        }
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))
    }
}

#[test]
fn database_config_requires_the_database_url_file_setting() {
    let error = DatabaseConfig::load_from(&FakeSource::default()).unwrap_err();

    assert_eq!(
        error,
        ConfigError::MissingEnvironment {
            name: "MEDIA_DATABASE_URL_FILE",
        },
    );
}

#[test]
fn database_config_reports_an_unreadable_secret_without_exposing_contents() {
    let path = PathBuf::from("/database-url.secret");
    let mut source = FakeSource::default();
    source.set_env("MEDIA_DATABASE_URL_FILE", path.as_os_str());
    source.unreadable.insert(path);

    let error = DatabaseConfig::load_from(&source).unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert!(rendered.contains("MEDIA_DATABASE_URL_FILE"));
    assert!(!rendered.contains("postgres://"));
}

#[test]
fn server_config_rejects_empty_and_whitespace_only_tokens() {
    for invalid in [b"\n".as_slice(), b" \t\n".as_slice()] {
        let mut source = FakeSource::valid_server();
        source.set_secret("MEDIA_RUNNER_TOKEN_FILE", invalid);

        assert_eq!(
            ServerConfig::load_from(&source).unwrap_err(),
            ConfigError::InvalidSecret {
                name: "MEDIA_RUNNER_TOKEN_FILE",
            },
        );
    }
}

#[test]
fn client_config_rejects_oversized_token_files() {
    let mut source = FakeSource::default();
    source.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
    source.set_secret("MEDIA_TOKEN_FILE", &vec![b'x'; 513]);

    assert_eq!(
        ClientConfig::load_from(&source).unwrap_err(),
        ConfigError::InvalidSecret {
            name: "MEDIA_TOKEN_FILE",
        },
    );
}

#[test]
fn server_config_accepts_one_normal_final_newline_and_uses_defaults() {
    let config = ServerConfig::load_from(&FakeSource::valid_server()).unwrap();

    assert_eq!(config.listen_addr().to_string(), "0.0.0.0:8080");
    assert_eq!(config.lease_ttl().whole_seconds(), 60);
    assert_eq!(config.primary_token().expose_secret(), "primary-secret");
    assert_eq!(config.secondary_token().expose_secret(), "secondary-secret");
}

#[test]
fn server_config_debug_redacts_database_url_and_all_tokens() {
    let config = ServerConfig::load_from(&FakeSource::valid_server()).unwrap();
    let debug = format!("{config:?}");

    assert!(debug.contains("[REDACTED]"));
    for secret in [
        "database-secret",
        "postgres://",
        "primary-secret",
        "secondary-secret",
        "runner-secret",
    ] {
        assert!(!debug.contains(secret), "debug output exposed {secret}");
    }
}

#[test]
fn server_config_validates_listen_address_and_lease_ttl_without_echoing_values() {
    for (name, value) in [
        ("MEDIA_LISTEN_ADDR", "not-an-address"),
        ("MEDIA_LEASE_TTL_SECONDS", "29"),
        ("MEDIA_LEASE_TTL_SECONDS", "301"),
    ] {
        let mut source = FakeSource::valid_server();
        source.set_env(name, value);

        let error = ServerConfig::load_from(&source).unwrap_err();
        assert_eq!(
            error,
            ConfigError::InvalidEnvironment { name },
            "unexpected validation result for {name}",
        );
        assert!(!error.to_string().contains(value));
    }
}

#[test]
fn client_config_rejects_credential_bearing_urls_and_redacts_valid_config() {
    let mut invalid = FakeSource::default();
    invalid.set_env(
        "MEDIA_SERVICE_URL",
        "https://user:password@media.internal.example",
    );
    invalid.set_secret("MEDIA_TOKEN_FILE", b"cli-token");

    let error = ClientConfig::load_from(&invalid).unwrap_err();
    assert_eq!(
        error,
        ConfigError::InvalidEnvironment {
            name: "MEDIA_SERVICE_URL",
        },
    );
    assert!(!format!("{error:?}: {error}").contains("password"));

    let mut valid = FakeSource::default();
    valid.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
    valid.set_secret("MEDIA_TOKEN_FILE", b"cli-token\n");
    let debug = format!("{:?}", ClientConfig::load_from(&valid).unwrap());

    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("media.internal.example"));
    assert!(!debug.contains("cli-token"));
}
