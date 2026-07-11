pub const GIB: u64 = 1024 * 1024 * 1024;

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
