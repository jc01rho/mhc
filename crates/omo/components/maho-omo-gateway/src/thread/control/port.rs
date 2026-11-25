//! The `SessionControl` and `SessionHost` ports.
//!
//! Upstream counterpart: `thread/control/port.ts` and `thread/control/session-host.ts` (omo #12879, #12884).
//!
//! `SessionControl` is one live thread's controls, wherever it is held: the session on this device's endpoint
//! (`local_control.rs`), or the session on another device of the account (the remote leg implements this same trait
//! over that device's relay). Every control tool has one body over this port (`operations.rs`); `locate.rs` picks the
//! implementation, and nothing past it reads the device again. A method answers `ControlError::Refused` with the tool
//! failure the body answers as data; `ControlError::Transport` is the leg's own transport failure, which the tool maps
//! where it always did (`host_unavailable:`, `unsupported:`).

use serde_json::Value;

use crate::thread::adapter::BoxFuture;
use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::contracts_results::ThreadResultSummary;
use crate::thread::contracts_state::ThreadModel;
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::model_match::ModelChoice;
use crate::thread::thread_state::thread_model;

use super::interrupt::InterruptOutcome;

/// Active model and thinking level snapshot before a compound `model:level` switch (`#14482`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionModelState {
    pub model: ThreadModel,
    pub thinking_level: String,
}

pub fn session_model_state(state: &Value) -> Option<SessionModelState> {
    let thinking_level = state.get("thinkingLevel")?.as_str()?.to_owned();
    let model = thread_model(state.get("model"))?;
    Some(SessionModelState {
        model,
        thinking_level,
    })
}

/// The engine's `set_thinking_level` scope that changes only the addressed session. Any other scope also rewrites the
/// device's remembered level for the model, which a thread tool never asks for.
pub const SESSION_ONLY_THINKING_SCOPE: &str = "turn";

/// How a control method fails: its typed refusal, answered by the tool as data, or the leg's transport failure.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlError {
    Refused(Box<ThreadToolFailure>),
    Transport(String),
}

impl ControlError {
    /// The typed refusal, when this is one.
    pub fn refusal(&self) -> Option<&ThreadToolFailure> {
        match self {
            Self::Refused(failure) => Some(failure.as_ref()),
            Self::Transport(_) => None,
        }
    }
}

pub type ControlResult<T> = Result<T, ControlError>;

/// A typed refusal for the thread a control addressed.
pub fn refuse(
    code: &str,
    message: impl Into<String>,
    next_action: impl Into<String>,
    details: Option<Value>,
) -> ControlError {
    ControlError::Refused(Box::new(thread_tool_failure(
        code,
        message,
        next_action,
        details,
    )))
}

/// How a close ended: the session ended, no live session held it any more, or another client still holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    Closed,
    NotLive,
    HeldElsewhere,
}

/// Options for `thread_close` (`force`, `graceful`, `wait_ms`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloseOptions {
    pub force: bool,
    pub graceful: bool,
    pub wait_ms: Option<i64>,
}

/// Drain summary returned when a graceful close ends a session (omo #14313).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CloseGraceful {
    pub turn: u64,
    pub waited_ms: u64,
    pub stopped_wake_sources: Vec<String>,
    pub returned_user_messages: Vec<String>,
}

/// A close's outcome and the deliveries the engine handed back unwritten (empty when none, omo #14234).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseAnswer {
    pub outcome: CloseOutcome,
    pub dropped_deliveries: Vec<String>,
    pub graceful: Option<CloseGraceful>,
}

/// The session's model catalog. `held`: answered from a copy the leg keeps (bounded in age), on which only an exact
/// provider/id is decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    pub models: Vec<ModelChoice>,
    pub held: bool,
}

