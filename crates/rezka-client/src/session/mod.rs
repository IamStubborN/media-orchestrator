use time::Duration;
use url::Url;

use crate::{
    RezkaError, mirror::MirrorSet, redaction::sanitize_provider_text,
    session::cookie::SessionSnapshot, transport::Transport,
};

pub mod anubis;
pub mod cookie;
pub mod validation;

pub use cookie::SessionJar;
pub use validation::{ProbeResponse, SessionValidation, SessionValidationProbe};

#[derive(Clone)]
pub struct RezkaClientConfig {
    pub mirrors: MirrorSet,
    pub user_agent: String,
    pub request_timeout: Duration,
    pub max_retries: u8,
    pub anubis_max_nonce: u64,
    pub proxy_url: Option<Url>,
}

pub struct RezkaClient {
    transport: Transport,
}

impl RezkaClient {
    pub fn new(config: RezkaClientConfig) -> Result<Self, RezkaError> {
        Self::with_jar(config, SessionJar::empty())
    }

    pub fn from_snapshot(
        config: RezkaClientConfig,
        snapshot: &SessionSnapshot,
    ) -> Result<Self, RezkaError> {
        let transport = Transport::from_snapshot_with_proxy_and_anubis(
            config.mirrors,
            snapshot,
            config.user_agent,
            config.request_timeout,
            config.max_retries,
            config.proxy_url,
            config.anubis_max_nonce,
        )?;
        Ok(Self { transport })
    }

    pub async fn ensure_session(
        &mut self,
        probe: &SessionValidationProbe,
    ) -> Result<SessionValidation, RezkaError> {
        let response = self.fetch_probe(probe).await?;

        Self::ready_session(probe, &response)
    }

    pub async fn validate_session(
        &mut self,
        probe: &SessionValidationProbe,
    ) -> Result<SessionValidation, RezkaError> {
        let response = self.fetch_probe(probe).await?;
        Self::ready_session(probe, &response)
    }

    fn ready_session(
        probe: &SessionValidationProbe,
        response: &ProbeResponse,
    ) -> Result<SessionValidation, RezkaError> {
        match Self::classify_probe(probe, response) {
            SessionValidation::Inconclusive => Err(inconclusive_validation()),
            status => Ok(status),
        }
    }

    pub async fn fetch_probe(
        &mut self,
        probe: &SessionValidationProbe,
    ) -> Result<ProbeResponse, RezkaError> {
        if !self.transport.contains_mirror_origin(&probe.url) {
            return Err(RezkaError::Configuration {
                message: "probe origin is not a configured Rezka mirror",
            });
        }

        let response = self
            .transport
            .get_first_with_failover(probe.url.clone(), None)
            .await?;
        Ok(ProbeResponse::new(
            response.status,
            response.url,
            response.body,
        ))
    }

    #[must_use]
    pub fn classify_probe(
        probe: &SessionValidationProbe,
        response: &ProbeResponse,
    ) -> SessionValidation {
        validation::classify(probe, response)
    }

    pub fn export_session(&self) -> Result<SessionSnapshot, RezkaError> {
        self.transport.export_session()
    }

    /// Remove only the retired DLE authentication cookie during anonymous
    /// session migration. The encrypted snapshot remains otherwise intact.
    pub fn remove_dle_authentication_cookie(&mut self) -> bool {
        self.transport.remove_dle_authentication_cookie()
    }

    /// Drop only the Anubis clearance, preserving every other anonymous cookie.
    pub fn invalidate_anubis_clearance(&mut self) -> bool {
        self.transport.invalidate_anubis_clearance()
    }

    pub fn with_browser_fallback(
        mut self,
        fallback: Box<dyn anubis::BrowserChallengeFallback>,
    ) -> Self {
        self.transport = self.transport.with_browser_fallback(fallback);
        self
    }

    pub(crate) fn transport_mut(&mut self) -> &mut Transport {
        &mut self.transport
    }

    fn with_jar(config: RezkaClientConfig, jar: SessionJar) -> Result<Self, RezkaError> {
        let transport = Transport::new_with_proxy_and_anubis(
            config.mirrors,
            jar,
            config.user_agent,
            config.request_timeout,
            config.max_retries,
            config.proxy_url,
            config.anubis_max_nonce,
        )?;
        Ok(Self { transport })
    }
}

fn inconclusive_validation() -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text("session validation inconclusive"),
    }
}
