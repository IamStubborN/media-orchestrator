use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use media::config::{
    ClientConfig, ConfigError, ConfigSource, DatabaseConfig, ProcessConfigSource, RunnerConfig,
    ServerConfig,
};
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
        source.set_secret("MEDIA_LIFECYCLE_TOKEN_FILE", b"lifecycle-secret");
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

fn valid_runner_source() -> FakeSource {
    let mut source = FakeSource::default();
    let encoded_key = STANDARD.encode([7_u8; 32]);
    source.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
    source.set_secret("MEDIA_TOKEN_FILE", b"runner-token");
    source.set_env("MEDIA_REZKA_MIRRORS", "https://rezka.test");
    source.set_env(
        "MEDIA_REZKA_SESSION_PROBE_URL",
        "https://rezka.test/account/probe",
    );
    source.set_env(
        "MEDIA_REZKA_SESSION_VALID_MARKERS_JSON",
        r#"["account-menu"]"#,
    );
    source.set_env(
        "MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON",
        r#"["login-form"]"#,
    );
    source.set_env(
        "MEDIA_REZKA_SESSION_STORE_FILE",
        "/runner/rezka/session.bin",
    );
    source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", encoded_key.as_bytes());
    source
}

#[test]
fn runner_config_loads_distinct_existing_torrent_categories() {
    let mut source = valid_runner_source();
    source.set_env("MEDIA_QBITTORRENT_URL", "http://gluetun:8400");
    source.set_env("MEDIA_QBITTORRENT_TV_CATEGORY", "tv");
    source.set_env("MEDIA_QBITTORRENT_MOVIES_CATEGORY", "movies");
    source.set_env("MEDIA_QBITTORRENT_USERNAME", "runner");
    source.set_secret("MEDIA_QBITTORRENT_PASSWORD_FILE", b"qbit-secret");

    let config = RunnerConfig::load_from(&source).unwrap();
    let qbittorrent = config.qbittorrent().unwrap();

    assert_eq!(qbittorrent.tv_category(), "tv");
    assert_eq!(qbittorrent.movies_category(), "movies");
}

#[test]
fn runner_storage_reserve_defaults_to_zero_and_accepts_explicit_bytes() {
    let source = valid_runner_source();
    assert_eq!(
        RunnerConfig::load_from(&source)
            .unwrap()
            .storage_reserve_bytes(),
        0
    );

    let mut source = valid_runner_source();
    source.set_env("MEDIA_STORAGE_RESERVE_BYTES", "1073741824");
    assert_eq!(
        RunnerConfig::load_from(&source)
            .unwrap()
            .storage_reserve_bytes(),
        1_073_741_824
    );

    source.set_env("MEDIA_STORAGE_RESERVE_BYTES", "-1");
    assert_eq!(
        RunnerConfig::load_from(&source).unwrap_err(),
        ConfigError::InvalidEnvironment {
            name: "MEDIA_STORAGE_RESERVE_BYTES"
        }
    );
}

#[test]
fn database_config_requires_the_database_url_setting() {
    let error = DatabaseConfig::load_from(&FakeSource::default()).unwrap_err();

    assert_eq!(
        error,
        ConfigError::MissingEnvironment {
            name: "MEDIA_DATABASE_URL",
        },
    );
}

#[test]
fn notification_webhooks_are_optional_but_both_hmac_secrets_are_atomic() {
    let source = FakeSource::valid_server();
    assert!(
        ServerConfig::load_from(&source)
            .unwrap()
            .notifications()
            .is_none()
    );

    let mut partial = FakeSource::valid_server();
    partial.set_secret("MEDIA_PRIMARY_WEBHOOK_HMAC_FILE", b"primary-hmac");
    assert_eq!(
        ServerConfig::load_from(&partial).unwrap_err(),
        ConfigError::MissingEnvironment {
            name: "MEDIA_SECONDARY_WEBHOOK_HMAC",
        }
    );

    let mut enabled = FakeSource::valid_server();
    enabled.set_secret("MEDIA_PRIMARY_WEBHOOK_HMAC_FILE", b"primary-hmac");
    enabled.set_secret("MEDIA_SECONDARY_WEBHOOK_HMAC_FILE", b"secondary-hmac");
    let config = ServerConfig::load_from(&enabled).unwrap();
    let rendered = format!("{:?}", config.notifications().unwrap());
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("hermes-primary"));
    assert!(!rendered.contains("primary-hmac"));
}

