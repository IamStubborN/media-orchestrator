pub const MAX_STICKY_VPN_ATTEMPTS: u32 = 3;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RunnerLifecycleState {
    Ready,
    Rotating,
    Blocked,
}

impl RunnerLifecycleState {
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Rotating => "rotating",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunnerLifecycle {
    pub state: RunnerLifecycleState,
    pub reason: Option<String>,
    pub previous_ip: Option<String>,
    pub current_ip: Option<String>,
    pub updated_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunnerLifecycleUpdate {
    pub state: RunnerLifecycleState,
    pub reason: Option<String>,
    pub previous_ip: Option<String>,
    pub current_ip: Option<String>,
}
