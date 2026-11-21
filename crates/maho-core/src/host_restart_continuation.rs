//! Port of senpi `packages/coding-agent/src/core/host-restart-continuation.ts` (senpi#7063, senpi#7080).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine_turn_hold::SessionHold;
use crate::engine_turn_limit::{
    ENGINE_TURN_START_ENTRY_TYPE, is_user_directed_turn, resets_engine_turn_history,
};
use crate::stop_resume_directive::STOP_RESUME_CUSTOM_TYPE;

pub const HOST_DRAIN_HOLD_KEY: &str = "host-drain";
pub const INTERRUPTED_RUN_HOLD_KEY: &str = "interrupted-run";
pub const SESSION_CONTROL_DELIVERY_TYPE: &str = "session_control_delivery";
pub const RESTART_CONTINUATION_LIMIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartHoldReason {
    RepeatedRestartNoProgress,
    RepeatedRestartLimit,
}

impl RestartHoldReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepeatedRestartNoProgress => "repeated_restart_no_progress",
            Self::RepeatedRestartLimit => "repeated_restart_limit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RestartVerdict {
    should_continue: bool,
    reason: Option<RestartHoldReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartRelease {
    Resume { interrupted_run: bool },
    Release { keys: Vec<String> },
}

fn is_restart_continuation(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("custom_message")
        && entry.get("customType").and_then(Value::as_str) == Some(STOP_RESUME_CUSTOM_TYPE)
        && entry
            .get("details")
            .and_then(|details| details.get("resumeCause"))
            .and_then(Value::as_str)
            == Some("interrupted")
}

fn is_progress(entry: &Value) -> bool {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return false;
    }
    let Some(message) = entry.get("message") else {
        return false;
    };
    match message.get("role").and_then(Value::as_str) {
        Some("assistant") => matches!(
            message.get("stopReason").and_then(Value::as_str),
            Some("stop" | "length" | "toolUse")
        ),
        Some("toolResult") => message
            .get("details")
            .and_then(|details| details.get("interrupted"))
            .is_none_or(Value::is_null),
        _ => false,
    }
}

fn is_new_input(entry: &Value) -> bool {
    match entry.get("type").and_then(Value::as_str) {
        Some("message") => {
            entry
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                == Some("user")
        }
        Some("custom_message") => {
            let Some(custom_type) = entry.get("customType").and_then(Value::as_str) else {
                return false;
            };
            custom_type == SESSION_CONTROL_DELIVERY_TYPE
                || resets_engine_turn_history(custom_type, entry.get("details"))
        }
        _ => false,
    }
}

pub fn engine_started_last_run(branch: &[Value]) -> bool {
    for entry in branch.iter().rev() {
        match entry.get("type").and_then(Value::as_str) {
            Some("message")
                if entry
                    .get("message")
                    .and_then(|message| message.get("role"))
                    .and_then(Value::as_str)
                    == Some("user") =>
            {
                return false;
            }
            Some("custom_message")
                if entry
                    .get("customType")
                    .and_then(Value::as_str)
                    .is_some_and(|custom_type| {
                        custom_type == STOP_RESUME_CUSTOM_TYPE || is_user_directed_turn(custom_type)
                    }) =>
            {
                return false;
            }
            Some("custom")
                if entry.get("customType").and_then(Value::as_str)
                    == Some(ENGINE_TURN_START_ENTRY_TYPE) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn restart_verdict(branch: &[Value]) -> RestartVerdict {
    let mut continuations = 0usize;
    let mut progressed = false;
    let mut engine_started: Option<bool> = None;
    for entry in branch.iter().rev() {
        if engine_started.is_some() {
            break;
        }
        if continuations == 0 && is_progress(entry) {
            progressed = true;
        }
        if is_restart_continuation(entry) {
            continuations += 1;
        } else if (entry.get("type").and_then(Value::as_str) == Some("message")
            && entry
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                == Some("user"))
            || (entry.get("type").and_then(Value::as_str) == Some("custom_message")
                && entry
                    .get("customType")
                    .and_then(Value::as_str)
                    .is_some_and(is_user_directed_turn))
        {
            engine_started = Some(false);
        } else if entry.get("type").and_then(Value::as_str) == Some("custom")
            && entry.get("customType").and_then(Value::as_str) == Some(ENGINE_TURN_START_ENTRY_TYPE)
        {
            engine_started = Some(true);
        }
    }
    if engine_started != Some(true) {
        return RestartVerdict {
            should_continue: false,
            reason: None,
        };
    }
    if continuations == 0 {
        return RestartVerdict {
            should_continue: true,
            reason: None,
        };
    }
    if !progressed {
        return RestartVerdict {
            should_continue: false,
            reason: Some(RestartHoldReason::RepeatedRestartNoProgress),
        };
    }
    let mut since_input = 0usize;
    for entry in branch.iter().rev() {
        if is_new_input(entry) {
            break;
        }
        if is_restart_continuation(entry) {
            since_input += 1;
        }
    }
    if since_input >= RESTART_CONTINUATION_LIMIT {
        RestartVerdict {
            should_continue: false,
            reason: Some(RestartHoldReason::RepeatedRestartLimit),
        }
    } else {
        RestartVerdict {
            should_continue: true,
            reason: None,
        }
    }
}

pub fn restart_hold_data(continued: bool, branch: &[Value]) -> Option<Value> {
    if !continued {
        return None;
    }
    let verdict = restart_verdict(branch);
    if verdict.should_continue {
        return None;
    }
    verdict
        .reason
        .map(|reason| json!({ "reason": reason.as_str() }))
}

pub fn restart_release(holds: &[SessionHold], branch: &[Value]) -> Option<RestartRelease> {
    let mut restart_keys: HashSet<&str> = HashSet::from([HOST_DRAIN_HOLD_KEY]);
    if holds
        .iter()
        .any(|hold| hold.key == INTERRUPTED_RUN_HOLD_KEY)
        && restart_verdict(branch).should_continue
    {
        restart_keys.insert(INTERRUPTED_RUN_HOLD_KEY);
    }
    let restart_holds_count = holds
        .iter()
        .filter(|hold| restart_keys.contains(hold.key.as_str()))
        .count();
    if restart_holds_count == 0 {
        return None;
    }
    if restart_holds_count == holds.len() {
        return Some(RestartRelease::Resume {
            interrupted_run: restart_keys.contains(INTERRUPTED_RUN_HOLD_KEY),
        });
    }
    holds
        .iter()
        .any(|hold| hold.key == HOST_DRAIN_HOLD_KEY)
        .then(|| RestartRelease::Release {
            keys: vec![HOST_DRAIN_HOLD_KEY.to_owned()],
        })
}
