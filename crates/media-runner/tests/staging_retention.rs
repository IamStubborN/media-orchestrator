use std::time::{Duration, SystemTime, UNIX_EPOCH};

use media_runner::{cleanup_orphan_staging, cleanup_terminal_staging, mark_terminal};

#[tokio::test]
async fn cleanup_removes_only_marked_terminal_directories_after_retention() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old");
    let recent = root.path().join("recent");
    let active = root.path().join("active");
    tokio::fs::create_dir_all(&old).await.unwrap();
    tokio::fs::create_dir_all(&recent).await.unwrap();
    tokio::fs::create_dir_all(&active).await.unwrap();
    tokio::fs::write(old.join("partial"), "keep until expiry")
        .await
        .unwrap();

    mark_terminal(root.path(), "old", UNIX_EPOCH + Duration::from_secs(10))
        .await
        .unwrap();
    assert!(!old.join(".terminal-at.tmp").exists());
    mark_terminal(root.path(), "recent", UNIX_EPOCH + Duration::from_secs(100))
        .await
        .unwrap();

    let removed = cleanup_terminal_staging(
        root.path(),
        UNIX_EPOCH + Duration::from_secs(110),
        Duration::from_secs(50),
        &[],
    )
    .await
    .unwrap();

    assert_eq!(removed, vec![old]);
    assert!(!root.path().join("old").exists());
    assert!(recent.exists());
    assert!(active.exists());
}

#[tokio::test]
async fn cleanup_ignores_invalid_markers_files_and_missing_roots() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(root.path().join("invalid"))
        .await
        .unwrap();
    tokio::fs::write(root.path().join("invalid/.terminal-at"), "not-a-time")
        .await
        .unwrap();
    tokio::fs::write(root.path().join("ordinary-file"), "data")
        .await
        .unwrap();

    assert!(
        cleanup_terminal_staging(
            root.path(),
            UNIX_EPOCH + Duration::from_secs(100),
            Duration::from_secs(1),
            &[],
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(
        cleanup_terminal_staging(
            &root.path().join("missing"),
            UNIX_EPOCH + Duration::from_secs(100),
            Duration::from_secs(1),
            &[],
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn cleanup_never_removes_a_protected_leased_job() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(root.path().join("leased"))
        .await
        .unwrap();
    mark_terminal(root.path(), "leased", UNIX_EPOCH + Duration::from_secs(1))
        .await
        .unwrap();

    let removed = cleanup_terminal_staging(
        root.path(),
        UNIX_EPOCH + Duration::from_secs(100),
        Duration::from_secs(1),
        &["leased"],
    )
    .await
    .unwrap();

    assert!(removed.is_empty());
    assert!(root.path().join("leased").is_dir());
}

#[tokio::test]
async fn orphan_sweep_removes_stale_unstamped_directories_but_keeps_active_and_protected() {
    let root = tempfile::tempdir().unwrap();
    let orphan = root.path().join("orphan");
    let protected = root.path().join("protected");
    let active = root.path().join("active");

    // The orphan (a non-terminal outcome that never stamped) and the protected
    // (currently leased) job are written first, so their newest mtime is old.
    for directory in [&orphan, &protected] {
        tokio::fs::create_dir_all(directory.join("s01e01"))
            .await
            .unwrap();
        tokio::fs::write(directory.join("s01e01/encoded.partial.mkv"), b"data")
            .await
            .unwrap();
    }
    // The active job is written last, so it is still fresh relative to the window.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    tokio::fs::create_dir_all(active.join("s01e01"))
        .await
        .unwrap();
    tokio::fs::write(active.join("s01e01/encoded.partial.mkv"), b"data")
        .await
        .unwrap();

    let removed = cleanup_orphan_staging(
        root.path(),
        SystemTime::now(),
        Duration::from_secs(1),
        &["protected"],
    )
    .await
    .unwrap();

    assert_eq!(removed, vec![orphan.clone()]);
    assert!(!orphan.exists());
    assert!(protected.is_dir(), "a leased job is never swept");
    assert!(
        active.is_dir(),
        "a directory still being written is never swept"
    );
}
