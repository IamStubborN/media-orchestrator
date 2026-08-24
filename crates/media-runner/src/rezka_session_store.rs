use std::{
    fmt,
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use base64::Engine as _;
use fs2::FileExt as _;
use rand::{RngCore as _, rngs::OsRng};
use rezka_client::SessionSnapshot;
use secrecy::{ExposeSecret as _, SecretBox};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const ENVELOPE_VERSION: u8 = 1;
const NONCE_LENGTH: usize = 12;
const MAX_ENVELOPE_BYTES: usize = 256 * 1024;
const MAX_SESSION_SNAPSHOT_PLAINTEXT_BYTES: usize = 128 * 1024;
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
    #[error("encrypted Rezka session lock could not be acquired")]
    LockFailed,
    #[error("encrypted Rezka session snapshot is healthy")]
    SnapshotHealthy,
    #[error("encrypted Rezka session snapshot is missing")]
    SnapshotMissing,
    #[error("encrypted Rezka session snapshot could not be quarantined")]
    QuarantineFailed,
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

/// Process-shared advisory lock for the complete anonymous session lifecycle.
///
/// Callers acquire this guard from `spawn_blocking` and keep it alive across load, probe/challenge
/// handling, and atomic save. The lock is a separate sibling file, so replacing `session.bin`
/// never invalidates a holder's descriptor.
pub struct RezkaSessionStoreGuard {
    file: File,
}

impl Drop for RezkaSessionStoreGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn acquire_rezka_session_lock(
    path: &Path,
) -> Result<RezkaSessionStoreGuard, RezkaSessionStoreError> {
    let lock_path = path
        .parent()
        .ok_or(RezkaSessionStoreError::InvalidPath)?
        .join("session.bin.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|_| RezkaSessionStoreError::LockFailed)?;
    file.lock_exclusive()
        .map_err(|_| RezkaSessionStoreError::LockFailed)?;
    Ok(RezkaSessionStoreGuard { file })
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

    pub fn lock(&self) -> Result<RezkaSessionStoreGuard, RezkaSessionStoreError> {
        acquire_rezka_session_lock(&self.config.path)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.config.path
    }

    pub fn load(&self) -> Result<Option<SessionSnapshot>, RezkaSessionStoreError> {
        let Some(envelope_bytes) = read_envelope(&self.config.path)? else {
            return Ok(None);
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
        let plaintext = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: AAD,
                },
            )
            .map_err(|_| RezkaSessionStoreError::DecryptionFailed)?;
        // SecretBox zeroizes its Vec on drop; moving it avoids another plaintext allocation.
        let snapshot = SessionSnapshot::from_secret_bytes(SecretBox::new(Box::new(plaintext)));

        Ok(Some(snapshot))
    }

    pub fn save(&self, snapshot: &SessionSnapshot) -> Result<(), RezkaSessionStoreError> {
        if snapshot
            .with_secret_bytes(|plaintext| plaintext.len() > MAX_SESSION_SNAPSHOT_PLAINTEXT_BYTES)
        {
            return Err(RezkaSessionStoreError::InvalidEnvelope);
        }
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
        if envelope_bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(RezkaSessionStoreError::InvalidEnvelope);
        }

        persist_envelope(&self.config.path, &envelope_bytes)
    }

    pub fn delete(&self) -> Result<(), RezkaSessionStoreError> {
        match std::fs::remove_file(&self.config.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(RezkaSessionStoreError::DeleteFailed),
        }
    }

    /// Explicitly move an invalid encrypted snapshot aside for operator recovery.
    ///
    /// The validation and move are kept under the same inter-process lock used by
    /// normal session lifecycle operations. Healthy and missing snapshots are
    /// rejected, and no automatic caller invokes this method.
    pub fn quarantine_corrupt_snapshot(&self) -> Result<PathBuf, RezkaSessionStoreError> {
        let _guard = self.lock()?;
        match self.load() {
            Ok(Some(_)) => return Err(RezkaSessionStoreError::SnapshotHealthy),
            Ok(None) => return Err(RezkaSessionStoreError::SnapshotMissing),
            Err(error) if is_quarantinable_corruption(&error) => {}
            Err(error) => return Err(error),
        }

        let source = &self.config.path;
        let parent = source.parent().ok_or(RezkaSessionStoreError::InvalidPath)?;
        let file_name = source
            .file_name()
            .ok_or(RezkaSessionStoreError::InvalidPath)?;
        let metadata = std::fs::symlink_metadata(source).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => RezkaSessionStoreError::SnapshotMissing,
            _ => RezkaSessionStoreError::QuarantineFailed,
        })?;
        if !metadata.is_file() {
            return Err(RezkaSessionStoreError::QuarantineFailed);
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RezkaSessionStoreError::QuarantineFailed)?;
        let base_name = file_name.to_string_lossy();
        for attempt in 0_u32..100 {
            let destination = parent.join(format!(
                "{base_name}.corrupt-{}-{:09}-{attempt}",
                timestamp.as_secs(),
                timestamp.subsec_nanos()
            ));
            if destination.exists() {
                continue;
            }
            return quarantine_file_with_ops(
                source,
                &destination,
                |from, to| std::fs::rename(from, to),
                sync_directory_io,
            );
        }

        Err(RezkaSessionStoreError::QuarantineFailed)
    }
}

