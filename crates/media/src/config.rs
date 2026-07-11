use std::{
    ffi::OsString,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};

const DATABASE_URL_FILE: &str = "MEDIA_DATABASE_URL_FILE";
const LISTEN_ADDR: &str = "MEDIA_LISTEN_ADDR";
const PRIMARY_TOKEN_FILE: &str = "MEDIA_PRIMARY_TOKEN_FILE";
const SECONDARY_TOKEN_FILE: &str = "MEDIA_SECONDARY_TOKEN_FILE";
const RUNNER_TOKEN_FILE: &str = "MEDIA_RUNNER_TOKEN_FILE";
const LEASE_TTL_SECONDS: &str = "MEDIA_LEASE_TTL_SECONDS";
const SERVICE_URL: &str = "MEDIA_SERVICE_URL";
const TOKEN_FILE: &str = "MEDIA_TOKEN_FILE";
const REZKA_MIRRORS: &str = "MEDIA_REZKA_MIRRORS";
const REZKA_SESSION_PROBE_URL: &str = "MEDIA_REZKA_SESSION_PROBE_URL";
const REZKA_SESSION_VALID_MARKERS_JSON: &str = "MEDIA_REZKA_SESSION_VALID_MARKERS_JSON";
const REZKA_SESSION_INVALID_MARKERS_JSON: &str = "MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON";
const REZKA_USERNAME_FILE: &str = "MEDIA_REZKA_USERNAME_FILE";
const REZKA_PASSWORD_FILE: &str = "MEDIA_REZKA_PASSWORD_FILE";
const REZKA_COOKIE_KEY_FILE: &str = "MEDIA_REZKA_COOKIE_KEY_FILE";
const REZKA_SESSION_STORE_FILE: &str = "MEDIA_REZKA_SESSION_STORE_FILE";
const REZKA_USER_AGENT: &str = "MEDIA_REZKA_USER_AGENT";
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8080";
const DEFAULT_REZKA_USER_AGENT: &str = "media-orchestrator/0.1 rezka-session";
const DEFAULT_LEASE_TTL_SECONDS: i64 = 60;
const MIN_LEASE_TTL_SECONDS: i64 = 30;
const MAX_LEASE_TTL_SECONDS: i64 = 300;
const MAX_TOKEN_BYTES: usize = 512;
const MAX_REZKA_USERNAME_BYTES: usize = 256;
const MAX_REZKA_PASSWORD_BYTES: usize = 1024;
const MAX_REZKA_MARKERS: usize = 64;
const MAX_REZKA_MARKER_BYTES: usize = 256;
const MAX_REZKA_MARKERS_JSON_BYTES: usize = 32 * 1024;

