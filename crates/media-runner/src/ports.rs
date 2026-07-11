use std::path::Path;

use async_trait::async_trait;

use crate::{MediaProbe, PlexExpectation, PlexObservation, ProcessCommand, SensitiveUrl};

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum RunnerPortError {
    #[error("filesystem operation failed")]
    Filesystem,
    #[error("HTTP transfer failed")]
    Http,
    #[error("media process failed")]
    Process,
    #[error("runner service request failed")]
    Service,
    #[error("operation cancelled")]
    Cancelled,
    #[error("runner work item is invalid")]
    InvalidWork,
}

pub trait Cancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

#[async_trait]
pub trait FileSystemPort: Send + Sync {
    async fn available_bytes(&self, path: &Path) -> Result<u64, RunnerPortError>;
    async fn file_len(&self, path: &Path) -> Result<Option<u64>, RunnerPortError>;
    async fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, RunnerPortError>;
    async fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), RunnerPortError>;
    async fn write_chunk(
        &self,
        path: &Path,
        offset: u64,
        contents: &[u8],
    ) -> Result<(), RunnerPortError>;
    async fn create_dir_all(&self, path: &Path) -> Result<(), RunnerPortError>;
    async fn publish_atomic(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), RunnerPortError>;
    async fn replace_atomic(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), RunnerPortError>;
}

#[async_trait]
pub trait HttpPort: Send + Sync {
    async fn download_video(
        &self,
        url: &SensitiveUrl,
        partial_path: &Path,
        resume_from: u64,
        filesystem: &dyn FileSystemPort,
        cancellation: &dyn Cancellation,
    ) -> Result<(), RunnerPortError>;

    async fn fetch_subtitle(
        &self,
        url: &SensitiveUrl,
        cancellation: &dyn Cancellation,
    ) -> Result<Vec<u8>, RunnerPortError>;
}

#[async_trait]
pub trait ProcessPort: Send + Sync {
    async fn probe(
        &self,
        path: &Path,
        cancellation: &dyn Cancellation,
    ) -> Result<MediaProbe, RunnerPortError>;

    async fn run(
        &self,
        command: &ProcessCommand,
        cancellation: &dyn Cancellation,
    ) -> Result<(), RunnerPortError>;
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PlexCheck {
    Matched(PlexObservation),
    Pending,
    Mismatch,
}

#[async_trait]
pub trait RunnerServicePort: Send + Sync {
    async fn scan_and_verify(
        &self,
        expectation: &PlexExpectation,
    ) -> Result<PlexCheck, RunnerPortError>;
}
