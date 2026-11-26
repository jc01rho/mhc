//! Remote session controls and locator over [`RemoteThreadSurface`] (`thread/control/relay-control.ts`;
//! omo #14324, #14313, #14273, #14482, #14305, #13304).

use std::sync::{Arc, Mutex, PoisonError};

use maho_thread_address_core::row_key;
use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::address::{ResolveContext, ResolveOptions, ResolvedWhere, resolve_address};
use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::contracts_results::ThreadResultSummary;
use crate::thread::contracts_state::ThreadModel;
use crate::thread::model_match::ModelChoice;
use crate::thread::remote::address_book::relay_address_entry;
use crate::thread::remote::closed_rows::with_closed_rows;
use crate::thread::remote::outcome::relay_outcome_failure;
use crate::thread::remote::ports::{
    RemoteCommandOutcome, RemoteSessionRow, RemoteThreadSurface, SessionCommandOptions,
    escape_for_display, keyed,
};
use crate::thread::remote::remote_ops::reply_data;
use crate::thread::store_ops::parse_rfc3339_millis;

use super::close_ladder::{REMOTE_GRACEFUL_MAX_WAIT_MS, RecordAnswer, SESSION_GONE, close_ladder};
use super::interrupt::{InterruptOutcome, interrupt_over};
use super::locate::{LocatedControl, RemoteControlLocator};
use super::port::{
    CloseAnswer, CloseOptions, CloseOutcome, ControlError, ControlResult, ModelCatalog,
    SESSION_ONLY_THINKING_SCOPE, SessionControl, SessionModelState, refuse, session_model_state,
};
use super::relay_session_host::displayed_thread;

fn is_thinking_unsupported(reason: &str) -> bool {
    reason == "thinking_level_unsupported"
        || (reason.starts_with("Thinking level ")
            && reason.ends_with(" is not supported by the active model."))
}

