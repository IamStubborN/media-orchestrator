use std::fmt;

use scraper::{Html, Selector};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    RezkaError, mirror::same_origin, redaction::sanitize_provider_text, transport::Transport,
};

const CHALLENGE_SELECTOR: &str = "#anubis_challenge";
const PASS_CHALLENGE_PATH: &str = "/.within.website/x/cmd/anubis/api/pass-challenge";
const MAX_DIFFICULTY: u8 = 32;
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
    let selector = Selector::parse(CHALLENGE_SELECTOR)
        .map_err(|_| invalid_challenge("challenge selector invalid"))?;
    let document = Html::parse_document(html);
    let element = document
        .select(&selector)
        .next()
        .ok_or_else(|| invalid_challenge("challenge element missing"))?;
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

pub fn solve_challenge(
    challenge: &AnubisChallenge,
    max_nonce: u64,
) -> Result<AnubisProof, RezkaError> {
    if !(1..=MAX_DIFFICULTY).contains(&challenge.difficulty) {
        return Err(challenge_failed());
    }

    let mut seeded_hasher = Sha256::new();
    seeded_hasher.update(challenge.random_data.as_bytes());
    let mut nonce_buffer = [0_u8; MAX_U64_DECIMAL_DIGITS];

    for nonce in 0..=max_nonce {
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
        .append_pair("nonce", &nonce)
        .append_pair("response", &proof.response_hex)
        .append_pair("redir", redir.as_str())
        .append_pair("elapsedTime", &elapsed_ms);

    let response = transport.get_first(url, Some(redir)).await?;
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

fn challenge_failed() -> RezkaError {
    RezkaError::ChallengeFailed {
        context: sanitize_provider_text("bounded proof of work exhausted"),
    }
}
