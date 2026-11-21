//! Port of senpi `packages/coding-agent/src/core/session-settled.ts` (senpi#6768).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::session_archived::session_marker_data;
use crate::session_manager::SessionManager;

pub const SESSION_SETTLED_CUSTOM_TYPE: &str = "session_settled";

pub const BY_REFUSAL: &str = "set_session_settled: by must be a ThreadSettledBy (kind user, agent, pr-merged, pr-closed or inactivity, with its fields)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOperatingState {
    Active,
    Held,
    Settled,
    Archived,
}

impl SessionOperatingState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Held => "held",
            Self::Settled => "settled",
            Self::Archived => "archived",
        }
    }
}

pub const fn resolve_session_operating_state(
    archived: bool,
    settled: bool,
    held: bool,
) -> SessionOperatingState {
    if archived {
        SessionOperatingState::Archived
    } else if settled {
        SessionOperatingState::Settled
    } else if held {
        SessionOperatingState::Held
    } else {
        SessionOperatingState::Active
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ThreadSettledBy {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "agent")]
    Agent {
        #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        #[serde(
            rename = "threadTitle",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        thread_title: Option<String>,
    },
    #[serde(rename = "pr-merged")]
    PrMerged {
        #[serde(
            rename = "pullRequestNumber",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pull_request_number: Option<u64>,
    },
    #[serde(rename = "pr-closed")]
    PrClosed {
        #[serde(
            rename = "pullRequestNumber",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pull_request_number: Option<u64>,
    },
    #[serde(rename = "inactivity")]
    Inactivity { days: u64 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSettled {
    pub settled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<ThreadSettledBy>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledFields {
    pub settled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_by: Option<ThreadSettledBy>,
}

pub const NOT_SETTLED: SessionSettled = SessionSettled {
    settled: false,
    by: None,
};

fn integer_at_least(value: Option<&Value>, minimum: u64) -> Option<u64> {
    let value = value?;
    if let Some(n) = value.as_u64() {
        return (n >= minimum).then_some(n);
    }
    if let Some(n) = value.as_i64()
        && n >= 0
    {
        let n = u64::try_from(n).ok()?;
        return (n >= minimum).then_some(n);
    }
    if let Some(f) = value.as_f64()
        && f.is_finite()
        && f >= 0.0
        && f.fract() == 0.0
        && f <= (u64::MAX as f64)
    {
        let n = f as u64;
        return (n >= minimum).then_some(n);
    }
    None
}

fn optional_text(value: Option<&Value>) -> Result<Option<String>, ()> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let Some(text) = value.as_str() else {
        return Err(());
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Err(())
    } else {
        Ok(Some(trimmed.to_owned()))
    }
}

pub fn parse_thread_settled_by(value: &Value) -> Option<ThreadSettledBy> {
    let by = value.as_object()?;
    match by.get("kind").and_then(Value::as_str)? {
        "user" => Some(ThreadSettledBy::User),
        "agent" => {
            let thread_id = optional_text(by.get("threadId")).ok()?;
            let thread_title = optional_text(by.get("threadTitle")).ok()?;
            Some(ThreadSettledBy::Agent {
                thread_id,
                thread_title,
            })
        }
        "pr-merged" => {
            let pull_request_number = match by.get("pullRequestNumber") {
                None | Some(Value::Null) => None,
                Some(raw) => Some(integer_at_least(Some(raw), 1)?),
            };
            Some(ThreadSettledBy::PrMerged {
                pull_request_number,
            })
        }
        "pr-closed" => {
            let pull_request_number = match by.get("pullRequestNumber") {
                None | Some(Value::Null) => None,
                Some(raw) => Some(integer_at_least(Some(raw), 1)?),
            };
            Some(ThreadSettledBy::PrClosed {
                pull_request_number,
            })
        }
        "inactivity" => {
            let days = integer_at_least(by.get("days"), 0)?;
            Some(ThreadSettledBy::Inactivity { days })
        }
        _ => None,
    }
}

pub fn parse_session_settled(command: &Value) -> Result<SessionSettled, String> {
    let Some(settled) = command.get("settled").and_then(Value::as_bool) else {
        return Err("set_session_settled: settled must be a boolean".to_owned());
    };
    let Some(raw_by) = command.get("by").filter(|value| !value.is_null()) else {
        return Ok(SessionSettled { settled, by: None });
    };
    let Some(by) = parse_thread_settled_by(raw_by) else {
        return Err(BY_REFUSAL.to_owned());
    };
    Ok(if settled {
        SessionSettled {
            settled: true,
            by: Some(by),
        }
    } else {
        NOT_SETTLED
    })
}

pub fn settled_state_of(entry: &Value, session_id: &str) -> Option<SessionSettled> {
    let data = session_marker_data(entry, SESSION_SETTLED_CUSTOM_TYPE, session_id)?;
    if data.get("settled").and_then(Value::as_bool) != Some(true) {
        return Some(NOT_SETTLED);
    }
    let by = data.get("by").and_then(parse_thread_settled_by);
    Some(SessionSettled { settled: true, by })
}

pub fn settled_fields(state: &SessionSettled) -> SettledFields {
    if state.settled {
        SettledFields {
            settled: true,
            settled_by: state.by.clone(),
        }
    } else {
        SettledFields {
            settled: false,
            settled_by: None,
        }
    }
}

pub fn read_session_settled(manager: &SessionManager) -> SessionSettled {
    manager.get_settled()
}

pub fn set_session_settled(manager: &mut SessionManager, state: &SessionSettled) -> Value {
    let session_id = manager.session_id().to_owned();
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut data = Map::new();
    data.insert("settled".to_owned(), Value::Bool(state.settled));
    if state.settled
        && let Some(by) = &state.by
        && let Ok(by_val) = serde_json::to_value(by)
    {
        data.insert("by".to_owned(), by_val);
    }
    data.insert("at".to_owned(), Value::String(at));
    data.insert("sessionId".to_owned(), Value::String(session_id));
    manager.append_custom(SESSION_SETTLED_CUSTOM_TYPE, Some(Value::Object(data)))
}