fn is_quarantinable_corruption(error: &RezkaSessionStoreError) -> bool {
    matches!(
        error,
        RezkaSessionStoreError::InvalidEnvelope
            | RezkaSessionStoreError::UnsupportedVersion
            | RezkaSessionStoreError::DecryptionFailed
    )
}

fn quarantine_file_with_ops(
    source: &Path,
    destination: &Path,
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
    mut sync_parent: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<PathBuf, RezkaSessionStoreError> {
    let parent = source.parent().ok_or(RezkaSessionStoreError::InvalidPath)?;
    rename(source, destination).map_err(|_| RezkaSessionStoreError::QuarantineFailed)?;
    if sync_parent(parent).is_ok() {
        return Ok(destination.to_owned());
    }

    // A directory fsync failure means the move is not durable yet. Restore the
    // original name before reporting failure so an operator can retry safely.
    let _ = rename(destination, source);
    let _ = sync_parent(parent);
    // The original and quarantine names are both in the same directory; a
    // failed rollback is unrecoverable by this process, but never hide it as
    // success or emit any file contents.
    Err(RezkaSessionStoreError::QuarantineFailed)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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

fn read_envelope(path: &Path) -> Result<Option<Vec<u8>>, RezkaSessionStoreError> {
    let Some(file) = open_envelope_file(path)? else {
        return Ok(None);
    };
    let metadata = file
        .metadata()
        .map_err(|_| RezkaSessionStoreError::ReadFailed)?;
    if !metadata.is_file() || metadata.len() > MAX_ENVELOPE_BYTES as u64 {
        return Err(RezkaSessionStoreError::InvalidEnvelope);
    }

    let initial_capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_ENVELOPE_BYTES)
        .min(MAX_ENVELOPE_BYTES);
    read_bounded(file, initial_capacity).map(Some)
}

#[cfg(unix)]
fn open_envelope_file(path: &Path) -> Result<Option<std::fs::File>, RezkaSessionStoreError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let result = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    map_open_result(result)
}

#[cfg(not(unix))]
fn open_envelope_file(path: &Path) -> Result<Option<std::fs::File>, RezkaSessionStoreError> {
    // Rust has no portable no-follow open flag. This best-effort pre-check has a residual
    // symlink replacement race; descriptor metadata and bounded reads still enforce type/size.
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(RezkaSessionStoreError::InvalidEnvelope);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RezkaSessionStoreError::ReadFailed),
    }

    map_open_result(std::fs::OpenOptions::new().read(true).open(path))
}

fn map_open_result(
    result: std::io::Result<std::fs::File>,
) -> Result<Option<std::fs::File>, RezkaSessionStoreError> {
    match result {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(RezkaSessionStoreError::ReadFailed),
    }
}

fn read_bounded(
    reader: impl std::io::Read,
    initial_capacity: usize,
) -> Result<Vec<u8>, RezkaSessionStoreError> {
    let mut bytes = Vec::with_capacity(initial_capacity.min(MAX_ENVELOPE_BYTES));
    reader
        .take((MAX_ENVELOPE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| RezkaSessionStoreError::ReadFailed)?;
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(RezkaSessionStoreError::InvalidEnvelope);
    }

    Ok(bytes)
}

