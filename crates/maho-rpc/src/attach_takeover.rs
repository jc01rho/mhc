use crate::attach_ledger::AttachHolder;
use serde_json::{Value, json};

pub const RPC_ERROR_TAKEOVER_UNSUPPORTED: &str = "takeover_unsupported";
pub const RPC_ERROR_TAKEOVER_HOLDER_REQUIRED: &str = "takeover_holder_required";
pub const RPC_ERROR_TAKEOVER_FAILED: &str = "takeover_failed";

pub fn takeover_unsupported_detail() -> Value {
    json!({
        "reason": "only a headless engine with a takeover ledger takes sessions over"
    })
}

pub fn session_released_record(session_id: &str) -> Value {
    json!({
        "type": "session_released",
        "sessionId": session_id,
        "reason": "taken_over",
    })
}

pub fn takeover_reply(session_id: &str, record: &AttachHolder, released: usize) -> Value {
    json!({
        "sessionId": session_id,
        "attached": true,
        "holder": record.holder,
        "generation": record.generation,
        "released": released,
    })
}
