use std::{io::Write as _, path::PathBuf};

#[cfg(unix)]
use std::time::{Duration, Instant};

use aes_gcm::{
    Aes256Gcm,
    aead::{Aead as _, KeyInit as _, Payload},
};
use base64::Engine as _;
use media_runner::{EncryptedRezkaSessionStore, RezkaSessionStoreConfig, RezkaSessionStoreError};
use rezka_client::SessionSnapshot;
use secrecy::SecretBox;
use tempfile::TempDir;

fn key(byte: u8) -> SecretBox<[u8; 32]> {
    SecretBox::new(Box::new([byte; 32]))
}

fn snapshot(byte: u8) -> SessionSnapshot {
    snapshot_with_len(byte, 64)
}

fn snapshot_with_len(byte: u8, len: usize) -> SessionSnapshot {
    SessionSnapshot::from_secret_bytes(SecretBox::new(Box::new(vec![byte; len])))
}

fn serialized_envelope_len(plaintext_len: usize) -> usize {
    const JSON_WITH_EMPTY_VALUES_LEN: usize =
        br#"{"version":1,"nonce_b64":"","ciphertext_b64":""}"#.len();
    const NONCE_B64_LEN: usize = 16;
    const TAG_LEN: usize = 16;

    JSON_WITH_EMPTY_VALUES_LEN + NONCE_B64_LEN + (plaintext_len + TAG_LEN).div_ceil(3) * 4
}

fn max_plaintext_len() -> usize {
    const MAX_ENVELOPE_BYTES: usize = 256 * 1024;
    (0..=MAX_ENVELOPE_BYTES)
        .rev()
        .find(|len| serialized_envelope_len(*len) <= MAX_ENVELOPE_BYTES)
        .unwrap()
}

fn store(path: PathBuf, key_byte: u8) -> EncryptedRezkaSessionStore {
    EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path,
        key: key(key_byte),
    })
    .unwrap()
}

#[test]
fn encrypted_store_round_trips_without_plaintext_on_disk() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);

    let original = snapshot(0x11);
    store.save(&original).unwrap();
    let disk = std::fs::read_to_string(&path).unwrap();

    assert!(disk.contains("\"version\":1"));
    assert!(disk.contains("\"nonce_b64\":"));
    assert!(disk.contains("\"ciphertext_b64\":"));
    assert!(!disk.contains(&base64::engine::general_purpose::STANDARD.encode(vec![0x11; 64])));

    let envelope: serde_json::Value = serde_json::from_str(&disk).unwrap();
    let nonce: [u8; 12] = base64::engine::general_purpose::STANDARD
        .decode(envelope["nonce_b64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(envelope["ciphertext_b64"].as_str().unwrap())
        .unwrap();
    let cipher = Aes256Gcm::new_from_slice(&[7; 32]).unwrap();
    let plaintext = cipher
        .decrypt(
            &nonce.into(),
            Payload {
                msg: &ciphertext,
                aad: b"media-orchestrator:rezka-session:v1",
            },
        )
        .unwrap();
    assert_eq!(plaintext, vec![0x11; 64]);

    let restored = store.load().unwrap().unwrap();
    assert!(restored.secret_eq(&original));
}

#[test]
fn repeated_saves_use_distinct_envelopes_and_overwrite_with_latest_snapshot() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);

    store.save(&snapshot(0x11)).unwrap();
    let first: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    store.save(&snapshot(0x22)).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

    assert_ne!(first["nonce_b64"], second["nonce_b64"]);
    assert_ne!(first["ciphertext_b64"], second["ciphertext_b64"]);
    assert!(store.load().unwrap().unwrap().secret_eq(&snapshot(0x22)));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn maximum_sized_envelope_round_trips_at_the_calculated_boundary() {
    const MAX_ENVELOPE_BYTES: usize = 256 * 1024;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);
    let plaintext_len = max_plaintext_len();
    assert!(serialized_envelope_len(plaintext_len) <= MAX_ENVELOPE_BYTES);
    assert!(serialized_envelope_len(plaintext_len + 1) > MAX_ENVELOPE_BYTES);

    let original = snapshot_with_len(0x5a, plaintext_len);
    store.save(&original).unwrap();

    assert_eq!(
        std::fs::metadata(&path).unwrap().len() as usize,
        serialized_envelope_len(plaintext_len)
    );
    assert!(store.load().unwrap().unwrap().secret_eq(&original));
}