#[test]
fn server_config_uses_public_tvmaze_defaults_without_a_key() {
    let config = ServerConfig::load_from(&FakeSource::valid_server()).unwrap();

    assert_eq!(
        config.tvmaze().base_url().as_str(),
        "https://api.tvmaze.com/"
    );
    assert!(config.tvmaze().user_agent().contains("media-orchestrator"));
}

#[test]
fn server_tmdb_config_is_optional_and_redacts_the_api_key() {
    let source = FakeSource::valid_server();
    assert!(ServerConfig::load_from(&source).unwrap().tmdb().is_none());

    let mut source = FakeSource::valid_server();
    source.set_env("MEDIA_TMDB_API_KEY", "tmdb-secret");
    let config = ServerConfig::load_from(&source).unwrap();
    let tmdb = config.tmdb().unwrap();
    assert_eq!(tmdb.base_url().as_str(), "https://api.themoviedb.org/3/");
    assert_eq!(tmdb.language(), "ru");
    let rendered = format!("{tmdb:?}");
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("tmdb-secret"));
}

#[test]
fn database_config_accepts_direct_environment_and_prefers_file_override() {
    let mut direct = FakeSource::default();
    direct.set_env("MEDIA_DATABASE_URL", "postgres://direct");
    assert_eq!(
        DatabaseConfig::load_from(&direct)
            .unwrap()
            .database_url()
            .expose_secret(),
        "postgres://direct"
    );

    direct.set_secret("MEDIA_DATABASE_URL_FILE", b"postgres://file");
    assert_eq!(
        DatabaseConfig::load_from(&direct)
            .unwrap()
            .database_url()
            .expose_secret(),
        "postgres://file"
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
fn database_config_enforces_the_post_trim_byte_limit() {
    const MAX_DATABASE_URL_BYTES: usize = 8 * 1024;
    let mut source = FakeSource::default();
    source.set_secret(
        "MEDIA_DATABASE_URL_FILE",
        &[vec![b'x'; MAX_DATABASE_URL_BYTES], b"\n".to_vec()].concat(),
    );

    let config = DatabaseConfig::load_from(&source).unwrap();
    assert_eq!(
        config.database_url().expose_secret().len(),
        MAX_DATABASE_URL_BYTES
    );

    source.set_secret(
        "MEDIA_DATABASE_URL_FILE",
        &vec![b'y'; MAX_DATABASE_URL_BYTES + 1],
    );
    assert_eq!(
        DatabaseConfig::load_from(&source).unwrap_err(),
        ConfigError::InvalidSecret {
            name: "MEDIA_DATABASE_URL_FILE",
        }
    );
}

#[cfg(unix)]
#[test]
fn process_config_source_rejects_secret_symlinks_without_leaking_paths_or_values() {
    use std::os::unix::fs::symlink;

    let directory = std::env::temp_dir().join(format!(
        "media-config-source-symlink-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory).unwrap();
    let target = directory.join("target-secret-value");
    let link = directory.join("configured-secret-path");
    std::fs::write(&target, b"forbidden-secret-value").unwrap();
    symlink(&target, &link).unwrap();

    let error = ProcessConfigSource.read_bounded(&link, 512).unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert!(!rendered.contains("configured-secret-path"));
    assert!(!rendered.contains("target-secret-value"));
    assert!(!rendered.contains("forbidden-secret-value"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn process_config_source_enforces_exact_bounded_read_lengths() {
    let path = std::env::temp_dir().join(format!(
        "media-config-source-boundary-{}",
        std::process::id()
    ));
    std::fs::write(&path, [b'x'; 16]).unwrap();
    assert_eq!(
        ProcessConfigSource.read_bounded(&path, 16).unwrap(),
        [b'x'; 16]
    );

    std::fs::write(&path, [b'y'; 17]).unwrap();
    let error = ProcessConfigSource.read_bounded(&path, 16).unwrap_err();
    let rendered = format!("{error:?}: {error}");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!rendered.contains("media-config-source-boundary"));
    assert!(!rendered.contains(&"y".repeat(17)));
    std::fs::remove_file(path).unwrap();
}

#[cfg(unix)]
#[test]
fn process_config_source_rejects_fifo_without_blocking() {
    let path =
        std::env::temp_dir().join(format!("media-config-source-fifo-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );

    let started = std::time::Instant::now();
    let error = ProcessConfigSource.read_bounded(&path, 512).unwrap_err();

    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!format!("{error:?}: {error}").contains("media-config-source-fifo"));
    std::fs::remove_file(path).unwrap();
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

#[test]
fn runner_config_loads_rezka_secret_files_getters_defaults_and_redacts_debug() {
    let mut source = valid_runner_source();
    source.set_env(
        "MEDIA_REZKA_MIRRORS",
        "https://rezka.test,https://rezka-alt.test",
    );
    source.set_env(
        "MEDIA_REZKA_SESSION_VALID_MARKERS_JSON",
        r#"["account-menu","logout-link"]"#,
    );
    source.set_env(
        "MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON",
        r#"["login-form","login_name"]"#,
    );
    source.set_secret("MEDIA_TOKEN_FILE", b"runner-token\n");
    source.set_env("MEDIA_RUNNER_EXIT_AFTER_JOB", "true");

    let config = RunnerConfig::load_from(&source).unwrap();
    let debug = format!("{config:?}");
    let encoded_key = STANDARD.encode([7_u8; 32]);

    assert!(format!("{:?}", config.service()).contains("[REDACTED]"));
    assert_eq!(config.rezka().mirrors().len(), 2);
    assert_eq!(
        config.rezka().session_probe_url().as_str(),
        "https://rezka.test/account/probe"
    );
    assert_eq!(
        config.rezka().session_valid_markers(),
        ["account-menu", "logout-link"]
    );
    assert_eq!(
        config.rezka().session_invalid_markers(),
        ["login-form", "login_name"]
    );
    assert_eq!(config.rezka().cookie_key().expose_secret(), &[7_u8; 32]);
    assert_eq!(
        config.rezka().session_store_path(),
        Path::new("/runner/rezka/session.bin")
    );
    assert_eq!(
        config.rezka().user_agent(),
        "media-orchestrator/0.1 rezka-session"
    );
    assert!(config.exit_after_job());
    assert!(debug.contains("[REDACTED]"));
    for forbidden in [
        "media.internal.example",
        "runner-token",
        "rezka.test",
        "account/probe",
        "account-menu",
        "login-form",
        encoded_key.as_str(),
        "/runner/rezka/session.bin",
    ] {
        assert!(
            !debug.contains(forbidden),
            "debug output exposed {forbidden}"
        );
    }
}

#[test]
fn runner_config_reads_the_cookie_key_only_from_the_configured_file() {
    let name = "MEDIA_REZKA_COOKIE_KEY_FILE";
    let mut source = valid_runner_source();
    source.set_env(name, "inline-secret-value");

    assert_eq!(
        RunnerConfig::load_from(&source).unwrap_err(),
        ConfigError::UnreadableSecret {
            name,
            kind: io::ErrorKind::NotFound
        }
    );
}

#[test]
fn runner_config_rejects_invalid_mirror_origins_without_echoing_them() {
    for mirror in [
        "http://rezka.test",
        "https://user:pass@rezka.test",
        "https://rezka.test/path",
        "https://rezka.test?token=secret",
        "https://rezka.test#secret-fragment",
        "ftp://rezka.test",
        "not-a-url-secret",
    ] {
        let mut source = valid_runner_source();
        source.set_env("MEDIA_REZKA_MIRRORS", mirror);

        let error = RunnerConfig::load_from(&source).unwrap_err();
        let rendered = format!("{error:?}: {error}");
        assert_eq!(
            error.to_string(),
            "configuration invalid: invalid Rezka mirror origin"
        );
        assert!(!rendered.contains(mirror));
    }
}

#[test]
fn runner_config_rejects_invalid_probe_urls_and_non_member_origins_without_echoing_them() {
    for probe in [
        "http://rezka.test/account/probe",
        "https://user:pass@rezka.test/account/probe",
        "https://rezka.test/account/probe?token=secret",
        "https://rezka.test/account/probe#secret-fragment",
        "ftp://rezka.test/account/probe",
        "not-a-probe-url-secret",
    ] {
        let mut source = valid_runner_source();
        source.set_env("MEDIA_REZKA_SESSION_PROBE_URL", probe);

        let error = RunnerConfig::load_from(&source).unwrap_err();
        let rendered = format!("{error:?}: {error}");
        assert_eq!(
            error.to_string(),
            "configuration invalid: invalid Rezka session probe URL"
        );
        assert!(!rendered.contains(probe));
    }

    for probe in [
        "https://rezka-alt.test/account/probe",
        "https://rezka.test:444/account/probe",
    ] {
        let mut source = valid_runner_source();
        source.set_env("MEDIA_REZKA_SESSION_PROBE_URL", probe);

        let error = RunnerConfig::load_from(&source).unwrap_err();
        assert_eq!(
            error.to_string(),
            "configuration invalid: Rezka session probe origin is not configured"
        );
        assert!(!format!("{error:?}: {error}").contains(probe));
    }
}

#[test]
fn runner_config_accepts_probe_with_the_same_effective_origin() {
    let mut source = valid_runner_source();
    source.set_env("MEDIA_REZKA_MIRRORS", "https://rezka.test:443");

    RunnerConfig::load_from(&source).unwrap();
}

#[test]
fn runner_config_rejects_malformed_or_wrong_length_cookie_keys_without_echoing_them() {
    for line_ending in ["\n", "\r\n"] {
        let mut source = valid_runner_source();
        let key = format!("{}{line_ending}", STANDARD.encode([8_u8; 32]));
        source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", key.as_bytes());
        RunnerConfig::load_from(&source).unwrap();
    }

    let short_key = STANDARD.encode([8_u8; 31]);
    for (key, expected) in [
        (
            "not-base64-secret",
            "configuration invalid: Rezka cookie key must be base64",
        ),
        (
            short_key.as_str(),
            "configuration invalid: Rezka cookie key must decode to 32 bytes",
        ),
    ] {
        let mut source = valid_runner_source();
        source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", key.as_bytes());

        let error = RunnerConfig::load_from(&source).unwrap_err();
        let rendered = format!("{error:?}: {error}");
        assert_eq!(error.to_string(), expected);
        assert!(!rendered.contains(key));
    }

    let mut double_newline = valid_runner_source();
    let key = format!("{}\n\n", STANDARD.encode([8_u8; 32]));
    double_newline.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", key.as_bytes());
    assert_eq!(
        RunnerConfig::load_from(&double_newline)
            .unwrap_err()
            .to_string(),
        "configuration invalid: Rezka cookie key must be base64"
    );
}

#[test]
fn runner_config_rejects_invalid_and_unbounded_validation_marker_arrays() {
    let maximum_markers = serde_json::to_string(&vec!["m".repeat(256); 64]).unwrap();
    let mut maximum_source = valid_runner_source();
    maximum_source.set_env("MEDIA_REZKA_SESSION_VALID_MARKERS_JSON", maximum_markers);
    RunnerConfig::load_from(&maximum_source).unwrap();

    for name in [
        "MEDIA_REZKA_SESSION_VALID_MARKERS_JSON",
        "MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON",
    ] {
        let mut source = valid_runner_source();
        source.env.remove(name);
        assert_eq!(
            RunnerConfig::load_from(&source).unwrap_err(),
            ConfigError::MissingEnvironment { name }
        );
    }

    let too_many = serde_json::to_string(&vec!["marker"; 65]).unwrap();
    let oversized_marker = serde_json::to_string(&vec!["m".repeat(257)]).unwrap();
    for (valid, invalid, expected) in [
        (
            "[]",
            r#"["login-form"]"#,
            "configuration invalid: Rezka validation markers must be non-empty",
        ),
        (
            r#"["account-menu"]"#,
            "[]",
            "configuration invalid: Rezka validation markers must be non-empty",
        ),
        (
            r#"[""]"#,
            r#"["login-form"]"#,
            "configuration invalid: Rezka validation markers must be non-empty",
        ),
        (
            r#"["account-menu"]"#,
            r#"["   "]"#,
            "configuration invalid: Rezka validation markers must be non-empty",
        ),
        (
            "not-json-secret",
            r#"["login-form"]"#,
            "configuration invalid: Rezka validation markers must be JSON arrays of strings",
        ),
        (
            r#"{"marker":"account-menu"}"#,
            r#"["login-form"]"#,
            "configuration invalid: Rezka validation markers must be JSON arrays of strings",
        ),
        (
            r#"["account-menu",7]"#,
            r#"["login-form"]"#,
            "configuration invalid: Rezka validation markers must be JSON arrays of strings",
        ),
        (
            too_many.as_str(),
            r#"["login-form"]"#,
            "configuration invalid: Rezka validation markers exceed limits",
        ),
        (
            oversized_marker.as_str(),
            r#"["login-form"]"#,
            "configuration invalid: Rezka validation markers exceed limits",
        ),
    ] {
        let mut source = valid_runner_source();
        source.set_env("MEDIA_REZKA_SESSION_VALID_MARKERS_JSON", valid);
        source.set_env("MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON", invalid);

        let error = RunnerConfig::load_from(&source).unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert!(!format!("{error:?}: {error}").contains("not-json-secret"));
    }
}
