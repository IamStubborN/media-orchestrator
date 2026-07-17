#![forbid(unsafe_code)]

mod adapters;
mod download;
mod media;
mod pipeline;
mod plex;
mod ports;
mod retention;
pub mod rezka_session_store;
mod storage;
mod subtitle;

pub use adapters::{
    HttpRunnerServiceAdapter, ReqwestHttpAdapter, TokioFileSystem, TokioProcessAdapter,
};
pub use download::{ResumeAction, ResumeError, decide_resume};
pub use media::{
    AudioTrackMetadata, MediaProbe, MediaProbeError, ProcessCommand, build_hls_ingest_command,
    build_rezka_vaapi_command,
};
pub use pipeline::{
    EpisodeOutcome, EpisodePipeline, EpisodeWork, ProviderKind, SensitiveUrl, SensitiveUrlError,
    SubtitleTrack, VideoSourceKind,
};
pub use plex::{PlexExpectation, PlexMismatch, PlexObservation, validate_plex_observation};
pub use ports::{
    Cancellation, FileSystemPort, HttpPort, PlexCheck, ProcessPort, RunnerPortError,
    RunnerServicePort, StageReporter, TransferObservation, TransferSource,
};

pub use retention::{
    StagingRetentionError, cleanup_orphan_staging, cleanup_terminal_staging, mark_terminal,
};
pub use rezka_session_store::{
    EncryptedRezkaSessionStore, RezkaSessionStoreConfig, RezkaSessionStoreError,
};
pub use storage::{
    GIB, PeakEstimate, StorageBlocked, StoragePreflight, StorageRoots, StorageRootsError,
};
pub use subtitle::{SubtitleValidationError, validate_webvtt};
