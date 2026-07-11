#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct OperationKey([u8; 32]);

impl OperationKey {
    #[must_use]
    pub const fn from_bytes(value: [u8; 32]) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for OperationKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OperationKey([REDACTED])")
    }
}