fn models_of(value: Option<&Value>) -> Vec<ModelChoice> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|entry| {
                    let provider = entry.get("provider")?.as_str()?.to_owned();
                    let id = entry.get("id")?.as_str()?.to_owned();
                    let name = entry.get("name").and_then(Value::as_str).map(str::to_owned);
                    Some(ModelChoice { provider, id, name })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Controls a live session on another device of the account via [`RemoteSessionsPort::session_command`].
pub struct RelayControl {
    remote: Arc<RemoteThreadSurface>,
    row: RemoteSessionRow,
    thread_id: String,
    title: String,
}

pub fn create_relay_control(
    remote: Arc<RemoteThreadSurface>,
    row: RemoteSessionRow,
) -> RelayControl {
    let thread_id = row_key(&keyed(&row));
    let title = escape_for_display(&row.title);
    RelayControl {
        remote,
        row,
        thread_id,
        title,
    }
}

async fn run_command_outcome(
    remote: &RemoteThreadSurface,
    row: &RemoteSessionRow,
    mut rpc: Map<String, Value>,
    options: SessionCommandOptions,
) -> ControlResult<(Value, bool)> {
    rpc.insert("sessionId".into(), json!(row.session_id));
    let outcome = remote
        .sessions
        .session_command(
            row.device.clone(),
            row.session_id.clone(),
            Value::Object(rpc),
            options,
        )
        .await;
    match outcome {
        RemoteCommandOutcome::Ok {
            response, local, ..
        } => Ok((response, local)),
        ref other => {
            if let RemoteCommandOutcome::Refused { reason, .. } = other
                && SESSION_GONE.contains(&reason.as_str())
            {
                remote.sessions.closed(row, None, None);
            }
            Err(ControlError::Refused(relay_outcome_failure(
                other,
                &row.device,
            )))
        }
    }
}

async fn run_command(
    remote: &RemoteThreadSurface,
    row: &RemoteSessionRow,
    rpc: Map<String, Value>,
    options: SessionCommandOptions,
) -> ControlResult<Map<String, Value>> {
    let (response, _) = run_command_outcome(remote, row, rpc, options).await?;
    Ok(reply_data(&response))
}

async fn run_answer(
    remote: &RemoteThreadSurface,
    row: &RemoteSessionRow,
    rpc: Value,
    wait_ms: Option<i64>,
) -> ControlResult<RecordAnswer> {
    let outcome = remote
        .sessions
        .session_command(
            row.device.clone(),
            row.session_id.clone(),
            rpc,
            SessionCommandOptions {
                fresh: false,
                direct: false,
                wait_ms,
            },
        )
        .await;
    match outcome {
        RemoteCommandOutcome::Ok { response, .. } => Ok(RecordAnswer::Ok(reply_data(&response))),
        RemoteCommandOutcome::Refused { reason, .. } if SESSION_GONE.contains(&reason.as_str()) => {
            Ok(RecordAnswer::Gone(reason))
        }
        ref other => Err(ControlError::Refused(relay_outcome_failure(
            other,
            &row.device,
        ))),
    }
}

impl SessionControl for RelayControl {
    fn thread_id(&self) -> &str {
        &self.thread_id
    }

    fn device(&self) -> Option<&str> {
        Some(&self.row.device)
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn visible(&self) -> BoxFuture<ControlResult<Vec<ThreadAddressEntry>>> {
        let remote = Arc::clone(&self.remote);
        let device = self.row.device.clone();
        Box::pin(async move {
            match remote.sessions.sessions(None).await {
                Ok(rows) => Ok(rows
                    .iter()
                    .filter(|entry| entry.device == device)
                    .map(|entry| relay_address_entry(entry, &[]))
                    .collect()),
                Err(error) => Err(refuse(
                    "host_unavailable",
                    "The target device's thread names could not be listed.",
                    "Retry when the device service is reachable.",
                    Some(json!({ "device": device, "reason": error })),
                )),
            }
        })
    }

    fn interrupt(&self, turn_id: Option<String>) -> BoxFuture<ControlResult<InterruptOutcome>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            interrupt_over(
                |mut request| {
                    Box::pin(async move {
                        request.insert("type".into(), json!("interrupt"));
                        let data =
                            run_command(&remote, &row, request, SessionCommandOptions::default())
                                .await?;
                        Ok(Value::Object(data))
                    })
                },
                turn_id.as_deref(),
            )
            .await
        })
    }

    fn close(&self, options: CloseOptions) -> BoxFuture<ControlResult<CloseAnswer>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let unloaded: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let closed_at: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
            let result = close_ladder(
                &row.session_id,
                |rpc, wait_ms| {
                    let remote = Arc::clone(&remote);
                    let row = row.clone();
                    let unloaded = Arc::clone(&unloaded);
                    let closed_at = Arc::clone(&closed_at);
                    async move {
                        let answered = run_answer(&remote, &row, rpc, wait_ms).await?;
                        if let RecordAnswer::Ok(ref data) = answered {
                            if let Some(path) = data
                                .get("sessionPath")
                                .and_then(Value::as_str)
                                .filter(|path| !path.is_empty())
                            {
                                *unloaded.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(path.to_owned());
                            }
                            if let Some(at) = data
                                .get("effect")
                                .and_then(Value::as_object)
                                .and_then(|effect| effect.get("updatedAt"))
                                .and_then(Value::as_str)
                                .and_then(parse_rfc3339_millis)
                            {
                                *closed_at.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(at);
                            }
                        }
                        Ok(answered)
                    }
                },
                options,
                Some(REMOTE_GRACEFUL_MAX_WAIT_MS),
            )
            .await;

            match result {
                Ok(closed) => {
                    if closed.outcome != CloseOutcome::HeldElsewhere {
                        let file = unloaded
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        let at = *closed_at.lock().unwrap_or_else(PoisonError::into_inner);
                        remote.sessions.closed(&row, file.as_deref(), at);
                    }
                    Ok(closed)
                }
                Err(ControlError::Refused(mut failure)) => {
                    let mut map = match failure.details.take() {
                        Some(Value::Object(map)) => map,
                        _ => Map::new(),
                    };
                    map.entry("device".to_owned())
                        .or_insert_with(|| json!(row.device));
                    failure.details = Some(Value::Object(map));
                    Err(ControlError::Refused(failure))
                }
                Err(other) => Err(other),
            }
        })
    }

    fn set_name(&self, name: String) -> BoxFuture<ControlResult<()>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let mut rpc = Map::new();
            rpc.insert("type".into(), json!("set_session_name"));
            rpc.insert("name".into(), json!(name));
            run_command(&remote, &row, rpc, SessionCommandOptions::default()).await?;
            Ok(())
        })
    }

    fn get_available_models(&self, fresh: bool) -> BoxFuture<ControlResult<ModelCatalog>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let mut rpc = Map::new();
            rpc.insert("type".into(), json!("get_available_models"));
            let (response, local) = run_command_outcome(
                &remote,
                &row,
                rpc,
                SessionCommandOptions {
                    fresh,
                    ..SessionCommandOptions::default()
                },
            )
            .await?;
            let data = reply_data(&response);
            Ok(ModelCatalog {
                models: models_of(data.get("models")),
                held: local,
            })
        })
    }

    fn get_model_state(&self) -> BoxFuture<ControlResult<Option<SessionModelState>>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let mut rpc = Map::new();
            rpc.insert("type".into(), json!("get_state"));
            let data = run_command(
                &remote,
                &row,
                rpc,
                SessionCommandOptions {
                    fresh: true,
                    ..SessionCommandOptions::default()
                },
            )
            .await?;
            Ok(session_model_state(&Value::Object(data)))
        })
    }

    fn set_model(&self, provider: String, id: String) -> BoxFuture<ControlResult<ThreadModel>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let mut rpc = Map::new();
            rpc.insert("type".into(), json!("set_model"));
            rpc.insert("provider".into(), json!(provider));
            rpc.insert("modelId".into(), json!(id));
            rpc.insert("scope".into(), json!("session"));
            let data = run_command(&remote, &row, rpc, SessionCommandOptions::default()).await?;
            let named = |key: &str| data.get(key).and_then(Value::as_str).map(str::to_owned);
            Ok(ThreadModel {
                provider: named("provider").unwrap_or(provider),
                id: named("id").unwrap_or(id),
            })
        })
    }

    fn get_available_thinking_levels(&self) -> BoxFuture<ControlResult<Vec<String>>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let mut rpc = Map::new();
            rpc.insert("type".into(), json!("get_available_thinking_levels"));
            let data = run_command(&remote, &row, rpc, SessionCommandOptions::default()).await?;
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
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let outcome = remote
                .sessions
                .session_command(
                    row.device.clone(),
                    row.session_id.clone(),
                    json!({
                        "type": "set_thinking_level",
                        "sessionId": row.session_id,
                        "level": level,
                        "scope": SESSION_ONLY_THINKING_SCOPE,
                    }),
                    SessionCommandOptions::default(),
                )
                .await;
            match outcome {
                RemoteCommandOutcome::Ok { .. } => Ok(()),
                RemoteCommandOutcome::Refused { ref reason, .. }
                    if is_thinking_unsupported(reason) =>
                {
                    Err(refuse(
                        "thinking_level_unsupported",
                        format!("Thinking level {level} is not supported by the active model."),
                        "Choose a level from the supported list and retry.",
                        Some(json!({ "device": row.device, "level": level })),
                    ))
                }
                ref other => Err(ControlError::Refused(relay_outcome_failure(
                    other,
                    &row.device,
                ))),
            }
        })
    }

    fn get_messages(&self) -> BoxFuture<ControlResult<Vec<Value>>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let mut rpc = Map::new();
            rpc.insert("type".into(), json!("get_messages"));
            let data = run_command(&remote, &row, rpc, SessionCommandOptions::default()).await?;
            Ok(data
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default())
        })
    }

    fn running(&self) -> BoxFuture<ControlResult<bool>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            if row.state == "running" {
                return Ok(true);
            }
            let mut rpc = Map::new();
            rpc.insert("type".into(), json!("get_state"));
            let data = run_command(&remote, &row, rpc, SessionCommandOptions::default()).await?;
            Ok(data.get("isStreaming").and_then(Value::as_bool) == Some(true))
        })
    }

    fn set_archived(&self, archived: bool) -> BoxFuture<ControlResult<()>> {
        let remote = Arc::clone(&self.remote);
        let row = self.row.clone();
        Box::pin(async move {
            let mut rpc = Map::new();
            rpc.insert("type".into(), json!("set_session_archived"));
            rpc.insert("archived".into(), json!(archived));
            run_command(&remote, &row, rpc, SessionCommandOptions::default()).await?;
            Ok(())
        })
    }

    fn listed(&self, archived: bool) -> BoxFuture<ControlResult<ThreadResultSummary>> {
        let mut next = self.row.clone();
        next.archived = archived;
        Box::pin(async move { Ok(displayed_thread(&next, None)) })
    }
}