pub trait ConfigSource {
    fn var_os(&self, name: &'static str) -> Option<OsString>;

    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
}

#[derive(Debug, Copy, Clone)]
pub struct ProcessConfigSource;

impl ConfigSource for ProcessConfigSource {
    fn var_os(&self, name: &'static str) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("required environment variable {name} is not set")]
    MissingEnvironment { name: &'static str },
    #[error("environment variable {name} is invalid")]
    InvalidEnvironment { name: &'static str },
    #[error("secret file configured by {name} could not be read: {kind:?}")]
    UnreadableSecret {
        name: &'static str,
        kind: io::ErrorKind,
    },
    #[error("secret file configured by {name} is empty or invalid")]
    InvalidSecret { name: &'static str },
    #[error("configuration invalid: {message}")]
    InvalidConfiguration { message: &'static str },
}

pub struct DatabaseConfig {
    database_url: SecretString,
}

impl DatabaseConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&ProcessConfigSource)
    }

    pub fn load_from(source: &impl ConfigSource) -> Result<Self, ConfigError> {
        Ok(Self {
            database_url: read_secret(source, DATABASE_URL_FILE, SecretKind::DatabaseUrl)?,
        })
    }

    #[must_use]
    pub const fn database_url(&self) -> &SecretString {
        &self.database_url
    }
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DatabaseConfig")
            .field("database_url", &"[REDACTED]")
            .finish()
    }
}

pub struct ServerConfig {
    database_url: SecretString,
    listen_addr: SocketAddr,
    primary_token: SecretString,
    secondary_token: SecretString,
    runner_token: SecretString,
    lease_ttl: time::Duration,
}

impl ServerConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&ProcessConfigSource)
    }

    pub fn load_from(source: &impl ConfigSource) -> Result<Self, ConfigError> {
        let database = DatabaseConfig::load_from(source)?;
        let listen_addr = optional_environment(source, LISTEN_ADDR)?
            .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_owned())
            .parse()
            .map_err(|_| ConfigError::InvalidEnvironment { name: LISTEN_ADDR })?;
        let lease_ttl_seconds = optional_environment(source, LEASE_TTL_SECONDS)?.map_or(
            Ok(DEFAULT_LEASE_TTL_SECONDS),
            |value| {
                value
                    .parse::<i64>()
                    .map_err(|_| ConfigError::InvalidEnvironment {
                        name: LEASE_TTL_SECONDS,
                    })
            },
        )?;
        if !(MIN_LEASE_TTL_SECONDS..=MAX_LEASE_TTL_SECONDS).contains(&lease_ttl_seconds) {
            return Err(ConfigError::InvalidEnvironment {
                name: LEASE_TTL_SECONDS,
            });
        }

        Ok(Self {
            database_url: database.database_url,
            listen_addr,
            primary_token: read_secret(source, PRIMARY_TOKEN_FILE, SecretKind::Token)?,
            secondary_token: read_secret(source, SECONDARY_TOKEN_FILE, SecretKind::Token)?,
            runner_token: read_secret(source, RUNNER_TOKEN_FILE, SecretKind::Token)?,
            lease_ttl: time::Duration::seconds(lease_ttl_seconds),
        })
    }

    #[must_use]
    pub const fn database_url(&self) -> &SecretString {
        &self.database_url
    }

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    #[must_use]
    pub const fn primary_token(&self) -> &SecretString {
        &self.primary_token
    }

    #[must_use]
    pub const fn secondary_token(&self) -> &SecretString {
        &self.secondary_token
    }

    #[must_use]
    pub const fn runner_token(&self) -> &SecretString {
        &self.runner_token
    }

    #[must_use]
    pub const fn lease_ttl(&self) -> time::Duration {
        self.lease_ttl
    }
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerConfig")
            .field("database_url", &"[REDACTED]")
            .field("listen_addr", &self.listen_addr)
            .field("primary_token", &"[REDACTED]")
            .field("secondary_token", &"[REDACTED]")
            .field("runner_token", &"[REDACTED]")
            .field("lease_ttl", &self.lease_ttl)
            .finish()
    }
}

pub struct ClientConfig {
    service_url: reqwest::Url,
    token: SecretString,
}

impl ClientConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&ProcessConfigSource)
    }

    pub fn load_from(source: &impl ConfigSource) -> Result<Self, ConfigError> {
        let service_url = required_environment(source, SERVICE_URL)?;
        let mut service_url = reqwest::Url::parse(&service_url)
            .map_err(|_| ConfigError::InvalidEnvironment { name: SERVICE_URL })?;
        if !matches!(service_url.scheme(), "http" | "https")
            || service_url.host_str().is_none()
            || !service_url.username().is_empty()
            || service_url.password().is_some()
            || service_url.query().is_some()
            || service_url.fragment().is_some()
        {
            return Err(ConfigError::InvalidEnvironment { name: SERVICE_URL });
        }
        if !service_url.path().ends_with('/') {
            let directory_path = format!("{}/", service_url.path());
            service_url.set_path(&directory_path);
        }

        Ok(Self {
            service_url,
            token: read_secret(source, TOKEN_FILE, SecretKind::Token)?,
        })
    }

    pub(crate) fn into_parts(self) -> (reqwest::Url, SecretString) {
        (self.service_url, self.token)
    }
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientConfig")
            .field("service_url", &"[REDACTED]")
            .field("token", &"[REDACTED]")
            .finish()
    }
}

pub struct RunnerConfig {
    service: ClientConfig,
    rezka: RezkaCompositionConfig,
}

impl RunnerConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&ProcessConfigSource)
    }

    pub fn load_from(source: &impl ConfigSource) -> Result<Self, ConfigError> {
        let service = ClientConfig::load_from(source)?;
        let mirrors = parse_rezka_mirrors(&required_environment(source, REZKA_MIRRORS)?)?;

        // The probe and marker contract is deployment supplied; it is not a generic provider API.
        let session_probe_url =
            parse_rezka_probe_url(&required_environment(source, REZKA_SESSION_PROBE_URL)?)?;
        if !mirrors
            .iter()
            .any(|mirror| same_effective_origin(mirror, &session_probe_url))
        {
            return Err(ConfigError::InvalidConfiguration {
                message: "Rezka session probe origin is not configured",
            });
        }
        let session_valid_markers = parse_rezka_markers(&required_environment(
            source,
            REZKA_SESSION_VALID_MARKERS_JSON,
        )?)?;
        let session_invalid_markers = parse_rezka_markers(&required_environment(
            source,
            REZKA_SESSION_INVALID_MARKERS_JSON,
        )?)?;

        Ok(Self {
            service,
            rezka: RezkaCompositionConfig {
                mirrors,
                session_probe_url,
                session_valid_markers,
                session_invalid_markers,
                username: read_secret(source, REZKA_USERNAME_FILE, SecretKind::RezkaUsername)?,
                password: read_secret(source, REZKA_PASSWORD_FILE, SecretKind::RezkaPassword)?,
                cookie_key: read_rezka_cookie_key(source)?,
                session_store_path: required_path_environment(source, REZKA_SESSION_STORE_FILE)?,
                user_agent: optional_environment(source, REZKA_USER_AGENT)?
                    .unwrap_or_else(|| DEFAULT_REZKA_USER_AGENT.to_owned()),
            },
        })
    }

    #[must_use]
    pub const fn service(&self) -> &ClientConfig {
        &self.service
    }

    #[must_use]
    pub const fn rezka(&self) -> &RezkaCompositionConfig {
        &self.rezka
    }
}

