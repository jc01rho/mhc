//! A thread this device's endpoint holds, controlled through the endpoint that listed it.
//!
//! Upstream counterpart: `thread/control/local-control.ts`. Every method is one engine record on the session's
//! command channel (`host_view::SessionCommandPort`), read with the same frame rules the live surface applies.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::address_book::AddressEntry;
use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::contracts_results::{ThreadResultSummary, thread_link};
use crate::thread::contracts_state::{ThreadModel, ThreadState};
use crate::thread::host_view::{SessionCommandPort, ThreadHostSession};
use crate::thread::live_surface::data_record;
use crate::thread::model_match::ModelChoice;
use crate::thread::thread_row::live_row;
use crate::thread::thread_state::thread_state;
use crate::thread::types::GatewayEndpointKind;

use super::close_ladder::{RecordAnswer, close_ladder, session_gone_reason};
use super::interrupt::{InterruptOutcome, interrupt_outcome, interrupt_request};
use super::port::{
    CloseAnswer, ControlError, ControlResult, ModelCatalog, SESSION_ONLY_THINKING_SCOPE,
    SessionControl, refuse,
};

const THINKING_LEVEL_UNSUPPORTED: &str = "thinking_level_unsupported:";
const ARCHIVE_UNSUPPORTED: &str = "unsupported:set_session_archived";

/// One record on the session's endpoint, answered with its reply data.
async fn command(
    port: Arc<dyn SessionCommandPort>,
    command_type: &'static str,
    session_id: String,
    mut fields: Map<String, Value>,
) -> ControlResult<Map<String, Value>> {
    fields.insert("type".into(), json!(command_type));
    fields.insert("sessionId".into(), json!(session_id));
    let frame = port
        .send(Value::Object(fields))
        .await
        .map_err(ControlError::Transport)?;
    data_record(&frame, command_type).map_err(ControlError::Transport)
}

/// One ladder record's answer: the reply data, `Gone` for a session the endpoint no longer holds.
fn ladder_answer(frame: &Value) -> ControlResult<RecordAnswer> {
    if frame.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(RecordAnswer::Ok(
            frame
                .get("data")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        ));
    }
    let code = frame
        .get("errorCode")
        .and_then(Value::as_str)
        .or_else(|| frame.get("error").and_then(Value::as_str))
        .unwrap_or_default();
    match session_gone_reason(code) {
        Some(reason) => Ok(RecordAnswer::Gone(reason.to_owned())),
        None => Err(ControlError::Transport(format!(
            "thread RPC request failed: {}",
            frame
                .get("error")
                .map_or_else(|| frame.to_string(), Value::to_string)
        ))),
    }
}

/// The controls of one session on this device's endpoint.
pub struct LocalControl {
    port: Arc<dyn SessionCommandPort>,
    session: ThreadHostSession,
    thread_id: String,
    title: String,
    visible: Vec<ThreadAddressEntry>,
    entry: Option<AddressEntry>,
    now: String,
}

impl LocalControl {
    /// `visible` is the threads a rename must not collide with; `entry` is the session's address book entry, which
    /// supplies its listing facts; `now` stands in for a creation time nothing reports.
    pub fn new(
        port: Arc<dyn SessionCommandPort>,
        session: ThreadHostSession,
        title: String,
        visible: Vec<ThreadAddressEntry>,
        entry: Option<AddressEntry>,
        now: String,
    ) -> Self {
        Self {
            thread_id: session.thread_id().to_owned(),
            port,
            session,
            title,
            visible,
            entry,
            now,
        }
    }

    fn routed(&self) -> (Arc<dyn SessionCommandPort>, String) {
        (self.port.clone(), self.session.session_id.clone())
    }
}

impl SessionControl for LocalControl {
    fn thread_id(&self) -> &str {
        &self.thread_id
    }

