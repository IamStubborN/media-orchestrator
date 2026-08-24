use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use scraper::{Html, Selector};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    RezkaError, mirror::same_origin, redaction::sanitize_provider_text, transport::Transport,
};

const CHALLENGE_SELECTOR: &str = "#anubis_challenge";
const PASS_CHALLENGE_PATH: &str = "/.within.website/x/cmd/anubis/api/pass-challenge";
pub const CLEARANCE_COOKIE: &str = "techaro.lol-anubis-auth";

/// Optional escape hatch for future Anubis algorithms that cannot be solved natively.
///
/// The application deliberately ships with no implementation and never enables this path from a
/// public route. Implementations must return only `Set-Cookie` values; challenge HTML and browser
/// state stay inside the isolated challenge context.
pub trait BrowserChallengeFallback: Send {
    fn solve<'a>(
        &'a mut self,
        challenge: &'a AnubisChallenge,
        origin: &'a Url,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, RezkaError>> + Send + 'a>>;
}
const MAX_DIFFICULTY: u8 = 32;
// Difficulty counts leading zero *nibbles* (see `has_leading_zero_nibbles`), so difficulty D needs
// ~2^(4D) expected SHA-256 hashes. The production `anubis_max_nonce` is 5_000_000 (~2^22), so only
// difficulty <= 5 (~2^20 ~= 1M) is comfortably solvable; difficulty 6 (~2^24 ~= 16M) is not. A
// parse-time ceiling rejects unreachable difficulties as `ProviderResponseInvalid` before they burn
// the full 0..=max_nonce sweep under the process-global proof semaphore. `MAX_DIFFICULTY` stays as
// the solver's secondary bound.
const MAX_ACCEPTED_DIFFICULTY: u8 = 5;
// Provider values are short opaque tokens; generous caps bound retained state and per-nonce hashing.
const MAX_CHALLENGE_ID_BYTES: usize = 1_024;
const MAX_RANDOM_DATA_BYTES: usize = 4_096;
const MAX_U64_DECIMAL_DIGITS: usize = 20;

#[derive(Clone, Eq, PartialEq)]
pub struct AnubisChallenge {
    pub id: String,
    pub random_data: String,
    pub difficulty: u8,
}

impl fmt::Debug for AnubisChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnubisChallenge")
            .field("id", &"[REDACTED]")
            .field("random_data", &"[REDACTED]")
            .field("difficulty", &self.difficulty)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct AnubisProof {
    pub response_hex: String,
    pub nonce: u64,
}

impl fmt::Debug for AnubisProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AnubisProof { response_hex: [REDACTED], nonce: [REDACTED] }")
    }
}

#[derive(Deserialize)]
struct ChallengeDocument {
    challenge: ChallengeFields,
    rules: ChallengeRules,
}

#[derive(Deserialize)]
struct ChallengeFields {
    id: String,
    #[serde(rename = "randomData")]
    random_data: String,
}

#[derive(Deserialize)]
struct ChallengeRules {
    difficulty: u8,
    #[serde(default)]
    algorithm: Option<String>,
}

#[must_use]
pub fn detect_challenge(html: &str) -> bool {
    let Ok(selector) = Selector::parse(CHALLENGE_SELECTOR) else {
        return false;
    };

    Html::parse_document(html)
        .select(&selector)
        .next()
        .is_some()
}

pub fn parse_challenge(html: &str) -> Result<AnubisChallenge, RezkaError> {
    parse_optional_challenge(html)?.ok_or_else(|| invalid_challenge("challenge element missing"))
}