/// [`RemoteControlLocator`] backed by a [`RemoteThreadSurface`], resolving remote addresses and returning
/// [`RelayControl`] handles over a [`with_closed_rows`] ledger.
pub struct RelayControlLocator {
    remote: Arc<RemoteThreadSurface>,
}

impl RelayControlLocator {
    pub fn new(remote: Arc<RemoteThreadSurface>) -> Self {
        Self {
            remote: with_closed_rows(remote),
        }
    }

    pub fn surface(&self) -> Arc<RemoteThreadSurface> {
        Arc::clone(&self.remote)
    }
}

impl RemoteControlLocator for RelayControlLocator {
    fn locate(
        &self,
        address: String,
        all_scope: bool,
        caller_id: String,
    ) -> BoxFuture<Option<LocatedControl>> {
        let remote = Arc::clone(&self.remote);
        Box::pin(async move {
            let ctx = ResolveContext {
                remote: Some(remote.as_ref()),
                caller_workspace_root: "",
                local: &[],
                local_for: None,
                caller_id: &caller_id,
            };
            let opts = ResolveOptions {
                all_scope,
                probe: true,
                need_online: true,
                ..ResolveOptions::default()
            };
            match resolve_address(&ctx, &address, &opts).await {
                Ok(resolved) => match resolved.where_ {
                    ResolvedWhere::Device { row } => {
                        if row.state == "ended" {
                            Some(LocatedControl::NotLive {
                                thread_id: row_key(&keyed(&row)),
                                title: escape_for_display(&row.title),
                                device: Some(row.device.clone()),
                            })
                        } else {
                            Some(LocatedControl::Control(Arc::new(create_relay_control(
                                remote,
                                (*row).clone(),
                            ))))
                        }
                    }
                    ResolvedWhere::Here { .. } => None,
                },
                Err(refusal) => {
                    let has_device = refusal
                        .details
                        .as_ref()
                        .and_then(|details| details.get("device"))
                        .is_some();
                    if refusal.code.as_str() == "not_found" && !has_device {
                        None
                    } else {
                        Some(LocatedControl::Error(*refusal))
                    }
                }
            }
        })
    }

