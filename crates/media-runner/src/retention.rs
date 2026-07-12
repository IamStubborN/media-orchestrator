use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

const TERMINAL_MARKER: &str = ".terminal-at";

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
    tokio::fs::write(job_directory.join(TERMINAL_MARKER), timestamp).await?;
    Ok(())
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
