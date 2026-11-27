//! `runners/rpc-host/session-wire.ts` (v5.1.52 additions): typed input refusals of a hosted session.

use crate::host::HostError;

/// `HostSessionUnresponsiveError`: nothing was sent; the same session can accept input once its
/// state probes recover.
pub const HOST_SESSION_UNRESPONSIVE_CODE: &str = "unresponsive";
pub const HOST_SESSION_UNRESPONSIVE_MESSAGE: &str =
    "the session is unresponsive; input can be retried when it responds";

pub fn host_session_unresponsive_error() -> HostError {
    HostError {
        message: HOST_SESSION_UNRESPONSIVE_MESSAGE.to_string(),
    }
}

/// Whether an input failure is the retryable "unresponsive" refusal.
pub fn is_host_session_unresponsive(error: &HostError) -> bool {
    error.message == HOST_SESSION_UNRESPONSIVE_MESSAGE
}

const HOST_SESSION_DETACHED_PREFIX: &str = "the host session holds no connection";

/// `HostSessionDetachedError(operation)`: a session command, or an open, met a session that holds no
/// transport (detached, parked, closed or lost).
pub fn host_session_detached_error(operation: &str) -> HostError {
    HostError {
        message: format!("{HOST_SESSION_DETACHED_PREFIX} ({operation})"),
    }
}

/// Whether a failure is the typed "holds no connection" refusal.
pub fn is_host_session_detached(error: &HostError) -> bool {
    error.message.starts_with(HOST_SESSION_DETACHED_PREFIX)
}

/// `isHostSessionMissing`: the engine's typed response, including hosts that carry the code only in
/// the error string.
pub fn is_host_session_missing(error_code: Option<&str>, message: &str) -> bool {
    error_code == Some("unknown_session") || message == "unknown_session"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_session_is_read_from_the_code_or_the_bare_message() {
        assert!(is_host_session_missing(Some("unknown_session"), "gone"));
        assert!(is_host_session_missing(None, "unknown_session"));
        assert!(!is_host_session_missing(None, "unknown_session: other"));
        assert!(is_host_session_unresponsive(
            &host_session_unresponsive_error()
        ));
    }

    #[test]
    fn a_session_without_a_connection_is_told_apart_from_an_unresponsive_one() {
        let detached = host_session_detached_error("open_session");
        assert!(is_host_session_detached(&detached));
        assert!(!is_host_session_unresponsive(&detached));
        assert!(!is_host_session_detached(&host_session_unresponsive_error()));
    }
}
