use std::{fmt, io::Write as _, path::Path};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use base64::Engine as _;
use rand::{RngCore as _, rngs::OsRng};
use rezka_client::SessionSnapshot;
use secrecy::{ExposeSecret as _, SecretBox};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroize as _;

const ENVELOPE_VERSION: u8 = 1;
const NONCE_LENGTH: usize = 12;
const AAD: &[u8] = b"media-orchestrator:rezka-session:v1";

#[derive(Error, Debug, PartialEq, Eq)]
pub enum RezkaSessionStoreError {
    #[error("encrypted Rezka session path is invalid")]
    InvalidPath,
    #[error("encrypted Rezka session could not be read")]
    ReadFailed,
    #[error("encrypted Rezka session could not be written")]
    WriteFailed,
    #[error("encrypted Rezka session could not be deleted")]
    DeleteFailed,
    #[error("encrypted Rezka session envelope is invalid")]
    InvalidEnvelope,
    #[error("encrypted Rezka session envelope version is unsupported")]
    UnsupportedVersion,
    #[error("encrypted Rezka session could not be encrypted")]
    EncryptionFailed,
    #[error("encrypted Rezka session could not be decrypted")]
    DecryptionFailed,
}

pub struct RezkaSessionStoreConfig {
    pub path: std::path::PathBuf,
    pub key: SecretBox<[u8; 32]>,
}

impl fmt::Debug for RezkaSessionStoreConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RezkaSessionStoreConfig")
            .field("parent", &self.path.parent())
            .finish()
    }
}

pub struct EncryptedRezkaSessionStore {
    config: RezkaSessionStoreConfig,
}

impl fmt::Debug for EncryptedRezkaSessionStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncryptedRezkaSessionStore")
            .field("parent", &self.config.path.parent())
            .finish()
    }
}

impl EncryptedRezkaSessionStore {
    pub fn new(config: RezkaSessionStoreConfig) -> Result<Self, RezkaSessionStoreError> {
        config
            .path
            .parent()
            .ok_or(RezkaSessionStoreError::InvalidPath)?;
        Ok(Self { config })
    }

    pub fn load(&self) -> Result<Option<SessionSnapshot>, RezkaSessionStoreError> {
        let envelope_bytes = match std::fs::read(&self.config.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RezkaSessionStoreError::ReadFailed),
        };
        let envelope: Envelope = serde_json::from_slice(&envelope_bytes)
            .map_err(|_| RezkaSessionStoreError::InvalidEnvelope)?;
        if envelope.version != ENVELOPE_VERSION {
            return Err(RezkaSessionStoreError::UnsupportedVersion);
        }

        let nonce = decode_nonce(&envelope.nonce_b64)?;
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(envelope.ciphertext_b64)
            .map_err(|_| RezkaSessionStoreError::InvalidEnvelope)?;
        let cipher = cipher(&self.config.key);
        let nonce = Nonce::from(nonce);
        let mut plaintext = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: AAD,
                },
            )
            .map_err(|_| RezkaSessionStoreError::DecryptionFailed)?;
        let snapshot =
            SessionSnapshot::from_secret_bytes(SecretBox::new(Box::new(plaintext.clone())));
        plaintext.zeroize();

        Ok(Some(snapshot))
    }

    pub fn save(&self, snapshot: &SessionSnapshot) -> Result<(), RezkaSessionStoreError> {
        let mut nonce = [0_u8; NONCE_LENGTH];
        OsRng.fill_bytes(&mut nonce);
        let cipher = cipher(&self.config.key);
        let cipher_nonce = Nonce::from(nonce);
        let ciphertext = snapshot.with_secret_bytes(|plaintext| {
            cipher.encrypt(
                &cipher_nonce,
                Payload {
                    msg: plaintext,
                    aad: AAD,
                },
            )
        });
        let envelope = Envelope {
            version: ENVELOPE_VERSION,
            nonce_b64: base64::engine::general_purpose::STANDARD.encode(nonce),
            ciphertext_b64: base64::engine::general_purpose::STANDARD
                .encode(ciphertext.map_err(|_| RezkaSessionStoreError::EncryptionFailed)?),
        };
        let envelope_bytes =
            serde_json::to_vec(&envelope).map_err(|_| RezkaSessionStoreError::WriteFailed)?;

        persist_envelope(&self.config.path, &envelope_bytes)
    }

    pub fn delete(&self) -> Result<(), RezkaSessionStoreError> {
        match std::fs::remove_file(&self.config.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(RezkaSessionStoreError::DeleteFailed),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Envelope {
    version: u8,
    nonce_b64: String,
    ciphertext_b64: String,
}

fn cipher(key: &SecretBox<[u8; 32]>) -> Aes256Gcm {
    Aes256Gcm::new_from_slice(key.expose_secret()).expect("AES-256-GCM accepts a 32-byte key")
}

fn decode_nonce(encoded: &str) -> Result<[u8; NONCE_LENGTH], RezkaSessionStoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| RezkaSessionStoreError::InvalidEnvelope)?
        .try_into()
        .map_err(|_| RezkaSessionStoreError::InvalidEnvelope)
}

fn persist_envelope(path: &Path, envelope_bytes: &[u8]) -> Result<(), RezkaSessionStoreError> {
    let parent = path.parent().ok_or(RezkaSessionStoreError::InvalidPath)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| RezkaSessionStoreError::WriteFailed)?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
    temporary
        .as_file_mut()
        .write_all(envelope_bytes)
        .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
    temporary
        .persist(path)
        .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RezkaSessionStoreError::WriteFailed)?;

    Ok(())
}
