use std::{
    fmt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use secrecy::SecretString;
use time::Duration;
use url::Url;

use crate::{
    RezkaError,
    mirror::MirrorSet,
    redaction::sanitize_provider_text,
    session::{
        anubis::{parse_optional_challenge, solve_challenge_with_cancellation, submit_challenge},
        cookie::{SessionJar, SessionSnapshot},
    },
    transport::Transport,
};

pub mod anubis;
pub mod cookie;
pub mod dle;
pub mod validation;

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
        let transport = Transport::from_snapshot_with_proxy(
            config.mirrors,
            snapshot,
            config.user_agent,
            config.request_timeout,
            config.max_retries,
            config.proxy_url,
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

        if let Some(challenge) = parse_optional_challenge(response.body())? {
            let started = Instant::now();
            let proof = solve_challenge_async(challenge.clone(), self.anubis_max_nonce).await?;
            submit_challenge(
                &mut self.transport,
                &challenge,
                &proof,
                response.url.clone(),
                started.elapsed().as_millis(),
            )
            .await?;

            response = self.fetch_probe(probe).await?;
            if parse_optional_challenge(response.body())?.is_some() {
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
        if parse_optional_challenge(response.body())?.is_some() {
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

    pub async fn ensure_session(
        &mut self,
        probe: &SessionValidationProbe,
    ) -> Result<SessionValidation, RezkaError> {
        let mut response = self.fetch_probe(probe).await?;

        if let Some(challenge) = parse_optional_challenge(response.body())? {
            let started = Instant::now();
            let proof = solve_challenge_async(challenge.clone(), self.anubis_max_nonce).await?;
            submit_challenge(
                &mut self.transport,
                &challenge,
                &proof,
                response.url.clone(),
                started.elapsed().as_millis(),
            )
            .await?;

            response = self.fetch_probe(probe).await?;
            if parse_optional_challenge(response.body())?.is_some() {
                return Err(challenge_failed());
            }
        }

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

    pub(crate) fn transport_mut(&mut self) -> &mut Transport {
        &mut self.transport
    }

    fn with_jar(config: RezkaClientConfig, jar: SessionJar) -> Result<Self, RezkaError> {
        let transport = Transport::new_with_proxy(
            config.mirrors,
            jar,
            config.user_agent,
            config.request_timeout,
            config.max_retries,
            config.proxy_url,
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

async fn solve_challenge_async(
    challenge: anubis::AnubisChallenge,
    max_nonce: u64,
) -> Result<anubis::AnubisProof, RezkaError> {
    let permit = proof_semaphore()
        .acquire_owned()
        .await
        .map_err(|_| challenge_solver_join_failed())?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation_guard = ProofCancellationGuard {
        cancelled: Arc::clone(&cancelled),
    };
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        solve_challenge_with_cancellation(&challenge, max_nonce, Some(&cancelled))
    })
    .await
    .map_err(|_| challenge_solver_join_failed())?;
    drop(cancellation_guard);
    result
}

fn proof_semaphore() -> Arc<tokio::sync::Semaphore> {
    static SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SEMAPHORE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

struct ProofCancellationGuard {
    cancelled: Arc<AtomicBool>,
}

impl Drop for ProofCancellationGuard {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

fn inconclusive_validation() -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text("session validation inconclusive"),
    }
}

#[cfg(test)]
mod tests {
    use super::{proof_semaphore, solve_challenge_async};
    use crate::{RezkaErrorCode, session::anubis::AnubisChallenge};

    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn challenge(difficulty: u8) -> AnubisChallenge {
        AnubisChallenge {
            id: "test".to_owned(),
            random_data: "deterministic-test-proof".to_owned(),
            difficulty,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn global_proof_budget_serializes_without_timing_assumptions() {
        let _test_guard = TEST_LOCK.lock().await;
        let held_permit = proof_semaphore().acquire_owned().await.unwrap();
        let proof = tokio::spawn(solve_challenge_async(challenge(33), 0));

        tokio::task::yield_now().await;
        assert!(!proof.is_finished());

        drop(held_permit);
        let error = proof.await.unwrap().unwrap_err();
        assert_eq!(error.code(), RezkaErrorCode::ChallengeFailed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn abort_cancels_blocking_proof_and_releases_permit_without_timers() {
        let _test_guard = TEST_LOCK.lock().await;
        let proof = tokio::spawn(solve_challenge_async(challenge(32), u64::MAX));
        wait_for_available_permits(0).await;

        proof.abort();
        let _ = proof.await;
        wait_for_available_permits(1).await;

        let error = solve_challenge_async(challenge(33), 0).await.unwrap_err();
        assert_eq!(error.code(), RezkaErrorCode::ChallengeFailed);
    }

    async fn wait_for_available_permits(expected: usize) {
        for _ in 0..100_000 {
            if proof_semaphore().available_permits() == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("proof permit did not reach {expected}");
    }
}