impl std::fmt::Debug for RunnerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunnerConfig")
            .field("service", &"[REDACTED]")
            .field("rezka", &"[REDACTED]")
            .finish()
    }
}

pub struct RezkaCompositionConfig {
    mirrors: Vec<url::Url>,
    session_probe_url: url::Url,
    session_valid_markers: Vec<String>,
    session_invalid_markers: Vec<String>,
    username: SecretString,
    password: SecretString,
    cookie_key: SecretBox<[u8; 32]>,
    session_store_path: PathBuf,
    user_agent: String,
}

impl RezkaCompositionConfig {
    #[must_use]
    pub fn mirrors(&self) -> &[url::Url] {
        &self.mirrors
    }

    #[must_use]
    pub const fn session_probe_url(&self) -> &url::Url {
        &self.session_probe_url
    }

    #[must_use]
    pub fn session_valid_markers(&self) -> &[String] {
        &self.session_valid_markers
    }

    #[must_use]
    pub fn session_invalid_markers(&self) -> &[String] {
        &self.session_invalid_markers
    }

    #[must_use]
    pub const fn username(&self) -> &SecretString {
        &self.username
    }

    #[must_use]
    pub const fn password(&self) -> &SecretString {
        &self.password
    }

    #[must_use]
    pub const fn cookie_key(&self) -> &SecretBox<[u8; 32]> {
        &self.cookie_key
    }

    #[must_use]
    pub fn session_store_path(&self) -> &Path {
        &self.session_store_path
    }

    #[must_use]
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

impl std::fmt::Debug for RezkaCompositionConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RezkaCompositionConfig")
            .field("mirrors", &"[REDACTED]")
            .field("session_probe_url", &"[REDACTED]")
            .field("session_valid_markers", &"[REDACTED]")
            .field("session_invalid_markers", &"[REDACTED]")
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .field("cookie_key", &"[REDACTED]")
            .field("session_store_path", &"[REDACTED]")
            .field("user_agent", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Copy, Clone)]
enum SecretKind {
    DatabaseUrl,
    Token,
    RezkaUsername,
    RezkaPassword,
}

fn read_secret(
    source: &impl ConfigSource,
    name: &'static str,
    kind: SecretKind,
) -> Result<SecretString, ConfigError> {
    let contents = read_secret_bytes(source, name)?;

    let valid = match kind {
        SecretKind::DatabaseUrl => {
            !contents.is_empty()
                && contents
                    .iter()
                    .all(|byte| !byte.is_ascii_whitespace() && !byte.is_ascii_control())
        }
        SecretKind::Token => {
            !contents.is_empty()
                && contents.len() <= MAX_TOKEN_BYTES
                && contents.iter().all(|byte| (0x21..=0x7e).contains(byte))
        }
        SecretKind::RezkaUsername => {
            !contents.is_empty()
                && contents.len() <= MAX_REZKA_USERNAME_BYTES
                && contents.iter().all(|byte| (0x21..=0x7e).contains(byte))
        }
        SecretKind::RezkaPassword => {
            !contents.is_empty()
                && contents.len() <= MAX_REZKA_PASSWORD_BYTES
                && !contents.contains(&0)
        }
    };
    if !valid {
        return Err(ConfigError::InvalidSecret { name });
    }
    let contents = String::from_utf8(contents).map_err(|_| ConfigError::InvalidSecret { name })?;
    Ok(SecretString::from(contents))
}

fn read_secret_bytes(
    source: &impl ConfigSource,
    name: &'static str,
) -> Result<Vec<u8>, ConfigError> {
    let path = source
        .var_os(name)
        .ok_or(ConfigError::MissingEnvironment { name })?;
    if path.is_empty() {
        return Err(ConfigError::InvalidEnvironment { name });
    }
    let mut contents =
        source
            .read(Path::new(&path))
            .map_err(|error| ConfigError::UnreadableSecret {
                name,
                kind: error.kind(),
            })?;
    strip_one_final_line_ending(&mut contents);
    Ok(contents)
}