    fn device(&self) -> Option<&str> {
        None
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn visible(&self) -> BoxFuture<ControlResult<Vec<ThreadAddressEntry>>> {
        let visible = self.visible.clone();
        Box::pin(async move { Ok(visible) })
    }

    fn interrupt(&self, turn_id: Option<String>) -> BoxFuture<ControlResult<InterruptOutcome>> {
        let (port, session_id) = self.routed();
        Box::pin(async move {
            let data = command(
                port,
                "interrupt",
                session_id,
                interrupt_request(turn_id.as_deref()),
            )
            .await?;
            Ok(interrupt_outcome(&Value::Object(data)))
        })
    }

    fn close(&self, force: bool) -> BoxFuture<ControlResult<CloseAnswer>> {
        if self.session.endpoint_kind == GatewayEndpointKind::Tui {
            let thread_id = self.thread_id.clone();
            return Box::pin(async move {
                Err(refuse(
                    "unsupported",
                    format!(
                        "Thread {thread_id} belongs to an interactive terminal, which closes it when the terminal exits."
                    ),
                    "Close that terminal, or use thread_interrupt (tool.thread_interrupt() in an eval cell) to stop its turn.",
                    Some(json!({ "command": "close_session" })),
                ))
            });
        }
        let (port, session_id) = self.routed();
        Box::pin(async move {
            let send = move |record: Value| {
                let port = port.clone();
                async move {
                    match port.send(record).await {
                        Ok(frame) => ladder_answer(&frame),
                        Err(error) => match session_gone_reason(&error) {
                            Some(reason) => Ok(RecordAnswer::Gone(reason.to_owned())),
                            None => Err(ControlError::Transport(error)),
                        },
                    }
                }
            };
            close_ladder(&session_id, send, force).await
        })
    }

    fn set_name(&self, name: String) -> BoxFuture<ControlResult<()>> {
        let (port, session_id) = self.routed();
        Box::pin(async move {
            let mut fields = Map::new();
            fields.insert("name".into(), json!(name));
            command(port, "set_session_name", session_id, fields).await?;
            Ok(())
        })
    }

    fn get_available_models(&self, _fresh: bool) -> BoxFuture<ControlResult<ModelCatalog>> {
        let (port, session_id) = self.routed();
        Box::pin(async move {
            let data = command(port, "get_available_models", session_id, Map::new()).await?;
            let models = data
                .get("models")
                .and_then(Value::as_array)
                .map(|models| {
                    models
                        .iter()
                        .filter_map(|model| {
                            Some(ModelChoice {
                                provider: model.get("provider")?.as_str()?.to_owned(),
                                id: model.get("id")?.as_str()?.to_owned(),
                                name: model.get("name").and_then(Value::as_str).map(str::to_owned),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(ModelCatalog {
                models,
                held: false,
            })
        })
    }

    fn set_model(&self, provider: String, id: String) -> BoxFuture<ControlResult<ThreadModel>> {
        let (port, session_id) = self.routed();
        Box::pin(async move {
            let mut fields = Map::new();
            fields.insert("provider".into(), json!(provider));
            fields.insert("modelId".into(), json!(id));
            // `scope: "session"` switches only this session; without it the engine persists the device's default model.
            fields.insert("scope".into(), json!("session"));
            let data = command(port, "set_model", session_id, fields).await?;
            let named = |key: &str| data.get(key).and_then(Value::as_str).map(str::to_owned);
            Ok(ThreadModel {
                provider: named("provider").unwrap_or(provider),
                id: named("id").unwrap_or(id),
            })
        })
    }

    fn get_available_thinking_levels(&self) -> BoxFuture<ControlResult<Vec<String>>> {
        let (port, session_id) = self.routed();
        Box::pin(async move {
            let data = command(
                port,
                "get_available_thinking_levels",
                session_id,
                Map::new(),
            )
            .await?;
            Ok(data
                .get("levels")
                .and_then(Value::as_array)
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default())
        })
    }

    fn set_thinking_level(&self, level: String) -> BoxFuture<ControlResult<()>> {
        let (port, session_id) = self.routed();
        Box::pin(async move {
            let mut fields = Map::new();
            fields.insert("level".into(), json!(level));
            fields.insert("scope".into(), json!(SESSION_ONLY_THINKING_SCOPE));
            match command(port, "set_thinking_level", session_id, fields).await {
                Ok(_) => Ok(()),
                Err(ControlError::Transport(error)) => {
                    let unsupported = error
                        .strip_prefix(THINKING_LEVEL_UNSUPPORTED)
                        .map(str::to_owned);
                    match unsupported {
                        Some(message) => Err(refuse(
                            "thinking_level_unsupported",
                            message,
                            "Choose a level from the supported list and retry.",
                            Some(json!({ "level": level })),
                        )),
                        None => Err(ControlError::Transport(error)),
                    }
                }
                Err(error) => Err(error),
            }
        })
    }

    fn get_messages(&self) -> BoxFuture<ControlResult<Vec<Value>>> {
        let (port, session_id) = self.routed();
        Box::pin(async move {
            let data = command(port, "get_messages", session_id, Map::new()).await?;
            Ok(data
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default())
        })
    }

    fn running(&self) -> BoxFuture<ControlResult<bool>> {
        // The same rule thread_list applies: a queued or approval-waiting thread is mid-turn.
        let listed_running = thread_state(
            self.session.archived,
            true,
            self.session.activity.as_deref(),
        ) == ThreadState::Running;
        let (port, session_id) = self.routed();
        Box::pin(async move {
            if listed_running {
                return Ok(true);
            }
            let state = command(port, "get_state", session_id, Map::new()).await?;
            Ok(state.get("isStreaming").and_then(Value::as_bool) == Some(true))
        })
    }

    fn set_archived(&self, archived: bool) -> BoxFuture<ControlResult<()>> {
        let (port, session_id) = self.routed();
        let thread_id = self.thread_id.clone();
        Box::pin(async move {
            let mut fields = Map::new();
            fields.insert("archived".into(), json!(archived));
            match command(port, "set_session_archived", session_id, fields).await {
                Ok(_) => Ok(()),
                Err(ControlError::Transport(error)) if error == ARCHIVE_UNSUPPORTED => Err(refuse(
                    "unsupported",
                    format!(
                        "The engine holding thread {thread_id} does not know set_session_archived: it runs a senpi from before archived threads."
                    ),
                    "Update the engine on this device and restart the session's host or terminal, then retry.",
                    Some(json!({ "command": "set_session_archived" })),
                )),
                Err(error) => Err(error),
            }
        })
    }

    fn listed(&self, archived: bool) -> BoxFuture<ControlResult<ThreadResultSummary>> {
        let mut session = self.session.clone();
        session.archived = archived;
        let mut row = live_row(&session, self.entry.as_ref(), &self.now);
        row.name = self.title.clone();
        row.link = thread_link(&self.thread_id, &self.title);
        Box::pin(async move { Ok(row) })
    }
}