#[test]
fn oversized_save_is_rejected_without_overwriting_previous_envelope() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);
    let previous = snapshot(0x31);
    store.save(&previous).unwrap();
    let previous_envelope = std::fs::read(&path).unwrap();

    let error = store
        .save(&snapshot_with_len(0x7f, max_plaintext_len() + 1))
        .unwrap_err();

    assert_eq!(error, RezkaSessionStoreError::InvalidEnvelope);
    assert_eq!(std::fs::read(&path).unwrap(), previous_envelope);
    assert!(store.load().unwrap().unwrap().secret_eq(&previous));
}

#[test]
fn every_save_uses_a_fresh_nonce() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);
    let mut nonces = std::collections::BTreeSet::new();

    for _ in 0..32 {
        store.save(&snapshot(0x55)).unwrap();
        let envelope: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(nonces.insert(envelope["nonce_b64"].as_str().unwrap().to_owned()));
    }
}

#[test]
fn missing_load_returns_none_and_missing_delete_succeeds() {
    let dir = TempDir::new().unwrap();
    let store = store(dir.path().join("rezka-session.bin"), 7);

    assert!(store.load().unwrap().is_none());
    store.delete().unwrap();
}

#[test]
fn delete_removes_an_existing_envelope() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);
    store.save(&snapshot(0x11)).unwrap();

    store.delete().unwrap();

    assert!(!path.exists());
    assert!(store.load().unwrap().is_none());
}

#[test]
fn encrypted_store_delete_removes_file_and_missing_delete_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path: path.clone(),
        key: key(7),
    })
    .unwrap();
    store.save(&snapshot(0x55)).unwrap();
    assert!(path.exists());

    store.delete().unwrap();
    assert!(!path.exists());
    store.delete().unwrap();
}

#[test]
fn invalid_path_returns_only_a_sanitized_error() {
    let error = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path: PathBuf::new(),
        key: key(7),
    })
    .unwrap_err();

    assert_eq!(
        format!("{error:?}: {error}"),
        "InvalidPath: encrypted Rezka session path is invalid"
    );
}

#[test]
fn wrong_key_and_corrupt_envelope_return_only_sanitized_errors() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    store(path.clone(), 7).save(&snapshot(0x33)).unwrap();

    let wrong_key_store = store(path.clone(), 8);
    let error = wrong_key_store.load().unwrap_err();
    assert_eq!(
        format!("{error:?}: {error}"),
        "DecryptionFailed: encrypted Rezka session could not be decrypted"
    );

    std::fs::write(&path, b"{not-an-envelope").unwrap();
    let corrupt = wrong_key_store.load().unwrap_err();
    assert_eq!(
        format!("{corrupt:?}: {corrupt}"),
        "InvalidEnvelope: encrypted Rezka session envelope is invalid"
    );
}

#[test]
fn unsupported_version_is_rejected_without_envelope_details() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    std::fs::write(
        &path,
        br#"{"version":2,"nonce_b64":"AAAAAAAAAAAAAAAA","ciphertext_b64":"AA=="}"#,
    )
    .unwrap();

    let error = store(path, 7).load().unwrap_err();

    assert_eq!(
        format!("{error:?}: {error}"),
        "UnsupportedVersion: encrypted Rezka session envelope version is unsupported"
    );
}

#[test]
fn invalid_base64_is_rejected_without_envelope_details() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    std::fs::write(
        &path,
        br#"{"version":1,"nonce_b64":"not base64","ciphertext_b64":"AA=="}"#,
    )
    .unwrap();

    let error = store(path, 7).load().unwrap_err();

    assert_eq!(
        format!("{error:?}: {error}"),
        "InvalidEnvelope: encrypted Rezka session envelope is invalid"
    );
}

