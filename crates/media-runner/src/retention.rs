use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

const TERMINAL_MARKER: &str = ".terminal-at";
const TERMINAL_MARKER_TEMP: &str = ".terminal-at.tmp";

#[derive(Debug, Error)]
pub enum StagingRetentionError {
    #[error("staging retention filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("system clock is before the Unix epoch")]
    Clock,
}

pub async fn mark_terminal(
    staging_root: &Path,
    job_id: &str,
    now: SystemTime,
) -> Result<(), StagingRetentionError> {
    let job_directory = staging_root.join(job_id);
    if !job_directory.is_dir() {
        return Ok(());
    }
    let timestamp = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StagingRetentionError::Clock)?
        .as_secs()
        .to_string();
    let temporary = job_directory.join(TERMINAL_MARKER_TEMP);
    tokio::fs::write(&temporary, timestamp).await?;
    tokio::fs::rename(temporary, job_directory.join(TERMINAL_MARKER)).await?;
    Ok(())
}

/// Age-based sweep for staging directories that were never stamped terminal.
///
/// The terminal-stamp fast path ([`cleanup_terminal_staging`]) only removes
/// directories a runner explicitly retired on a completed or terminal outcome.
/// Non-terminal outcomes (PlexPending, BlockedStorage, plex-mismatch, retryable
/// failures) and permanently-dead runners never stamp their staging, so those
/// directories would otherwise leak forever. This sweep removes any staging
/// directory whose most recent activity — the newest mtime across the directory
/// and everything under it — is older than `orphan_after`, while never touching a
/// protected (currently leased) job or a directory that is still being written.
///
/// `orphan_after` should be generous (well beyond the longest a job can run) so
/// an active download or transcode is never mistaken for an orphan; because the
/// window is measured against the newest descendant mtime, an in-progress job
/// whose partials are still growing is always kept.
pub async fn cleanup_orphan_staging(
    staging_root: &Path,
    now: SystemTime,
    orphan_after: Duration,
    protected_job_ids: &[&str],
) -> Result<Vec<PathBuf>, StagingRetentionError> {
    let mut removed = Vec::new();
    let mut entries = match tokio::fs::read_dir(staging_root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(removed),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| protected_job_ids.contains(&name))
        {
            continue;
        }
        let last_activity = latest_mtime(&entry.path()).await?;
        // A future mtime (clock skew) reads as no elapsed age, so the directory is
        // kept rather than deleted.
        if now.duration_since(last_activity).unwrap_or_default() < orphan_after {
            continue;
        }
        tokio::fs::remove_dir_all(entry.path()).await?;
        removed.push(entry.path());
    }
    Ok(removed)
}

/// Newest modification time across `root` and every descendant, without following
/// symlinks. Used to decide whether a staging directory is still active.
async fn latest_mtime(root: &Path) -> Result<SystemTime, StagingRetentionError> {
    let mut newest = tokio::fs::symlink_metadata(root).await?.modified()?;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            // `DirEntry::metadata` does not traverse symlinks, so a symlinked
            // directory contributes its own mtime and is not descended into.
            let metadata = entry.metadata().await?;
            let modified = metadata.modified()?;
            if modified > newest {
                newest = modified;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Ok(newest)
}

pub async fn cleanup_terminal_staging(
    staging_root: &Path,
    now: SystemTime,
    retention: Duration,
    protected_job_ids: &[&str],
) -> Result<Vec<PathBuf>, StagingRetentionError> {
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StagingRetentionError::Clock)?;
    let mut removed = Vec::new();
    let mut entries = match tokio::fs::read_dir(staging_root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(removed),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| protected_job_ids.contains(&name))
        {
            continue;
        }
        let marker = match tokio::fs::read_to_string(entry.path().join(TERMINAL_MARKER)).await {
            Ok(marker) => marker,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let Ok(seconds) = marker.trim().parse::<u64>() else {
            continue;
        };
        let terminal_at = Duration::from_secs(seconds);
        if now.saturating_sub(terminal_at) < retention {
            continue;
        }
        tokio::fs::remove_dir_all(entry.path()).await?;
        removed.push(entry.path());
    }
    Ok(removed)
}
