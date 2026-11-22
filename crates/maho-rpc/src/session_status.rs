//! The `session_status` record and the ONE pure function that derives it (senpi `session-status.ts`).
//!
//! Every status a subscriber sees - the snapshot in the `subscribe_sessions` answer and every delta after it - is
//! computed by [`derive_session_status`] from the registry entry, the session's runtime view and the fanout's
//! pending question frames. Nothing here reads a clock, a socket or a registry.
use crate::session_registry::RpcSessionState;
use serde_json::{Map, Value, json};

/// A session's published labels: the open-time context overlaid with every `set_session_context` patch.
pub type PublishedSessionContext = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatusValue {
    Opening,
    Idle,
    Running,
    WaitingInput,
    Compacting,
    Unloaded,
    Closed,
}

impl SessionStatusValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opening => "opening",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::WaitingInput => "waiting_input",
            Self::Compacting => "compacting",
            Self::Unloaded => "unloaded",
            Self::Closed => "closed",
        }
    }
}

/// `SessionStatusCause`, as its wire string.
pub type SessionStatusCause = &'static str;
/// `SessionStatusActor`: `client`, `agent` or `host`.
pub type SessionStatusActor = &'static str;

/// Why a session's routing handle ended; set once, from the record that sealed it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionStatusTerminal {
    pub status: SessionStatusValue,
    pub reason: Option<String>,
}

/// What the registry entry contributes.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionStatusEntryView {
    pub state: RpcSessionState,
    /// `interactive` or `worker`.
    pub kind: &'static str,
    pub cwd: String,
    pub attachments: i64,
    /// Milliseconds since the Unix epoch of the holder's last observed activity, if known (senpi#6415).
    pub last_activity_at: Option<f64>,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub name: Option<String>,
    pub context: PublishedSessionContext,
    /// The context the session was opened with, before any patch; `childrenOf` matching reads this.
    pub open_context: Option<PublishedSessionContext>,
}

/// What the live runtime contributes; absent while the session is opening or after it was torn down.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionStatusRuntimeView {
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub model: Option<(String, String)>,
    pub thinking_level: Option<String>,
    pub steering: usize,
    pub follow_up: usize,
    pub pending_questions: usize,
    pub waiting: Option<maho_core::retry_wait::RetryWait>,
}

/// One session's views as the stream reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionView {
    pub entry: SessionStatusEntryView,
    pub runtime: Option<SessionStatusRuntimeView>,
}

fn live_status(runtime: &SessionStatusRuntimeView, fanout_questions: usize) -> SessionStatusValue {
    // A question is asked from inside a running turn, and compaction can run inside one too: the narrower state wins.
    if fanout_questions > 0 || runtime.pending_questions > 0 {
        SessionStatusValue::WaitingInput
    } else if runtime.is_compacting {
        SessionStatusValue::Compacting
    } else if runtime.is_streaming {
        SessionStatusValue::Running
    } else {
        SessionStatusValue::Idle
    }
}

/// Derives a session's status record body (everything except subscription, sequence and change) as a JSON object
/// whose `context` key the stream strips for subscribers that did not ask for it.
pub fn derive_session_status(
    entry: &SessionStatusEntryView,
    runtime: Option<&SessionStatusRuntimeView>,
    fanout_questions: usize,
    terminal: Option<&SessionStatusTerminal>,
    cause: Option<SessionStatusCause>,
) -> Map<String, Value> {
    let status = match (terminal, entry.state) {
        (Some(terminal), _) => terminal.status,
        (None, RpcSessionState::Opening) => SessionStatusValue::Opening,
        (
            None,
            RpcSessionState::Closing | RpcSessionState::Quarantined | RpcSessionState::Closed,
        ) => SessionStatusValue::Closed,
        (None, RpcSessionState::Open) => match runtime {
            None => SessionStatusValue::Opening,
            Some(runtime) => live_status(runtime, fanout_questions),
        },
    };
    let reason = terminal
        .and_then(|terminal| terminal.reason.clone())
        .or_else(|| {
            (status == SessionStatusValue::Idle && cause == Some("abort"))
                .then(|| "aborted".to_owned())
        });
    let mut record = Map::new();
    record.insert("status".into(), status.as_str().into());
    if let Some(reason) = reason {
        record.insert("reason".into(), reason.into());
    }
    record.insert("kind".into(), entry.kind.into());
    record.insert("cwd".into(), entry.cwd.clone().into());
    record.insert("attachments".into(), entry.attachments.max(0).into());
    for (key, value) in [
        ("durableSessionId", &entry.durable_session_id),
        ("sessionPath", &entry.session_path),
        ("name", &entry.name),
    ] {
        if let Some(value) = value {
            record.insert(key.into(), value.clone().into());
        }
    }
    if let Some((provider, id)) = runtime.and_then(|runtime| runtime.model.as_ref()) {
        record.insert("model".into(), json!({"provider":provider,"id":id}));
    }
    if let Some(level) = runtime.and_then(|runtime| runtime.thinking_level.as_ref()) {
        record.insert("thinkingLevel".into(), level.clone().into());
    }
    record.insert(
        "pendingQueue".into(),
        json!({"steering":runtime.map_or(0,|runtime|runtime.steering),"followUp":runtime.map_or(0,|runtime|runtime.follow_up)}),
    );
    if status == SessionStatusValue::Running
        && let Some(waiting) = runtime.and_then(|runtime| runtime.waiting.as_ref())
        && let Ok(value) = serde_json::to_value(waiting)
    {
        record.insert("waiting".into(), value);
    }
    record.insert("context".into(), Value::Object(entry.context.clone()));
    record
}

