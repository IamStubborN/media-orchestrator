use std::{ffi::OsString, io, net::SocketAddr, path::Path};

use secrecy::SecretString;

const DATABASE_URL_FILE: &str = "MEDIA_DATABASE_URL_FILE";
const LISTEN_ADDR: &str = "MEDIA_LISTEN_ADDR";
const PRIMARY_TOKEN_FILE: &str = "MEDIA_PRIMARY_TOKEN_FILE";
const SECONDARY_TOKEN_FILE: &str = "MEDIA_SECONDARY_TOKEN_FILE";
const RUNNER_TOKEN_FILE: &str = "MEDIA_RUNNER_TOKEN_FILE";
const LEASE_TTL_SECONDS: &str = "MEDIA_LEASE_TTL_SECONDS";
const SERVICE_URL: &str = "MEDIA_SERVICE_URL";
const TOKEN_FILE: &str = "MEDIA_TOKEN_FILE";
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8080";
const DEFAULT_LEASE_TTL_SECONDS: i64 = 60;
const MIN_LEASE_TTL_SECONDS: i64 = 30;
const MAX_LEASE_TTL_SECONDS: i64 = 300;
const MAX_TOKEN_BYTES: usize = 512;

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

#[derive(Debug, Copy, Clone)]
enum SecretKind {
    DatabaseUrl,
    Token,
}

fn read_secret(
    source: &impl ConfigSource,
    name: &'static str,
    kind: SecretKind,
) -> Result<SecretString, ConfigError> {
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
    };
    if !valid {
        return Err(ConfigError::InvalidSecret { name });
    }
    let contents = String::from_utf8(contents).map_err(|_| ConfigError::InvalidSecret { name })?;
    Ok(SecretString::from(contents))
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