fn persist_envelope(path: &Path, envelope_bytes: &[u8]) -> Result<(), RezkaSessionStoreError> {
    persist_envelope_with_ops(
        path,
        envelope_bytes,
        |temporary, destination| {
            temporary
                .persist(destination)
                .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
            Ok(())
        },
        sync_directory,
    )
}

fn persist_envelope_with_ops(
    path: &Path,
    envelope_bytes: &[u8],
    replace: impl FnOnce(tempfile::NamedTempFile, &Path) -> Result<(), RezkaSessionStoreError>,
    mut sync_parent: impl FnMut(&Path) -> Result<(), RezkaSessionStoreError>,
) -> Result<(), RezkaSessionStoreError> {
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

    // Keep an encrypted copy of the previous snapshot until the replacement's
    // directory entry is durable. If the post-rename fsync fails, atomically
    // restore this copy before reporting the write failure.
    let previous =
        match open_envelope_file(path).map_err(|_| RezkaSessionStoreError::WriteFailed)? {
            Some(mut source) => {
                let mut backup = tempfile::NamedTempFile::new_in(parent)
                    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
                #[cfg(unix)]
                backup
                    .as_file()
                    .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
                    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
                std::io::copy(&mut source, backup.as_file_mut())
                    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
                backup
                    .as_file_mut()
                    .sync_all()
                    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
                Some(backup)
            }
            None => None,
        };
    replace(temporary, path)?;
    if sync_parent(parent).is_err() {
        match previous {
            Some(backup) => {
                backup
                    .persist(path)
                    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
            }
            None => match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(RezkaSessionStoreError::WriteFailed),
            },
        }
        let _ = sync_parent(parent);
        return Err(RezkaSessionStoreError::WriteFailed);
    }

    Ok(())
}

fn sync_directory_io(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path).and_then(|directory| directory.sync_all())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn sync_directory(path: &Path) -> Result<(), RezkaSessionStoreError> {
    sync_directory_io(path).map_err(|_| RezkaSessionStoreError::WriteFailed)
}

#[cfg(test)]
mod tests {
    use std::{fs::OpenOptions, io::Cursor};

    use fs2::FileExt as _;
    use rezka_client::{SessionSnapshot, session::cookie::SessionJar};
    use secrecy::SecretBox;
    use url::Url;

    use super::{
        EncryptedRezkaSessionStore, MAX_ENVELOPE_BYTES, RezkaSessionStoreConfig,
        RezkaSessionStoreError, acquire_rezka_session_lock, persist_envelope_with_ops,
        quarantine_file_with_ops, read_bounded,
    };

    #[test]
    fn bounded_reader_detects_one_byte_overflow() {
        let input = vec![0_u8; MAX_ENVELOPE_BYTES + 1];

        let error = read_bounded(Cursor::new(input), 0).unwrap_err();

        assert_eq!(error, RezkaSessionStoreError::InvalidEnvelope);
    }