pub(crate) fn parse_challenge_for_fallback(html: &str) -> Result<AnubisChallenge, RezkaError> {
    let selector = Selector::parse(CHALLENGE_SELECTOR)
        .map_err(|_| invalid_challenge("challenge selector invalid"))?;
    let document = Html::parse_document(html);
    let Some(element) = document.select(&selector).next() else {
        return Err(invalid_challenge("challenge element missing"));
    };
    let payload = element.text().collect::<String>();
    let parsed: ChallengeDocument = serde_json::from_str(&payload)
        .map_err(|_| invalid_challenge("challenge payload invalid"))?;
    if parsed.challenge.id.trim().is_empty()
        || parsed.challenge.random_data.trim().is_empty()
        || parsed.challenge.id.len() > MAX_CHALLENGE_ID_BYTES
        || parsed.challenge.random_data.len() > MAX_RANDOM_DATA_BYTES
        || !(1..=MAX_DIFFICULTY).contains(&parsed.rules.difficulty)
    {
        return Err(invalid_challenge("challenge fields invalid"));
    }
    Ok(AnubisChallenge {
        id: parsed.challenge.id,
        random_data: parsed.challenge.random_data,
        difficulty: parsed.rules.difficulty,
    })
}

pub(crate) fn parse_optional_challenge(html: &str) -> Result<Option<AnubisChallenge>, RezkaError> {
    let selector = Selector::parse(CHALLENGE_SELECTOR)
        .map_err(|_| invalid_challenge("challenge selector invalid"))?;
    let document = Html::parse_document(html);
    let Some(element) = document.select(&selector).next() else {
        return Ok(None);
    };
    let payload = element.text().collect::<String>();
    let parsed: ChallengeDocument = serde_json::from_str(&payload)
        .map_err(|_| invalid_challenge("challenge payload invalid"))?;

    if parsed
        .rules
        .algorithm
        .as_deref()
        .is_some_and(|algorithm| algorithm != "fast")
    {
        return Err(unsupported_algorithm());
    }

    if parsed.challenge.id.trim().is_empty()
        || parsed.challenge.random_data.trim().is_empty()
        || parsed.challenge.id.len() > MAX_CHALLENGE_ID_BYTES
        || parsed.challenge.random_data.len() > MAX_RANDOM_DATA_BYTES
        || parsed.rules.difficulty == 0
    {
        return Err(invalid_challenge("challenge fields invalid"));
    }

    if parsed.rules.difficulty > MAX_ACCEPTED_DIFFICULTY {
        return Err(excessive_difficulty());
    }

    Ok(Some(AnubisChallenge {
        id: parsed.challenge.id,
        random_data: parsed.challenge.random_data,
        difficulty: parsed.rules.difficulty,
    }))
}

pub fn solve_challenge(
    challenge: &AnubisChallenge,
    max_nonce: u64,
) -> Result<AnubisProof, RezkaError> {
    solve_challenge_with_cancellation(challenge, max_nonce, None)
}

/// Solve one proof on a blocking worker with a process-wide budget and deadline.
pub(crate) async fn solve_challenge_bounded(
    challenge: AnubisChallenge,
    max_nonce: u64,
    timeout: Duration,
) -> Result<AnubisProof, RezkaError> {
    let permit = proof_semaphore()
        .acquire_owned()
        .await
        .map_err(|_| timeout_challenge())?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation_guard = CancellationGuard {
        cancelled: Arc::clone(&cancelled),
    };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        solve_challenge_with_cancellation(&challenge, max_nonce, Some(&cancelled))
    });
    let result = tokio::time::timeout(timeout, task).await;
    drop(cancellation_guard);
    match result {
        Ok(Ok(Ok(proof))) => Ok(proof),
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(_)) | Err(_) => Err(timeout_challenge()),
    }
}