    fn remote_surface(&self) -> Option<&RemoteThreadSurface> {
        Some(self.remote.as_ref())
    }

    fn remote_surface_arc(&self) -> Option<Arc<RemoteThreadSurface>> {
        Some(Arc::clone(&self.remote))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use futures::executor::block_on;

    use super::*;
    use crate::thread::control::operations::{close_options, close_thread_with_options};
    use crate::thread::remote::ports::{
        ClientDeviceRow, RemoteClient, RemoteDevice, RemoteFailure, RemoteListing,
        RemoteLocateOutcome, RemoteSessionsPort, ServiceStatus,
    };

    struct ScriptedRemote {
        rows: Mutex<Vec<RemoteSessionRow>>,
        replies: Mutex<VecDeque<RemoteCommandOutcome>>,
        sent: Mutex<Vec<(String, String, Value, SessionCommandOptions)>>,
    }

    impl ScriptedRemote {
        fn new(rows: Vec<RemoteSessionRow>, replies: Vec<RemoteCommandOutcome>) -> Arc<Self> {
            Arc::new(Self {
                rows: Mutex::new(rows),
                replies: Mutex::new(replies.into()),
                sent: Mutex::new(Vec::new()),
            })
        }
    }

    impl RemoteClient for ScriptedRemote {
        fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>> {
            Box::pin(async {
                Ok(vec![
                    ClientDeviceRow {
                        id: "here".to_owned(),
                        name: "This".to_owned(),
                        online: true,
                        is_self: true,
                        last_seen_ms: None,
                    },
                    ClientDeviceRow {
                        id: "b".to_owned(),
                        name: "Laptop".to_owned(),
                        online: true,
                        is_self: false,
                        last_seen_ms: None,
                    },
                ])
            })
        }

        fn service_status(&self) -> BoxFuture<Result<ServiceStatus, RemoteFailure>> {
            Box::pin(async {
                Ok(ServiceStatus {
                    state: "connected".to_owned(),
                    device_id: Some("here".to_owned()),
                })
            })
        }
    }

    impl RemoteSessionsPort for ScriptedRemote {
        fn devices(&self) -> BoxFuture<Result<Vec<RemoteDevice>, String>> {
            Box::pin(async {
                Ok(vec![RemoteDevice {
                    device: "b".to_owned(),
                    label: "Laptop".to_owned(),
                    online: true,
                    platform: Some("darwin".to_owned()),
                    kind: Some("laptop".to_owned()),
                    last_seen_ms: None,
                }])
            })
        }

        fn sessions(
            &self,
            _device: Option<String>,
        ) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
            let rows = self
                .rows
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            Box::pin(async move { Ok(rows) })
        }

        fn listing(&self, _device: Option<String>) -> BoxFuture<Result<RemoteListing, String>> {
            let rows = self
                .rows
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            Box::pin(async move {
                Ok(RemoteListing {
                    rows,
                    incomplete: Vec::new(),
                    devices: None,
                })
            })
        }

        fn locate(&self, _device: String, _engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
            Box::pin(async {
                RemoteLocateOutcome::Refused {
                    reason: "unknown_session".to_owned(),
                }
            })
        }

        fn session_command(
            &self,
            device: String,
            session_id: String,
            rpc: Value,
            options: SessionCommandOptions,
        ) -> BoxFuture<RemoteCommandOutcome> {
            self.sent
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((device, session_id, rpc, options));
            let next = self
                .replies
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or_else(|| RemoteCommandOutcome::Ok {
                    response: json!({ "success": true, "data": {} }),
                    local: false,
                    located: None,
                });
            Box::pin(async move { next })
        }
    }

