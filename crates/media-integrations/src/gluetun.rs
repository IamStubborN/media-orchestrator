use std::{fmt, future::Future, net::IpAddr, time::Duration};

use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep},
};
use url::Url;

/// Maximum time to wait for the tunnel to report `running` after a rotation.
const ROTATION_RUNNING_DEADLINE: Duration = Duration::from_secs(30);
const ROTATION_POLL_INITIAL: Duration = Duration::from_millis(200);
const ROTATION_POLL_MAX: Duration = Duration::from_secs(2);

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum GluetunErrorCode {
    Configuration,
    InvalidJob,
    StickyJobActive,
    LeaseMismatch,
    Transport,
    Unauthorized,
    ProviderResponse,
    UnexpectedVpnState,
}

#[derive(Debug, thiserror::Error)]
pub enum GluetunError {
    #[error("Gluetun configuration is invalid: {message}")]
    Configuration { message: &'static str },
    #[error("sticky job is invalid")]
    InvalidJob,
    #[error("VPN rotation is forbidden while a sticky job is active")]
    StickyJobActive,
    #[error("sticky job lease does not match the active job")]
    LeaseMismatch,
    #[error("Gluetun request failed")]
    Transport,
    #[error("Gluetun authentication failed")]
    Unauthorized,
    #[error("Gluetun returned an invalid response ({status})")]
    ProviderResponse { status: StatusCode },
    #[error("Gluetun VPN did not return to the running state")]
    UnexpectedVpnState,
}

impl GluetunError {
    #[must_use]
    pub const fn code(&self) -> GluetunErrorCode {
        match self {
            Self::Configuration { .. } => GluetunErrorCode::Configuration,
            Self::InvalidJob => GluetunErrorCode::InvalidJob,
            Self::StickyJobActive => GluetunErrorCode::StickyJobActive,
            Self::LeaseMismatch => GluetunErrorCode::LeaseMismatch,
            Self::Transport => GluetunErrorCode::Transport,
            Self::Unauthorized => GluetunErrorCode::Unauthorized,
            Self::ProviderResponse { .. } => GluetunErrorCode::ProviderResponse,
            Self::UnexpectedVpnState => GluetunErrorCode::UnexpectedVpnState,
        }
    }
}

pub struct GluetunConfig {
    base_url: Url,
    api_key: SecretString,
    timeout: Duration,
}

impl GluetunConfig {
    pub fn new(
        mut base_url: Url,
        api_key: SecretString,
        timeout: Duration,
    ) -> Result<Self, GluetunError> {
        if base_url.cannot_be_a_base() || base_url.host_str().is_none() {
            return Err(GluetunError::Configuration {
                message: "base URL must be absolute",
            });
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            return Err(GluetunError::Configuration {
                message: "base URL must not contain credentials",
            });
        }
        if base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(GluetunError::Configuration {
                message: "base URL must not contain a query or fragment",
            });
        }
        if api_key.expose_secret().is_empty() {
            return Err(GluetunError::Configuration {
                message: "API key must not be empty",
            });
        }
        if timeout.is_zero() {
            return Err(GluetunError::Configuration {
                message: "request timeout must be positive",
            });
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            base_url,
            api_key,
            timeout,
        })
    }
}

