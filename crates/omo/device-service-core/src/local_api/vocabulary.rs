//! Port of `local-api/vocabulary.ts`: the value lists of the local API v1 vocabulary.

pub const LOCAL_API_VERSION: u64 = 1;

pub const STATUS_STATES: [&str; 14] = [
    "not_configured",
    "disabled",
    "sign_in_required",
    "plan_required",
    "keychain_locked",
    "connecting",
    "activation_pending",
    "connected",
    "offline",
    "authorization_stale",
    "service_degraded",
    "update_required",
    "signed_out",
    "fail_closed",
];

pub const CLIENT_NAMES: [&str; 4] = ["desktop", "omo", "installer", "sdk"];
pub const CAPABILITIES: [&str; 5] = ["sessions", "devices", "enforcement", "upgrade", "control"];
pub const REFUSED_REASONS: [&str; 3] = ["version", "data_home_required", "malformed"];

pub const ROUND_STATES: [&str; 2] = ["open", "done"];
pub const ROUND_DEVICE_STATES: [&str; 7] = [
    "answered",
    "pending",
    "undelivered",
    "page_cap",
    "cursor_echo",
    "timed_out",
    "skipped",
];
pub const LISTING_SOURCES: [&str; 2] = ["live", "cache"];
pub const SESSION_ROW_STATES: [&str; 3] = ["running", "idle", "ended"];
pub const FILTER_KEYS: [&str; 3] = ["device", "project", "state"];

/// A command_id is exactly this many base64url characters.
pub const COMMAND_ID_CHARS: usize = 22;
pub const COMMAND_STATES: [&str; 9] = [
    "queued",
    "transport_confirmed",
    "not confirmed",
    "not delivered: never_sent",
    "engine_dispatched",
    "engine_applied",
    "engine_refused",
    "engine_stale",
    "uncertain",
];
/// Only a retry by the sender can help, and only when the recipient provably cannot hold the command.
pub const RETRYABLE_STATES: [&str; 2] = ["not confirmed", "not delivered: never_sent"];

pub fn is_terminal_command_state(state: &str) -> bool {
    matches!(
        state,
        "not confirmed"
            | "not delivered: never_sent"
            | "engine_applied"
            | "engine_refused"
            | "engine_stale"
            | "uncertain"
    )
}

pub const ERROR_CODES: [&str; 32] = [
    "unauthenticated",
    "client_proof_invalid",
    "invalid_request",
    "unknown_type",
    "forbidden",
    "owner_transfer",
    "internal",
    "signed_out",
    "device_mismatch",
    "device_changed",
    "account_changed",
    "device_key_invalid",
    "service_disabled",
    "sign_in_required",
    "plan_required",
    "activation_pending",
    "authorization_stale",
    "peer_not_found",
    "peer_not_session_client",
    "session_not_found",
    "session_ambiguous",
    "engine_too_old",
    "not_attached",
    "command_id_conflict",
    "message_too_long",
    "upload_incomplete",
    "malformed",
    "method_unclassified",
    "method_local_only",
    "host_not_answering",
    "unsupported",
    "unknown_delivery",
];

/// Errors the client library raises itself, before or instead of an answer.
pub const CLIENT_ERROR_CODES: [&str; 9] = [
    "insecure_state_dir",
    "server_proof_invalid",
    "refused",
    "protocol_violation",
    "closed",
    "message_too_long",
    "read_failed",
    "handshake_timeout",
    "request_timeout",
];

/// The local API codes that refuse before anything is sent to the target device (omo#13483): a
/// caller may say "not sent" for these and only these.
pub const REFUSED_BEFORE_SENDING: [&str; 14] = [
    "unauthenticated",
    "invalid_request",
    "unknown_type",
    "forbidden",
    "signed_out",
    "service_disabled",
    "sign_in_required",
    "plan_required",
    "activation_pending",
    "peer_not_found",
    "session_not_found",
    "not_attached",
    "message_too_long",
    "unsupported",
];
