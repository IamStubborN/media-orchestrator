use std::{fmt, time::Instant};

use secrecy::SecretString;
use time::Duration;

use crate::{
    RezkaError,
    mirror::MirrorSet,
    redaction::sanitize_provider_text,
    session::{
        anubis::{detect_challenge, parse_challenge, solve_challenge, submit_challenge},
        cookie::{SessionJar, SessionSnapshot},
    },
    transport::Transport,
};

pub mod anubis;
pub mod cookie;
pub mod dle;
pub mod validation;

pub use validation::{ProbeResponse, SessionValidation, SessionValidationProbe};

pub struct RezkaClientConfig {
    pub mirrors: MirrorSet,
    pub user_agent: String,
    pub request_timeout: Duration,
    pub max_retries: u8,
    pub anubis_max_nonce: u64,
}

pub struct RezkaCredentials {
    pub username: SecretString,
    pub password: SecretString,
}

impl fmt::Debug for RezkaCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RezkaCredentials { username: [REDACTED], password: [REDACTED] }")
    }
}

pub struct RezkaClient {
    transport: Transport,
    anubis_max_nonce: u64,
}

impl RezkaClient {
    pub fn new(config: RezkaClientConfig) -> Result<Self, RezkaError> {
        Self::with_jar(config, SessionJar::empty())
    }

    pub fn from_snapshot(
        config: RezkaClientConfig,
        snapshot: &SessionSnapshot,
    ) -> Result<Self, RezkaError> {
        let transport = Transport::from_snapshot(
            config.mirrors,
            snapshot,
            config.user_agent,
            config.request_timeout,
            config.max_retries,
        )?;
        Ok(Self {
            transport,
            anubis_max_nonce: config.anubis_max_nonce,
        })
    }

    pub async fn ensure_authenticated(
        &mut self,
        credentials: &RezkaCredentials,
        probe: &SessionValidationProbe,
    ) -> Result<SessionValidation, RezkaError> {
        let mut response = self.fetch_probe(probe).await?;

        if detect_challenge(response.body()) {
            let started = Instant::now();
            let challenge = parse_challenge(response.body())?;
            let solver_challenge = challenge.clone();
            let max_nonce = self.anubis_max_nonce;
            let proof =
                tokio::task::spawn_blocking(move || solve_challenge(&solver_challenge, max_nonce))
                    .await
                    .map_err(|_| challenge_solver_join_failed())??;
            submit_challenge(
                &mut self.transport,
                &challenge,
                &proof,
                response.url.clone(),
                started.elapsed().as_millis(),
            )
            .await?;

            response = self.fetch_probe(probe).await?;
            if detect_challenge(response.body()) {
                return Err(challenge_failed());
            }
        }

        match Self::classify_probe(probe, &response) {
            SessionValidation::Valid => return Ok(SessionValidation::Valid),
            SessionValidation::Inconclusive => return Err(inconclusive_validation()),
            SessionValidation::Invalid => {}
        }

        dle::login(&mut self.transport, credentials).await?;
        let response = self.fetch_probe(probe).await?;
        if detect_challenge(response.body()) {
            return Err(challenge_failed());
        }

        match Self::classify_probe(probe, &response) {
            SessionValidation::Valid => Ok(SessionValidation::Valid),
            SessionValidation::Invalid => Err(RezkaError::AuthenticationRequired {
                context: sanitize_provider_text("session remains invalid after login"),
            }),
            SessionValidation::Inconclusive => Err(inconclusive_validation()),
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

    fn with_jar(config: RezkaClientConfig, jar: SessionJar) -> Result<Self, RezkaError> {
        let transport = Transport::new(
            config.mirrors,
            jar,
            config.user_agent,
            config.request_timeout,
            config.max_retries,
        )?;
        Ok(Self {
            transport,
            anubis_max_nonce: config.anubis_max_nonce,
        })
    }
}

fn challenge_failed() -> RezkaError {
    RezkaError::ChallengeFailed {
        context: sanitize_provider_text("challenge remained after one pass"),
    }
}

fn challenge_solver_join_failed() -> RezkaError {
    RezkaError::ChallengeFailed {
        context: sanitize_provider_text("challenge solver task failed"),
    }
}

fn inconclusive_validation() -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text("session validation inconclusive"),
    }
}
