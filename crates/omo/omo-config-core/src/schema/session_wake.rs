//! Port of `schema/session-wake.ts` (omo #13037, shipped in #13563).
//!
//! Waking a session that is not running. `same_device`: a send from this machine to a session
//! here that is not running starts it (default on; `false` is only an explicit opt-out).
//! `wake_limit`: at most this many consecutive wakes of a session nobody attends, `0` (the
//! default) meaning no bound.

use serde_json::Value;

use crate::internal::validate::{Node, boolean, nonnegative_integer, optional, strict_object};

pub fn omo_session_wake_settings_schema() -> Node {
    strict_object(vec![
        optional("same_device", boolean()),
        optional("wake_limit", nonnegative_integer()),
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSessionWakeSettings {
    pub same_device: bool,
    pub wake_limit: u64,
}

/// Reads `session_wake` from a config view (`resolveSessionWakeSettings`).
pub fn resolve_session_wake_settings(config: &Value) -> ResolvedSessionWakeSettings {
    let settings = config.get("session_wake");
    ResolvedSessionWakeSettings {
        same_device: settings
            .and_then(|settings| settings.get("same_device"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        wake_limit: settings
            .and_then(|settings| settings.get("wake_limit"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}