fn read_rezka_cookie_key(source: &impl ConfigSource) -> Result<SecretBox<[u8; 32]>, ConfigError> {
    let encoded = SecretBox::new(Box::new(read_secret_bytes(source, REZKA_COOKIE_KEY_FILE)?));
    let decoded = STANDARD.decode(encoded.expose_secret()).map_err(|_| {
        ConfigError::InvalidConfiguration {
            message: "Rezka cookie key must be base64",
        }
    })?;
    let decoded = SecretBox::new(Box::new(decoded));
    if decoded.expose_secret().len() != 32 {
        return Err(ConfigError::InvalidConfiguration {
            message: "Rezka cookie key must decode to 32 bytes",
        });
    }

    Ok(SecretBox::<[u8; 32]>::init_with_mut(|key| {
        key.copy_from_slice(decoded.expose_secret());
    }))
}

fn parse_rezka_mirrors(value: &str) -> Result<Vec<url::Url>, ConfigError> {
    let mirrors = value
        .split(',')
        .map(|candidate| {
            if candidate.is_empty() || candidate != candidate.trim() {
                return Err(invalid_rezka_mirror());
            }
            let mirror = url::Url::parse(candidate).map_err(|_| invalid_rezka_mirror())?;
            if !is_valid_rezka_mirror(&mirror) {
                return Err(invalid_rezka_mirror());
            }
            Ok(mirror)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if mirrors.is_empty() {
        return Err(invalid_rezka_mirror());
    }
    Ok(mirrors)
}

fn is_valid_rezka_mirror(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.port_or_known_default().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
}

const fn invalid_rezka_mirror() -> ConfigError {
    ConfigError::InvalidConfiguration {
        message: "invalid Rezka mirror origin",
    }
}

fn parse_rezka_probe_url(value: &str) -> Result<url::Url, ConfigError> {
    if value.is_empty() || value != value.trim() {
        return Err(invalid_rezka_probe());
    }
    let url = url::Url::parse(value).map_err(|_| invalid_rezka_probe())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || url.port_or_known_default().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_rezka_probe());
    }
    Ok(url)
}

const fn invalid_rezka_probe() -> ConfigError {
    ConfigError::InvalidConfiguration {
        message: "invalid Rezka session probe URL",
    }
}

fn same_effective_origin(left: &url::Url, right: &url::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host() == right.host()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn parse_rezka_markers(value: &str) -> Result<Vec<String>, ConfigError> {
    if value.len() > MAX_REZKA_MARKERS_JSON_BYTES {
        return Err(rezka_marker_limit_error());
    }
    let markers: Vec<String> =
        serde_json::from_str(value).map_err(|_| ConfigError::InvalidConfiguration {
            message: "Rezka validation markers must be JSON arrays of strings",
        })?;
    if markers.is_empty() || markers.iter().any(|marker| marker.trim().is_empty()) {
        return Err(ConfigError::InvalidConfiguration {
            message: "Rezka validation markers must be non-empty",
        });
    }
    if markers.len() > MAX_REZKA_MARKERS
        || markers
            .iter()
            .any(|marker| marker.len() > MAX_REZKA_MARKER_BYTES)
    {
        return Err(rezka_marker_limit_error());
    }
    Ok(markers)
}

const fn rezka_marker_limit_error() -> ConfigError {
    ConfigError::InvalidConfiguration {
        message: "Rezka validation markers exceed limits",
    }
}

fn strip_one_final_line_ending(contents: &mut Vec<u8>) {
    if contents.last() == Some(&b'\n') {
        contents.pop();
        if contents.last() == Some(&b'\r') {
            contents.pop();
        }
    }
}

fn optional_environment(
    source: &impl ConfigSource,
    name: &'static str,
) -> Result<Option<String>, ConfigError> {
    source
        .var_os(name)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| ConfigError::InvalidEnvironment { name })
        })
        .transpose()
}

fn required_environment(
    source: &impl ConfigSource,
    name: &'static str,
) -> Result<String, ConfigError> {
    optional_environment(source, name)?.ok_or(ConfigError::MissingEnvironment { name })
}

fn required_path_environment(
    source: &impl ConfigSource,
    name: &'static str,
) -> Result<PathBuf, ConfigError> {
    let path = source
        .var_os(name)
        .ok_or(ConfigError::MissingEnvironment { name })?;
    if path.is_empty() {
        return Err(ConfigError::InvalidEnvironment { name });
    }
    Ok(PathBuf::from(path))
}