impl fmt::Debug for GluetunConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GluetunConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StickyJobLease {
    job_id: String,
    generation: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VpnRotation {
    pub previous_public_ip: Option<String>,
    pub current_public_ip: Option<String>,
}

#[derive(Default)]
struct StickyState {
    active: Option<StickyJobLease>,
    generation: u64,
}

pub struct GluetunClient {
    client: reqwest::Client,
    config: GluetunConfig,
    sticky: Mutex<StickyState>,
}

impl GluetunClient {
    pub fn new(config: GluetunConfig) -> Result<Self, GluetunError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| GluetunError::Configuration {
                message: "HTTP client could not be configured",
            })?;
        Ok(Self {
            client,
            config,
            sticky: Mutex::new(StickyState::default()),
        })
    }

    pub async fn begin_job(
        &self,
        job_id: impl Into<String>,
    ) -> Result<StickyJobLease, GluetunError> {
        let job_id = job_id.into();
        if job_id.trim().is_empty() {
            return Err(GluetunError::InvalidJob);
        }
        let mut state = self.sticky.lock().await;
        if state.active.is_some() {
            return Err(GluetunError::StickyJobActive);
        }
        state.generation = state.generation.wrapping_add(1);
        let lease = StickyJobLease {
            job_id,
            generation: state.generation,
        };
        state.active = Some(lease.clone());
        Ok(lease)
    }

    pub async fn end_job(&self, lease: StickyJobLease) -> Result<(), GluetunError> {
        let mut state = self.sticky.lock().await;
        if state.active.as_ref() != Some(&lease) {
            return Err(GluetunError::LeaseMismatch);
        }
        state.active = None;
        Ok(())
    }

    pub async fn rotate_between_jobs(&self) -> Result<VpnRotation, GluetunError> {
        self.rotate_between_jobs_within(ROTATION_RUNNING_DEADLINE)
            .await
    }

    /// Rotates the VPN, waiting up to `running_deadline` for the tunnel to come
    /// back up. Gluetun briefly reports transitional states while the tunnel
    /// re-establishes, so the running state is polled with bounded backoff
    /// rather than required on the first read.
    pub async fn rotate_between_jobs_within(
        &self,
        running_deadline: Duration,
    ) -> Result<VpnRotation, GluetunError> {
        let state = self.sticky.lock().await;
        if state.active.is_some() {
            return Err(GluetunError::StickyJobActive);
        }

        // Every control call below can briefly return a transport blip or a 5xx
        // while Gluetun tears the tunnel down and brings it back up. A single
        // transient failure must not fail an otherwise healthy rotation, so each
        // step is retried with the same bounded backoff as `await_running`.
        let previous_public_ip = self
            .retry_transient(running_deadline, || self.public_ip())
            .await?;
        self.retry_transient(running_deadline, || self.set_status(VpnStatus::Stopped))
            .await?;
        self.retry_transient(running_deadline, || self.set_status(VpnStatus::Running))
            .await?;
        self.await_running(running_deadline).await?;
        let current_public_ip = self
            .retry_transient(running_deadline, || self.public_ip())
            .await?;
        drop(state);
        Ok(VpnRotation {
            previous_public_ip: Some(previous_public_ip),
            current_public_ip: Some(current_public_ip),
        })
    }

    /// Retries a control-plane operation while it fails transiently (a transport
    /// error or a 5xx from the control server), using the same bounded
    /// exponential backoff as [`Self::await_running`]. Non-transient errors (for
    /// example authentication or a malformed body) are returned immediately.
    async fn retry_transient<T, F, Fut>(
        &self,
        deadline: Duration,
        mut operation: F,
    ) -> Result<T, GluetunError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, GluetunError>>,
    {
        let start = Instant::now();
        let mut delay = ROTATION_POLL_INITIAL;
        loop {
            match operation().await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if !is_transient(&error) || start.elapsed() + delay >= deadline {
                        return Err(error);
                    }
                    sleep(delay).await;
                    delay = (delay * 2).min(ROTATION_POLL_MAX);
                }
            }
        }
    }

    async fn await_running(&self, deadline: Duration) -> Result<(), GluetunError> {
        let start = Instant::now();
        let mut delay = ROTATION_POLL_INITIAL;
        loop {
            if self.status().await? == VpnStatusReport::Running {
                return Ok(());
            }
            if start.elapsed() + delay >= deadline {
                return Err(GluetunError::UnexpectedVpnState);
            }
            sleep(delay).await;
            delay = (delay * 2).min(ROTATION_POLL_MAX);
        }
    }

    async fn public_ip(&self) -> Result<String, GluetunError> {
        let response = self.get("v1/publicip/ip").await?;
        let status = response.status();
        let payload: PublicIp = response
            .json()
            .await
            .map_err(|_| GluetunError::ProviderResponse { status })?;
        payload
            .public_ip
            .parse::<IpAddr>()
            .map_err(|_| GluetunError::ProviderResponse { status })?;
        Ok(payload.public_ip)
    }

    async fn status(&self) -> Result<VpnStatusReport, GluetunError> {
        let response = self.get("v1/vpn/status").await?;
        let status = response.status();
        response
            .json()
            .await
            .map_err(|_| GluetunError::ProviderResponse { status })
    }

    async fn set_status(&self, status: VpnStatus) -> Result<(), GluetunError> {
        let endpoint = endpoint(&self.config.base_url, "v1/vpn/status")?;
        let response = self
            .client
            .put(endpoint)
            .header("X-API-Key", self.config.api_key.expose_secret())
            .json(&status)
            .send()
            .await
            .map_err(|_| GluetunError::Transport)?;
        self.validate_status(response.status())
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response, GluetunError> {
        let response = self
            .client
            .get(endpoint(&self.config.base_url, path)?)
            .header("X-API-Key", self.config.api_key.expose_secret())
            .send()
            .await
            .map_err(|_| GluetunError::Transport)?;
        self.validate_status(response.status())?;
        Ok(response)
    }

    fn validate_status(&self, status: StatusCode) -> Result<(), GluetunError> {
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(GluetunError::Unauthorized);
        }
        if !status.is_success() {
            return Err(GluetunError::ProviderResponse { status });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct PublicIp {
    public_ip: String,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum VpnStatus {
    Running,
    Stopped,
}

/// The status Gluetun reports back. Beyond the terminal `running`/`stopped`
/// states it also emits transitional values (for example while the tunnel is
/// coming up); any unrecognized status is treated as transitional rather than
/// failing deserialization.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum VpnStatusReport {
    Running,
    Stopped,
    #[serde(other)]
    Transitional,
}

/// A control call is worth retrying only when the failure is transient: a
/// transport-level error or a 5xx from the control server while the tunnel
/// restarts. Authentication, configuration, and malformed-body failures are
/// deterministic and are surfaced immediately.
fn is_transient(error: &GluetunError) -> bool {
    match error {
        GluetunError::Transport => true,
        GluetunError::ProviderResponse { status } => status.is_server_error(),
        _ => false,
    }
}

fn endpoint(base_url: &Url, path: &str) -> Result<Url, GluetunError> {
    base_url
        .join(path)
        .map_err(|_| GluetunError::Configuration {
            message: "control endpoint could not be constructed",
        })
}
