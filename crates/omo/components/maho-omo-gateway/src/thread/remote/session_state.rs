//! Remote session state helpers and detach/refusal classifiers (omo `thread/remote/session-state.ts`).

use serde_json::{Map, Value};

use crate::thread::remote::ports::RemoteSessionState;

pub const ACCESS_ENDED: [&str; 4] = [
    "authorization_expired",
    "peer_revoked",
    "owner_transfer",
    "service_disabled",
];

pub const ACCESS_REFUSED: [&str; 13] = [
    "unauthenticated",
    "forbidden",
    "owner_transfer",
    "signed_out",
    "service_disabled",
    "sign_in_required",
    "plan_required",
    "peer_not_found",
    "peer_not_session_client",
    "account_changed",
    "device_changed",
    "capability_missing",
    "engine_too_old",
];

pub const SESSION_LEFT: [&str; 2] = ["session_closed", "session_replaced"];

pub const SESSION_GONE: [&str; 3] = [
    "session_not_found",
    "unknown_session",
    "session_identity_mismatch",
];

pub fn non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub fn session_state_of(session_id: &str, data: &Map<String, Value>) -> RemoteSessionState {
    let status = if data.get("isCompacting").and_then(Value::as_bool) == Some(true) {
        "compacting"
    } else if data
        .get("pendingQuestions")
        .and_then(Value::as_array)
        .is_some_and(|q| !q.is_empty())
    {
        "waiting_input"
    } else if data.get("isStreaming").and_then(Value::as_bool) == Some(true) {
        "running"
    } else {
        "idle"
    };
    RemoteSessionState {
        status: status.to_owned(),
        session_id: session_id.to_owned(),
        durable_session_id: non_empty(data.get("durableSessionId")),
        current_turn_id: non_empty(data.get("currentTurnId")),
    }
}

pub fn status_of(record: &Value, saw_status_record: bool) -> Option<String> {
    let record_type = record.get("type").and_then(Value::as_str)?;
    if record_type == "session_status" {
        return non_empty(record.get("status"));
    }
    if saw_status_record {
        return None;
    }
    match record_type {
        "agent_start" => Some("running".to_owned()),
        "agent_settled" => Some("idle".to_owned()),
        _ => None,
    }
}

pub fn cause_of(record: &Value) -> Option<String> {
    non_empty(record.get("lastChange")?.get("cause"))
}
