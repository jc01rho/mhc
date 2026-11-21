//! Port of senpi `packages/coding-agent/src/core/session-archived.ts` (senpi#6677, senpi#6768).

use serde_json::{Map, Value, json};

use crate::session_manager::SessionManager;

pub const SESSION_ARCHIVED_CUSTOM_TYPE: &str = "session_archived";

pub fn session_marker_data<'a>(
    entry: &'a Value,
    custom_type: &str,
    session_id: &str,
) -> Option<&'a Map<String, Value>> {
    if entry.get("type").and_then(Value::as_str) != Some("custom")
        || entry.get("customType").and_then(Value::as_str) != Some(custom_type)
    {
        return None;
    }
    let data = entry.get("data").and_then(Value::as_object)?;
    (data.get("sessionId").and_then(Value::as_str) == Some(session_id)).then_some(data)
}

pub fn archived_flag_of(entry: &Value, session_id: &str) -> Option<bool> {
    let data = session_marker_data(entry, SESSION_ARCHIVED_CUSTOM_TYPE, session_id)?;
    Some(data.get("archived").and_then(Value::as_bool) == Some(true))
}

pub fn is_session_archived(manager: &SessionManager) -> bool {
    manager.is_archived()
}

pub fn set_session_archived(manager: &mut SessionManager, archived: bool) -> Value {
    let session_id = manager.session_id().to_owned();
    manager.append_custom(
        SESSION_ARCHIVED_CUSTOM_TYPE,
        Some(json!({
            "archived": archived,
            "sessionId": session_id,
        })),
    )
}