    fn remote_row() -> RemoteSessionRow {
        RemoteSessionRow {
            device: "b".to_owned(),
            session_id: "rk-b".to_owned(),
            durable_id: Some("dur-b".to_owned()),
            title: "Remote".to_owned(),
            project: "/work/b".to_owned(),
            path: "/work/b".to_owned(),
            state: "running".to_owned(),
            updated_ms: 1_000,
            ..RemoteSessionRow::default()
        }
    }

    #[test]
    fn remote_graceful_close_caps_wait_at_remote_max_and_stamps_closed_rows() {
        let remote = ScriptedRemote::new(
            vec![remote_row()],
            vec![
                RemoteCommandOutcome::Ok {
                    response: json!({
                        "success": true,
                        "data": { "capabilities": ["unload_session", "unload_graceful"] }
                    }),
                    local: false,
                    located: None,
                },
                RemoteCommandOutcome::Ok {
                    response: json!({
                        "success": true,
                        "data": {
                            "outcome": "unloaded",
                            "sessionPath": "/b/dur-b.jsonl",
                            "effect": { "updatedAt": "2026-11-26T00:00:02.000Z" },
                            "graceful": { "turn": 3, "waited_ms": 120, "stopped": { "wake_sources": ["bash"] } },
                            "dropped": { "deliveries": [], "user_messages": [] }
                        }
                    }),
                    local: false,
                    located: None,
                },
            ],
        );
        let surface = with_closed_rows(Arc::new(RemoteThreadSurface::new(
            remote.clone(),
            remote.clone(),
            None,
        )));
        let control = create_relay_control(surface.clone(), remote_row());

        let result = block_on(close_thread_with_options(
            &control,
            close_options(None, Some(true), Some(60_000)),
        ))
        .expect("close result");
        assert_eq!(result["kind"], "ok");
        assert_eq!(result["closed"], true);
        assert_eq!(result["device"], "b");
        assert_eq!(result["graceful"]["turn"], 3);

        let sent = remote.sent.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(sent[1].2["waitMs"], REMOTE_GRACEFUL_MAX_WAIT_MS);
        assert_eq!(sent[1].3.wait_ms, Some(REMOTE_GRACEFUL_MAX_WAIT_MS));

        let listed = block_on(surface.sessions.listing(None)).expect("listing");
        assert_eq!(listed.rows[0].state, "ended");
        assert_eq!(
            listed.rows[0].session_file.as_deref(),
            Some("/b/dur-b.jsonl")
        );
    }

    #[test]
    fn get_model_state_reads_fresh_and_session_gone_stamps_closed_rows() {
        let remote = ScriptedRemote::new(
            vec![remote_row()],
            vec![
                RemoteCommandOutcome::Ok {
                    response: json!({
                        "success": true,
                        "data": {
                            "model": { "provider": "openai", "id": "gpt-6" },
                            "thinkingLevel": "high"
                        }
                    }),
                    local: false,
                    located: None,
                },
                RemoteCommandOutcome::Refused {
                    reason: "route_unlisted".to_owned(),
                    detail: None,
                },
            ],
        );
        let surface = with_closed_rows(Arc::new(RemoteThreadSurface::new(
            remote.clone(),
            remote.clone(),
            None,
        )));
        let control = create_relay_control(surface.clone(), remote_row());

        let model_state = block_on(control.get_model_state())
            .expect("model state")
            .expect("present");
        assert_eq!(model_state.model.provider, "openai");
        assert_eq!(model_state.thinking_level, "high");
        assert!(
            remote.sent.lock().unwrap_or_else(PoisonError::into_inner)[0]
                .3
                .fresh
        );

        let err = block_on(control.set_name("renamed".to_owned())).expect_err("gone");
        assert_eq!(
            err.refusal().expect("refusal").code.as_str(),
            "not_resumable"
        );
        let listed = block_on(surface.sessions.sessions(None)).expect("sessions");
        assert_eq!(listed[0].state, "ended");
    }
}
