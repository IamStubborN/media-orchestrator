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

/// How a runner stage failure should interact with the sticky VPN session.
///
/// Transient stream errors must retry on the same IP (with backoff) and must
/// not burn `sticky_attempt_count` into `rotating`. Clear geo / Anubis /
/// provider-reject signals are rotate-worthy.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum VpnFailureClass {
    /// Connection reset, HLS fragment timeout, SourceTransferTransient-like.
    RetrySameIp,
    /// IP blocked / RezkaReject / Anubis / clear geo ban.
    RotateWorthy,
}

impl VpnFailureClass {
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::RetrySameIp => "retry_same_ip",
            Self::RotateWorthy => "rotate_worthy",
        }
    }
}

/// Classify a stage `error_code` for sticky-VPN / rotate decisions.
#[must_use]
pub fn classify_vpn_failure(error_code: &str) -> VpnFailureClass {
    match error_code {
        "rezka_provider_rejected"
        | "anubis_challenge_required"
        | "anubis_challenge_failed"
        | "source_transfer_rejected"
        | "ip_blocked"
        | "geo_blocked"
        | "geo_ban" => VpnFailureClass::RotateWorthy,
        // Transient transfer / stream / generic execution — keep the sticky IP.
        "source_transfer_transient"
        | "stream_expired"
        | "execution_failed"
        | "runner_service_unavailable"
        | "connection_reset"
        | "hls_fragment_timeout"
        | "rezka_reachable"
        | "rezka_parser_invalid"
        | "session_store_error"
        | "rezka_premium_required" => VpnFailureClass::RetrySameIp,
        // Unknown codes: prefer same-IP retry to avoid rotate storms.
        _ => VpnFailureClass::RetrySameIp,
    }
}

#[cfg(test)]
mod tests {
    use super::{VpnFailureClass, classify_vpn_failure};

    #[test]
    fn classifies_rotate_worthy_signals() {
        for code in [
            "rezka_provider_rejected",
            "anubis_challenge_required",
            "anubis_challenge_failed",
            "source_transfer_rejected",
            "ip_blocked",
            "geo_blocked",
        ] {
            assert_eq!(classify_vpn_failure(code), VpnFailureClass::RotateWorthy);
        }
    }

    #[test]
    fn classifies_transient_as_retry_same_ip() {
        for code in [
            "source_transfer_transient",
            "stream_expired",
            "execution_failed",
            "connection_reset",
            "hls_fragment_timeout",
        ] {
            assert_eq!(classify_vpn_failure(code), VpnFailureClass::RetrySameIp);
        }
    }
}