#[test]
fn invalid_nonce_length_is_rejected_without_envelope_details() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    std::fs::write(
        &path,
        br#"{"version":1,"nonce_b64":"AA==","ciphertext_b64":"AA=="}"#,
    )
    .unwrap();

    let error = store(path, 7).load().unwrap_err();

    assert_eq!(
        format!("{error:?}: {error}"),
        "InvalidEnvelope: encrypted Rezka session envelope is invalid"
    );
}

#[test]
fn unknown_envelope_fields_are_rejected() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);
    store.save(&snapshot(0x77)).unwrap();
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    envelope["unexpected"] = serde_json::Value::String("ignored".to_owned());
    std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();

    let error = store.load().unwrap_err();

    assert_eq!(error, RezkaSessionStoreError::InvalidEnvelope);
}

#[test]
fn oversized_regular_file_is_rejected() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);
    store.save(&snapshot(0x78)).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(&vec![b' '; 1024 * 1024]).unwrap();

    let error = store.load().unwrap_err();

    assert_eq!(error, RezkaSessionStoreError::InvalidEnvelope);
}

#[test]
fn directory_is_rejected_as_an_invalid_envelope() {
    let dir = TempDir::new().unwrap();

    let error = store(dir.path().to_path_buf(), 7).load().unwrap_err();

    assert_eq!(error, RezkaSessionStoreError::InvalidEnvelope);
}

#[cfg(unix)]
#[test]
fn symlink_is_rejected_instead_of_loading_its_target() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let target = dir.path().join("target.bin");
    store(target.clone(), 7).save(&snapshot(0x79)).unwrap();
    let link = dir.path().join("session-link.bin");
    symlink(&target, &link).unwrap();

    let error = store(link, 7).load().unwrap_err();

    assert_eq!(error, RezkaSessionStoreError::ReadFailed);
}

#[cfg(unix)]
#[test]
fn device_is_rejected_as_an_invalid_envelope() {
    let error = store(PathBuf::from("/dev/null"), 7).load().unwrap_err();

    assert_eq!(error, RezkaSessionStoreError::InvalidEnvelope);
}

#[cfg(unix)]
#[test]
fn fifo_is_rejected_promptly_without_blocking() {
    let dir = TempDir::new().unwrap();
    let fifo = dir.path().join("session.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("fifo_load_helper")
        .arg("--nocapture")
        .env("MEDIA_RUNNER_FIFO_TEST_PATH", &fifo)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);

    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("FIFO load blocked past the watchdog deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn fifo_load_helper() {
    let Some(path) = std::env::var_os("MEDIA_RUNNER_FIFO_TEST_PATH") else {
        return;
    };

    let error = store(PathBuf::from(path), 7).load().unwrap_err();
    assert_eq!(error, RezkaSessionStoreError::InvalidEnvelope);
}

#[test]
fn config_and_store_debug_show_only_the_safe_parent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("secret-file-name.bin");
    let config = RezkaSessionStoreConfig {
        path: path.clone(),
        key: key(0xab),
    };
    let config_debug = format!("{config:?}");

    assert!(config_debug.contains(&format!("{:?}", dir.path())));
    assert!(!config_debug.contains("secret-file-name.bin"));
    assert!(!config_debug.contains("key"));

    let store_debug = format!("{:?}", EncryptedRezkaSessionStore::new(config).unwrap());
    assert!(store_debug.contains(&format!("{:?}", dir.path())));
    assert!(!store_debug.contains("secret-file-name.bin"));
    assert!(!store_debug.contains("key"));
}

#[cfg(unix)]
#[test]
fn encrypted_store_uses_restrictive_unix_permissions_after_overwrite() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = store(path.clone(), 7);

    store.save(&snapshot(0x44)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    store.save(&snapshot(0x45)).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn filesystem_failures_do_not_include_path_details() {
    let dir = TempDir::new().unwrap();
    let missing_parent = dir.path().join("missing-parent");
    let path = missing_parent.join("secret-file-name.bin");
    let store = store(path, 7);

    let error = store.save(&snapshot(0x66)).unwrap_err();

    assert_eq!(error, RezkaSessionStoreError::WriteFailed);
    assert_eq!(
        format!("{error:?}: {error}"),
        "WriteFailed: encrypted Rezka session could not be written"
    );
}
