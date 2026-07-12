use std::time::{Duration, UNIX_EPOCH};

use media_runner::{cleanup_terminal_staging, mark_terminal};

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