    #[test]
    fn encrypted_snapshot_round_trips_and_corruption_is_retained() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        let store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
            path: path.clone(),
            key: SecretBox::new(Box::new([7_u8; 32])),
        })
        .unwrap();
        let origin = Url::parse("https://rezka.test/").unwrap();
        let mut jar = SessionJar::empty();
        jar.store_response_cookies(["provider_state=opaque; Path=/"].into_iter(), &origin);
        let snapshot = jar.export().unwrap();

        store.save(&snapshot).unwrap();
        let restored = store.load().unwrap().unwrap();
        assert!(restored.secret_eq(&snapshot));

        std::fs::write(&path, b"corrupt encrypted snapshot").unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            store.load().unwrap_err(),
            RezkaSessionStoreError::InvalidEnvelope
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn lifecycle_lock_is_exclusive_and_releases_with_guard() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        let guard = acquire_rezka_session_lock(&path).unwrap();
        let lock_path = directory.path().join("session.bin.lock");
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert!(contender.try_lock_exclusive().is_err());
        drop(guard);
        contender.try_lock_exclusive().unwrap();
    }

    #[test]
    fn replacement_failure_preserves_the_previous_snapshot_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        let previous = b"previous-encrypted-envelope";
        std::fs::write(&path, previous).unwrap();

        let error = persist_envelope_with_ops(
            &path,
            b"new-encrypted-envelope",
            |_temporary, _destination| Err(RezkaSessionStoreError::WriteFailed),
            |_parent| Ok(()),
        )
        .unwrap_err();

        assert_eq!(error, RezkaSessionStoreError::WriteFailed);
        assert_eq!(std::fs::read(&path).unwrap(), previous);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn directory_sync_failure_restores_the_previous_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        let previous = b"previous-encrypted-envelope";
        std::fs::write(&path, previous).unwrap();
        let mut sync_calls = 0;

        let error = persist_envelope_with_ops(
            &path,
            b"new-encrypted-envelope",
            |temporary, destination| {
                temporary
                    .persist(destination)
                    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
                Ok(())
            },
            |_parent| {
                sync_calls += 1;
                (sync_calls > 1)
                    .then_some(())
                    .ok_or(RezkaSessionStoreError::WriteFailed)
            },
        )
        .unwrap_err();

        assert_eq!(error, RezkaSessionStoreError::WriteFailed);
        assert_eq!(sync_calls, 2);
        assert_eq!(std::fs::read(&path).unwrap(), previous);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_snapshot_is_retained_until_explicit_quarantine_action() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        let store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
            path: path.clone(),
            key: SecretBox::new(Box::new([7_u8; 32])),
        })
        .unwrap();
        std::fs::write(&path, b"corrupt snapshot bytes").unwrap();

        let retained = std::fs::read(&path).unwrap();
        assert_eq!(
            store.load().unwrap_err(),
            RezkaSessionStoreError::InvalidEnvelope
        );
        assert_eq!(std::fs::read(&path).unwrap(), retained);
        assert_eq!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            1
        );

        let quarantined = store.quarantine_corrupt_snapshot().unwrap();

        assert!(!path.exists());
        assert!(quarantined.starts_with(directory.path()));
        assert!(
            quarantined
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("session.bin.corrupt-")
        );
        assert_eq!(std::fs::read(quarantined).unwrap(), retained);
    }

    #[test]
    fn healthy_snapshot_is_never_quarantined_by_recovery_action() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        let store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
            path: path.clone(),
            key: SecretBox::new(Box::new([7_u8; 32])),
        })
        .unwrap();
        let snapshot = SessionSnapshot::from_secret_bytes(SecretBox::new(Box::new(vec![0x44; 64])));
        store.save(&snapshot).unwrap();
        let before = std::fs::read(&path).unwrap();

        assert_eq!(
            store.quarantine_corrupt_snapshot().unwrap_err(),
            RezkaSessionStoreError::SnapshotHealthy
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("session.bin.corrupt-")
                })
                .count(),
            0
        );
    }

    #[test]
    fn quarantine_rename_failure_preserves_the_original_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("session.bin");
        let destination = directory.path().join("session.bin.corrupt-123-000000000-0");
        let original = b"corrupt snapshot bytes";
        std::fs::write(&source, original).unwrap();

        let error = quarantine_file_with_ops(
            &source,
            &destination,
            |_source, _destination| Err(std::io::Error::other("simulated rename failure")),
            |_parent| Ok(()),
        )
        .unwrap_err();

        assert_eq!(error, RezkaSessionStoreError::QuarantineFailed);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(!destination.exists());
    }

    #[test]
    fn quarantine_directory_sync_failure_restores_the_original_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("session.bin");
        let destination = directory.path().join("session.bin.corrupt-123-000000000-0");
        let original = b"corrupt snapshot bytes";
        std::fs::write(&source, original).unwrap();
        let mut sync_calls = 0;

        let error = quarantine_file_with_ops(
            &source,
            &destination,
            |from, to| std::fs::rename(from, to),
            |_parent| {
                sync_calls += 1;
                (sync_calls > 1)
                    .then_some(())
                    .ok_or_else(|| std::io::Error::other("simulated fsync failure"))
            },
        )
        .unwrap_err();

        assert_eq!(error, RezkaSessionStoreError::QuarantineFailed);
        assert_eq!(sync_calls, 2);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(!destination.exists());
    }
}
