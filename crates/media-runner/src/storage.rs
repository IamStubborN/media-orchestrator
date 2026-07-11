pub const GIB: u64 = 1024 * 1024 * 1024;

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StorageRoots {
    staging: PathBuf,
    tv: PathBuf,
    movies: PathBuf,
}

impl StorageRoots {
    pub fn new(
        staging: impl Into<PathBuf>,
        tv: impl Into<PathBuf>,
        movies: impl Into<PathBuf>,
    ) -> Result<Self, StorageRootsError> {
        let roots = Self {
            staging: staging.into(),
            tv: tv.into(),
            movies: movies.into(),
        };
        let paths = [&roots.staging, &roots.tv, &roots.movies];
        if paths.iter().any(|path| !valid_root(path))
            || paths.iter().enumerate().any(|(index, left)| {
                paths
                    .iter()
                    .skip(index + 1)
                    .any(|right| left.starts_with(right) || right.starts_with(left))
            })
        {
            return Err(StorageRootsError);
        }
        Ok(roots)
    }

    #[must_use]
    pub fn staging(&self) -> &Path {
        &self.staging
    }

    #[must_use]
    pub fn tv(&self) -> &Path {
        &self.tv
    }

    #[must_use]
    pub fn movies(&self) -> &Path {
        &self.movies
    }
}

fn valid_root(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("runner storage roots are invalid")]
pub struct StorageRootsError;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PeakEstimate {
    bytes: u64,
}

impl PeakEstimate {
    pub fn new(
        download_bytes: u64,
        transcode_bytes: u64,
        publication_bytes: u64,
    ) -> Result<Self, StorageBlocked> {
        let bytes = download_bytes
            .checked_add(transcode_bytes)
            .and_then(|value| value.checked_add(publication_bytes))
            .ok_or(StorageBlocked {
                available_bytes: 0,
                required_bytes: u64::MAX,
            })?;
        Ok(Self { bytes })
    }

    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct StoragePreflight {
    reserve_bytes: u64,
}

impl StoragePreflight {
    #[must_use]
    pub const fn new(reserve_bytes: u64) -> Self {
        Self { reserve_bytes }
    }

    pub fn check(self, available_bytes: u64, estimate: PeakEstimate) -> Result<(), StorageBlocked> {
        let required_bytes =
            estimate
                .bytes()
                .checked_add(self.reserve_bytes)
                .ok_or(StorageBlocked {
                    available_bytes,
                    required_bytes: u64::MAX,
                })?;
        if available_bytes < required_bytes {
            return Err(StorageBlocked {
                available_bytes,
                required_bytes,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("insufficient storage for operation and configured reserve")]
pub struct StorageBlocked {
    available_bytes: u64,
    required_bytes: u64,
}

impl StorageBlocked {
    #[must_use]
    pub const fn available_bytes(self) -> u64 {
        self.available_bytes
    }

    #[must_use]
    pub const fn required_bytes(self) -> u64 {
        self.required_bytes
    }
}