/// The terminal a lifecycle record seals a session with: a park and a handoff park leave a session that reopens by
/// path (`unloaded`); every other close ends it (`closed`) with its `session_closed.reason`.
pub fn terminal_of_lifecycle(record: &Value) -> Option<SessionStatusTerminal> {
    let reason = record["reason"].as_str();
    match record["type"].as_str() {
        Some("session_parked") => Some(SessionStatusTerminal {
            status: SessionStatusValue::Unloaded,
            reason: Some(
                if reason == Some("unloaded") {
                    "unload_session"
                } else {
                    "idle"
                }
                .into(),
            ),
        }),
        Some("session_closed") if reason == Some("handoff_parked") => Some(SessionStatusTerminal {
            status: SessionStatusValue::Unloaded,
            reason: Some("handoff".into()),
        }),
        Some("session_closed") => Some(SessionStatusTerminal {
            status: SessionStatusValue::Closed,
            reason: reason.map(str::to_owned),
        }),
        _ => None,
    }
}

/// The status change a session record publishes, or `None` when the record changes no status.
pub fn cause_of_record(record: &Value) -> Option<SessionStatusCause> {
    Some(match record["type"].as_str()? {
        "agent_start" => "prompt",
        "agent_end" => "agent_end",
        "agent_idle" => "agent_idle",
        "session_abort" => "abort",
        "queue_update" => "queue",
        "model_changed" | "thinking_level_changed" => "set_model",
        "session_replaced" => "replace",
        "session_info_changed" => "rename",
        "session_parked" => "park",
        "session_closed" => "close",
        "question_resolved" => "question",
        "auto_retry_start" | "auto_retry_end" => "retry",
        "extension_ui_request" if record["method"] == "question" => "question",
        "compaction_start" | "compaction_end" => "compaction",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(state: RpcSessionState) -> SessionStatusEntryView {
        SessionStatusEntryView {
            state,
            kind: "interactive",
            cwd: "/w".into(),
            attachments: -1,
            last_activity_at: None,
            durable_session_id: Some("d1".into()),
            session_path: None,
            name: None,
            context: Map::new(),
            open_context: None,
        }
    }

    #[test]
    fn derivation_table_prefers_the_narrowest_live_state() {
        let open = entry(RpcSessionState::Open);
        let busy = SessionStatusRuntimeView {
            is_streaming: true,
            is_compacting: true,
            ..Default::default()
        };
        assert_eq!(
            derive_session_status(&open, Some(&busy), 1, None, None)["status"],
            "waiting_input"
        );
        assert_eq!(
            derive_session_status(&open, Some(&busy), 0, None, None)["status"],
            "compacting"
        );
        assert_eq!(
            derive_session_status(&open, None, 0, None, None)["status"],
            "opening"
        );
        let idle = derive_session_status(
            &open,
            Some(&SessionStatusRuntimeView::default()),
            0,
            None,
            Some("abort"),
        );
        assert_eq!(
            (idle["status"].as_str(), idle["reason"].as_str()),
            (Some("idle"), Some("aborted"))
        );
        assert_eq!(idle["attachments"], 0);
        assert_eq!(
            derive_session_status(&entry(RpcSessionState::Closing), None, 0, None, None)["status"],
            "closed"
        );
        let waiting = maho_core::retry_wait::RetryWait {
            kind: maho_core::retry_wait::RetryWaitKind::RateLimit,
            attempt: 2,
            max_attempts: 5,
            delay_ms: 45_000,
            until: "2026-11-19T03:00:45.000Z".into(),
            error: Some("429: slow down".into()),
        };
        let running_with_wait = derive_session_status(
            &open,
            Some(&SessionStatusRuntimeView {
                is_streaming: true,
                waiting: Some(waiting.clone()),
                ..Default::default()
            }),
            0,
            None,
            None,
        );
        assert_eq!(running_with_wait["status"], "running");
        assert_eq!(
            running_with_wait["waiting"],
            serde_json::to_value(&waiting).unwrap()
        );
        let closed = terminal_of_lifecycle(&json!({"type":"session_closed"})).unwrap();
        let closed_with_wait = derive_session_status(
            &open,
            Some(&SessionStatusRuntimeView {
                waiting: Some(waiting),
                ..Default::default()
            }),
            0,
            Some(&closed),
            None,
        );
        assert!(!closed_with_wait.contains_key("waiting"));
    }

    #[test]
    fn lifecycle_records_seal_and_cause() {
        let parked =
            terminal_of_lifecycle(&json!({"type":"session_closed","reason":"handoff_parked"}))
                .unwrap();
        assert_eq!(
            (parked.status, parked.reason.as_deref()),
            (SessionStatusValue::Unloaded, Some("handoff"))
        );
        let unloaded =
            terminal_of_lifecycle(&json!({"type":"session_parked","reason":"unloaded"})).unwrap();
        assert_eq!(unloaded.reason.as_deref(), Some("unload_session"));
        assert!(terminal_of_lifecycle(&json!({"type":"agent_end"})).is_none());
        assert_eq!(
            cause_of_record(&json!({"type":"extension_ui_request","method":"question"})),
            Some("question")
        );
        assert_eq!(
            cause_of_record(&json!({"type":"auto_retry_start"})),
            Some("retry")
        );
        assert_eq!(
            cause_of_record(&json!({"type":"auto_retry_end"})),
            Some("retry")
        );
        assert_eq!(
            cause_of_record(&json!({"type":"extension_ui_request","method":"confirm"})),
            None
        );
        // senpi#6672: a rename publishes a status change under its own cause.
        assert_eq!(
            cause_of_record(&json!({"type":"session_info_changed"})),
            Some("rename")
        );
    }
}