/// One live thread's controls, wherever it is held.
pub trait SessionControl: Send + Sync {
    /// The thread's durable id: the `thread` every result names.
    fn thread_id(&self) -> &str;
    /// The account device holding the thread, when it is not this one.
    fn device(&self) -> Option<&str>;
    /// The thread's title as its listing names it.
    fn title(&self) -> &str;
    /// Names on the target device that a rename must not take.
    fn visible(&self) -> BoxFuture<ControlResult<Vec<ThreadAddressEntry>>>;
    /// The engine's `interrupt` with the background stop (`interrupt.rs`); an idle session answers `interrupted: false`.
    fn interrupt(&self, turn_id: Option<String>) -> BoxFuture<ControlResult<InterruptOutcome>>;
    /// The one close ladder (`close_ladder.rs`) on the thread's engine; `force` drops what the session has not read,
    /// `graceful` drains the active turn and hands unread deliveries back to their senders.
    fn close(&self, options: CloseOptions) -> BoxFuture<ControlResult<CloseAnswer>>;
    fn set_name(&self, name: String) -> BoxFuture<ControlResult<()>>;
    /// `fresh` bypasses a catalog the leg holds.
    fn get_available_models(&self, fresh: bool) -> BoxFuture<ControlResult<ModelCatalog>>;
    /// Fresh `get_state` snapshot for restoring a refused compound `model:level` switch.
    fn get_model_state(&self) -> BoxFuture<ControlResult<Option<SessionModelState>>>;
    fn set_model(&self, provider: String, id: String) -> BoxFuture<ControlResult<ThreadModel>>;
    fn get_available_thinking_levels(&self) -> BoxFuture<ControlResult<Vec<String>>>;
    /// Changes only this session's level ([`SESSION_ONLY_THINKING_SCOPE`]); refuses `thinking_level_unsupported` for a
    /// level the active model cannot run.
    fn set_thinking_level(&self, level: String) -> BoxFuture<ControlResult<()>>;
    fn get_messages(&self) -> BoxFuture<ControlResult<Vec<Value>>>;
    /// Whether a turn is in flight: the listing's activity, else the engine's own `get_state`.
    fn running(&self) -> BoxFuture<ControlResult<bool>>;
    /// senpi `set_session_archived`: the archived flag in the session file, nothing else; refuses `unsupported` when
    /// the engine does not know the command.
    fn set_archived(&self, archived: bool) -> BoxFuture<ControlResult<()>>;
    /// The thread as `thread_list` lists it, with the archived flag set to `archived`.
    fn listed(&self, archived: bool) -> BoxFuture<ControlResult<ThreadResultSummary>>;
}

/// The thread a result names: `thread`, and its device when it is on another one.
pub fn control_identity(control: &dyn SessionControl) -> (String, Option<String>) {
    (
        control.thread_id().to_owned(),
        control.device().map(str::to_owned),
    )
}

/// One `open_session` as thread_create asks for it, on whichever device the host stands for: a new session (`cwd`),
/// an existing session's file (`session_path`), or a fork (`fork_from`). `permission_preset` is the approval mode the
/// new session starts in; `provider` + `model_id` and `thinking_level` are the engine's launch profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionOpenRequest {
    pub cwd: Option<String>,
    pub session_path: Option<String>,
    pub fork_from: Option<String>,
    pub name: Option<String>,
    pub initial_skills: Vec<String>,
    pub permission_preset: Option<String>,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub thinking_level: Option<String>,
    pub durable_session_id: Option<String>,
}

/// Host-only hints for `SessionHost::open`, never forwarded on the engine's `open_session` frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionOpenOptions {
    pub confirm_reach: bool,
}

/// The port thread_create opens sessions through (#12884). Past placement the tool body sees only this: the device's
/// visible threads, which a new name must not collide with, and one `open`. This device's operator endpoint and another
/// device's engine endpoint (through its relay) each implement it.
pub trait SessionHost: Send + Sync {
    /// The device that opens; `None` when it is this one.
    fn device(&self) -> Option<&str>;
    fn visible(&self) -> BoxFuture<ControlResult<Vec<ThreadAddressEntry>>>;
    fn open(
        &self,
        request: SessionOpenRequest,
        options: SessionOpenOptions,
    ) -> BoxFuture<ControlResult<ThreadResultSummary>>;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn session_model_state_requires_a_valid_model_and_thinking_level() {
        let valid = json!({
            "model": { "provider": "openai", "id": "gpt-6" },
            "thinkingLevel": "high",
        });
        assert_eq!(
            session_model_state(&valid),
            Some(SessionModelState {
                model: ThreadModel {
                    provider: "openai".into(),
                    id: "gpt-6".into(),
                },
                thinking_level: "high".into(),
            })
        );
        assert_eq!(
            session_model_state(&json!({ "model": { "provider": "openai", "id": "gpt-6" } })),
            None
        );
        assert_eq!(
            session_model_state(
                &json!({ "model": { "provider": "", "id": "gpt-6" }, "thinkingLevel": "high" })
            ),
            None
        );
        assert_eq!(
            session_model_state(
                &json!({ "model": { "provider": "openai", "id": "gpt-6" }, "thinkingLevel": 3 })
            ),
            None
        );
    }
}