fn proof_semaphore() -> Arc<tokio::sync::Semaphore> {
    static SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SEMAPHORE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

struct CancellationGuard {
    cancelled: Arc<AtomicBool>,
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn solve_challenge_with_cancellation(
    challenge: &AnubisChallenge,
    max_nonce: u64,
    cancelled: Option<&AtomicBool>,
) -> Result<AnubisProof, RezkaError> {
    if !(1..=MAX_DIFFICULTY).contains(&challenge.difficulty) {
        return Err(challenge_failed());
    }

    let mut seeded_hasher = Sha256::new();
    seeded_hasher.update(challenge.random_data.as_bytes());
    let mut nonce_buffer = [0_u8; MAX_U64_DECIMAL_DIGITS];

    for nonce in 0..=max_nonce {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
            return Err(cancelled_challenge());
        }
        let mut hasher = seeded_hasher.clone();
        hasher.update(encode_decimal_nonce(nonce, &mut nonce_buffer));
        let digest = hasher.finalize();

        if has_leading_zero_nibbles(&digest, challenge.difficulty) {
            return Ok(AnubisProof {
                response_hex: hex::encode(digest),
                nonce,
            });
        }
    }

    Err(challenge_failed())
}

fn encode_decimal_nonce(mut nonce: u64, buffer: &mut [u8; MAX_U64_DECIMAL_DIGITS]) -> &[u8] {
    let mut start = buffer.len();
    loop {
        start -= 1;
        buffer[start] = b'0' + (nonce % 10) as u8;
        nonce /= 10;
        if nonce == 0 {
            return &buffer[start..];
        }
    }
}

fn has_leading_zero_nibbles(digest: &[u8], difficulty: u8) -> bool {
    let zero_bytes = usize::from(difficulty / 2);
    if digest[..zero_bytes].iter().any(|byte| *byte != 0) {
        return false;
    }

    difficulty.is_multiple_of(2) || digest[zero_bytes] & 0xf0 == 0
}

pub async fn submit_challenge(
    transport: &mut Transport,
    challenge: &AnubisChallenge,
    proof: &AnubisProof,
    redir: Url,
    elapsed_ms: u128,
) -> Result<(), RezkaError> {
    if !same_origin(transport.selected_origin(), &redir) {
        return Err(invalid_challenge("challenge redirect origin invalid"));
    }

    let mut url = transport
        .selected_origin()
        .join(PASS_CHALLENGE_PATH)
        .map_err(|_| invalid_challenge("challenge endpoint invalid"))?;
    let nonce = proof.nonce.to_string();
    let elapsed_ms = elapsed_ms.to_string();
    url.query_pairs_mut()
        .append_pair("id", &challenge.id)
        .append_pair("response", &proof.response_hex)
        .append_pair("nonce", &nonce)
        .append_pair("redir", redir.as_str())
        .append_pair("elapsedTime", &elapsed_ms);

    let response = transport
        .get_first_without_challenge(url, Some(redir))
        .await?;
    if !response.status.is_redirection() {
        return Err(invalid_challenge("challenge pass response invalid"));
    }

    Ok(())
}

fn invalid_challenge(reason: &str) -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text(reason),
    }
}

fn unsupported_algorithm() -> RezkaError {
    RezkaError::AnubisUnsupportedAlgorithm {
        context: sanitize_provider_text("Anubis algorithm is not supported"),
    }
}

fn excessive_difficulty() -> RezkaError {
    RezkaError::AnubisExcessiveDifficulty {
        context: sanitize_provider_text("Anubis proof difficulty exceeds the configured bound"),
    }
}

fn challenge_failed() -> RezkaError {
    RezkaError::ChallengeFailed {
        context: sanitize_provider_text("bounded proof of work exhausted"),
    }
}

fn cancelled_challenge() -> RezkaError {
    RezkaError::AnubisTimeout {
        context: sanitize_provider_text("proof of work cancelled"),
    }
}

fn timeout_challenge() -> RezkaError {
    RezkaError::AnubisTimeout {
        context: sanitize_provider_text("proof of work timed out"),
    }
}

#[cfg(test)]
mod tests {
    use super::{AnubisChallenge, solve_challenge_bounded};
    use crate::RezkaErrorCode;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn bounded_solver_reports_timeout_without_blocking_the_runtime() {
        let error = solve_challenge_bounded(
            AnubisChallenge {
                id: "test".to_owned(),
                random_data: "deterministic-timeout".to_owned(),
                difficulty: 32,
            },
            u64::MAX,
            Duration::ZERO,
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), RezkaErrorCode::AnubisTimeout);
    }
}
