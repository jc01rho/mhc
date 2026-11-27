// allow: SIZE_OK — complete port of upstream control/relay-session-host.ts (open, adopt, finish, unreachable, unload M5)
//! Another device's engine endpoint as a `SessionHost`, plus remote listing row formatting
//! (omo `thread/control/relay-session-host.ts`; #14560, #14569, #14623 review M4/M5, #14394, #14397).

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::address::resolve_typed_device;
use crate::thread::address_book::HostSessionStatus;
use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::contracts_results::{ThreadResultSummary, thread_link};
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::host_view::ThreadHostSession;
use crate::thread::initial_skills::INITIAL_SKILLS_CONTEXT_KEY;
use crate::thread::remote::address_book::relay_address_entry;
use crate::thread::remote::outcome::relay_outcome_failure;
use crate::thread::remote::ports::{
    RemoteCommandOutcome, RemoteLocateOutcome, RemoteSessionRow, RemoteThreadSurface, RowSource,
    SessionCommandOptions, escape_for_display, iso_from_ms, keyed, row_created_at, row_folder,
    row_name_of, row_state, row_status,
};
use crate::thread::remote::remote_ops::{not_signed_in_failure, reply_data};
use crate::thread::thread_state::retry_wait;
use crate::thread::types::GatewayEndpointKind;
use crate::thread::worktree::remote_worktree::{prepare_remote_worktree, remove_remote_worktree};
use crate::thread::worktree::thread_worktree::ThreadWorktree;

use super::open_reply::{open_refusal, opened_engine_id};
use super::port::{
    ControlError, ControlResult, SessionAdoptRequest, SessionHost, SessionOpenOptions,
    SessionOpenRequest, refuse,
};
use super::session_host::{OPEN_SESSION_FORK, opened_thread};

const ROUTE_UNLISTED: &str = "route_unlisted";

fn non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn record_of(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// A remote listing row shaped into a `ThreadResultSummary`, before display escaping (`listedThread`).
pub fn listed_thread(row: &RemoteSessionRow, machine: Option<&str>) -> ThreadResultSummary {
    let thread = maho_thread_address_core::row_key(&keyed(row));
    let shown = row_name_of(row);
    let as_of = (row.source == Some(RowSource::Cache))
        .then_some(row.as_of)
        .flatten()
        .map(iso_from_ms);
    let (waiting_on, engine_version) = if row.source == Some(RowSource::Cache) {
        (None, None)
    } else {
        (
            retry_wait(row.state == "running", row.waiting.as_ref()),
            row.engine_version.clone(),
        )
    };
    ThreadResultSummary {
        link: thread_link(&thread, &shown.name),
        thread,
        name: shown.name,
        unnamed: shown.unnamed.then_some(true),
        status: row_status(row),
        state: row_state(row),
        model: row.model.clone(),
        activity: None,
        error_code: None,
        turn_id: None,
        waiting_on,
        engine_version,
        backlog: None,
        as_of,
        machine: machine.map(str::to_owned),
        cwd: row_folder(row).to_owned(),
        created_at: row_created_at(row),
        updated_at: Some(iso_from_ms(row.updated_ms)),
        device: Some(row.device.clone()),
        alias: None,
        surface: None,
        controls: None,
        error_note: None,
        skills: None,
        worktree: None,
        warnings: None,
    }
}

/// A remote listing row with display-unsafe characters escaped (`displayedThread`, #13304, #14305).
pub fn displayed_thread(row: &RemoteSessionRow, machine: Option<&str>) -> ThreadResultSummary {
    let mut listed = listed_thread(row, machine);
    let name = escape_for_display(&listed.name);
    listed.cwd = escape_for_display(&listed.cwd);
    listed.machine = listed.machine.map(|label| escape_for_display(&label));
    listed.link = thread_link(&listed.thread, &name);
    listed.name = name;
    listed
}

#[derive(Clone)]
pub struct FinishStep {
    pub name: Option<String>,
    pub reopens: bool,
    pub confirm_reach: bool,
    pub located: Option<String>,
    pub cwd: Option<String>,
    pub worktree: Option<ThreadWorktree>,
}

/// Another device's engine endpoint as a `SessionHost` (`createRelaySessionHost`).
#[derive(Clone)]
pub struct RelaySessionHost {
    remote: Arc<RemoteThreadSurface>,
    device: String,
    listed: Option<RemoteSessionRow>,
}

pub fn create_relay_session_host(
    remote: Arc<RemoteThreadSurface>,
    device: impl Into<String>,
    listed: Option<RemoteSessionRow>,
) -> RelaySessionHost {
    RelaySessionHost {
        remote,
        device: device.into(),
        listed,
    }
}

impl RelaySessionHost {
    pub async fn unreachable(
        &self,
        reason: &str,
        routing_id: Option<&str>,
        thread: &str,
        worktree: Option<&ThreadWorktree>,
    ) -> ControlError {
        let ended = match routing_id {
            Some(id) => Some(
                self.remote
                    .sessions
                    .endpoint_command(
                        self.device.clone(),
                        json!({
                            "type": "unload_session",
                            "sessionId": id,
                            "force": true,
                            "children": "detach",
                        }),
                    )
                    .await,
            ),
            None => None,
        };
        let created = !matches!(
            &ended,
            Some(RemoteCommandOutcome::Ok { response, .. })
                if reply_data(response).get("outcome").and_then(Value::as_str) == Some("unloaded")
        );
        // #14623 review M5: remove the worktree only once b's engine confirms the session is unloaded.
        let left = match (worktree, created) {
            (Some(wt), false) => remove_remote_worktree(&self.remote, &self.device, wt).await,
            _ => None,
        };
        let shown = escape_for_display(reason);
        let device = &self.device;
        let session_msg = if created {
            if routing_id.is_none() {
                format!(
                    "Device {device} made thread {thread}, but its route does not reach it ({shown}), and its open named no routing id to end it by: it is left with no live owner."
                )
            } else {
                format!(
                    "Device {device} made thread {thread}, but its route does not reach it ({shown}), and ending it there failed: it is left with no live owner."
                )
            }
        } else {
            format!(
                "Device {device} took the open, but its route does not reach the session it made ({shown}): no thread host there that this account reaches takes new threads."
            )
        };
        let kept = worktree.is_some() && (created || left.is_some());
        let remains = match (kept, worktree) {
            (true, Some(wt)) => format!(
                " The worktree {} made for it is still on that device.",
                wt.path
            ),
            _ if created => String::new(),
            _ => " Nothing was left behind.".to_owned(),
        };
        let mut details = Map::new();
        details.insert("device".to_owned(), json!(device));
        details.insert("reason".to_owned(), json!(shown));
        details.insert("created".to_owned(), json!(created));
        if created {
            details.insert("thread".to_owned(), json!(thread));
            if routing_id.is_none() {
                details.insert("unload".to_owned(), json!("no_routing_id"));
            }
        }
        if kept && let Some(wt) = worktree {
            details.insert(
                "worktree".to_owned(),
                serde_json::to_value(wt).unwrap_or(Value::Null),
            );
            if let Some(cleanup) = left {
                details.insert(
                    "cleanup".to_owned(),
                    serde_json::to_value(cleanup).unwrap_or(Value::Null),
                );
            }
        }
        refuse(
            "host_unavailable",
            format!("{session_msg}{remains}"),
            "Start omo on that device (an omo session, or omo daemon run), or create the thread on another device.",
            Some(Value::Object(details)),
        )
    }

    pub async fn held_under(&self, durable: Option<&str>) -> Option<String> {
        let durable = durable?;
        let listed = self
            .remote
            .sessions
            .endpoint_command(self.device.clone(), json!({ "type": "list_sessions" }))
            .await;
        let RemoteCommandOutcome::Ok { response, .. } = listed else {
            return None;
        };
        let data = reply_data(&response);
        let rows = data.get("sessions").and_then(Value::as_array)?;
        for entry in rows {
            let obj = entry.as_object()?;
            let entry_durable =
                non_empty(obj.get("durableSessionId")).or_else(|| non_empty(obj.get("durable_id")));
            if entry_durable.as_deref() == Some(durable) {
                return non_empty(obj.get("routing_id"))
                    .or_else(|| non_empty(obj.get("sessionId")))
                    .or_else(|| non_empty(obj.get("session_id")));
            }
        }
        None
    }

    pub async fn finish(
        &self,
        data: &Map<String, Value>,
        step: FinishStep,
    ) -> ControlResult<(ThreadResultSummary, bool)> {
        let state = record_of(data.get("state"));
        let mut warnings: Vec<ThreadToolFailure> = Vec::new();
        let locate_with_hint = |engine_id: String, hint: Option<String>| {
            let remote = Arc::clone(&self.remote);
            let device = self.device.clone();
            async move {
                if let Some(known) = hint {
                    return RemoteLocateOutcome::Located { session_id: known };
                }
                remote.sessions.locate(device, engine_id).await
            }
        };
        let mut thread_id = if step.reopens {
            let engine_id = opened_engine_id(data, &state)
                .or_else(|| self.listed.as_ref().and_then(|l| l.durable_id.clone()));
            let located = match engine_id {
                Some(id) => locate_with_hint(id, step.located.clone()).await,
                None => RemoteLocateOutcome::Refused {
                    reason: "unnamed_session".to_owned(),
                },
            };
            match located {
                RemoteLocateOutcome::Located { session_id } => {
                    if let Some(ref listed) = self.listed {
                        self.remote.sessions.reopened(listed, &session_id);
                    }
                    session_id
                }
                RemoteLocateOutcome::Refused { reason } => {
                    let shown = escape_for_display(&reason);
                    return Err(refuse(
                        "not_resumable",
                        format!(
                            "Device {} opened the thread's file, but the reopened session cannot be reached there ({shown}).",
                            self.device
                        ),
                        "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell), then open the thread by the id it lists.",
                        Some(json!({ "device": self.device, "reason": shown })),
                    ));
                }
            }
        } else {
            let Some(engine_id) = opened_engine_id(data, &state) else {
                return Err(refuse(
                    "internal_error",
                    format!(
                        "Device {} opened a session but did not name it.",
                        self.device
                    ),
                    "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to find the new thread.",
                    Some(json!({ "device": self.device })),
                ));
            };
            match locate_with_hint(engine_id.clone(), step.located.clone()).await {
                RemoteLocateOutcome::Located { session_id } => session_id,
                RemoteLocateOutcome::Refused { reason } => {
                    let warning = not_signed_in_failure(&reason, &self.device).map_or_else(
                        || {
                            let shown = escape_for_display(&reason);
                            thread_tool_failure(
                                "unsupported",
                                format!(
                                    "Device {} created the thread but did not name its relay key ({shown}).",
                                    self.device
                                ),
                                "Address it by its thread value once thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) lists it, or update omo on that device.",
                                Some(json!({ "device": self.device, "reason": shown })),
                            )
                        },
                        |f| *f,
                    );
                    warnings.push(warning);
                    engine_id
                }
            }
        };

        let route_step = |key: String, name: Option<String>, reopens: bool, confirm_reach: bool| {
            let remote = Arc::clone(&self.remote);
            let device = self.device.clone();
            async move {
                let direct_opts = SessionCommandOptions {
                    direct: true,
                    ..SessionCommandOptions::default()
                };
                if let Some(name) = name {
                    Some(
                        remote
                            .sessions
                            .session_command(
                                device,
                                key.clone(),
                                json!({ "type": "set_session_name", "sessionId": key, "name": name }),
                                direct_opts,
                            )
                            .await,
                    )
                } else if !reopens && confirm_reach {
                    Some(
                        remote
                            .sessions
                            .session_command(
                                device,
                                key.clone(),
                                json!({ "type": "get_state", "sessionId": key }),
                                direct_opts,
                            )
                            .await,
                    )
                } else {
                    None
                }
            }
        };

        let mut reached = route_step(
            thread_id.clone(),
            step.name.clone(),
            step.reopens,
            step.confirm_reach,
        )
        .await;
        let mut route_ok = matches!(reached, Some(RemoteCommandOutcome::Ok { .. }));

        if !step.reopens
            && matches!(&reached, Some(RemoteCommandOutcome::Refused { reason, .. }) if reason == ROUTE_UNLISTED)
        {
            if let Some(engine_id) = opened_engine_id(data, &state)
                && let RemoteLocateOutcome::Located { session_id } = self
                    .remote
                    .sessions
                    .locate(self.device.clone(), engine_id)
                    .await
            {
                thread_id = session_id;
                reached = route_step(
                    thread_id.clone(),
                    step.name.clone(),
                    step.reopens,
                    step.confirm_reach,
                )
                .await;
                if matches!(reached, Some(RemoteCommandOutcome::Ok { .. })) {
                    route_ok = true;
                }
            }
            if let Some(RemoteCommandOutcome::Refused { reason, .. }) = &reached
                && reason == ROUTE_UNLISTED
            {
                let durable_in_state = non_empty(state.get("durableSessionId"))
                    .or_else(|| non_empty(data.get("durableSessionId")))
                    .or_else(|| non_empty(data.get("durable_id")));
                let mut routing_id =
                    non_empty(data.get("routing_id")).or_else(|| non_empty(data.get("sessionId")));
                if routing_id.is_none() {
                    routing_id = self.held_under(durable_in_state.as_deref()).await;
                }
                let thread_name = durable_in_state.as_deref().unwrap_or(&thread_id);
                return Err(self
                    .unreachable(
                        reason,
                        routing_id.as_deref(),
                        thread_name,
                        step.worktree.as_ref(),
                    )
                    .await);
            }
        }

        let mut applied = step.name.clone();
        if step.name.is_some()
            && let Some(ref named_outcome) = reached
            && !matches!(named_outcome, RemoteCommandOutcome::Ok { .. })
        {
            applied = None;
            let mut failed = *relay_outcome_failure(named_outcome, &self.device);
            let mut details = failed
                .details
                .take()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let cause = details.get("reason").cloned();
            details.insert("reason".to_owned(), json!("name_not_applied"));
            if let Some(cause) = cause {
                details.insert("cause".to_owned(), cause);
            }
            failed.details = Some(Value::Object(details));
            warnings.push(failed);
        }

        let durable = non_empty(state.get("durableSessionId"))
            .or_else(|| non_empty(data.get("durableSessionId")))
            .or_else(|| non_empty(data.get("durable_id")))
            .or_else(|| non_empty(state.get("sessionId")))
            .or_else(|| self.listed.as_ref().and_then(|l| l.durable_id.clone()));
        let raw_cwd = non_empty(state.get("cwd"))
            .or(step.cwd)
            .or_else(|| self.listed.as_ref().map(|l| row_folder(l).to_owned()))
            .unwrap_or_default();
        let fallback_name = non_empty(state.get("sessionName"))
            .or_else(|| {
                self.listed
                    .as_ref()
                    .map(|l| l.title.clone())
                    .filter(|t| !t.is_empty())
            })
            .map(|n| escape_for_display(&n));
        let session = ThreadHostSession {
            session_id: thread_id,
            durable_session_id: durable,
            session_path: None,
            cwd: escape_for_display(&raw_cwd),
            name: applied.or(fallback_name),
            status: Some(HostSessionStatus::Open),
            kind: None,
            archived: false,
            activity: None,
            error_code: None,
            turn_id: None,
            model: None,
            waiting: None,
            engine_version: None,
            created_at: non_empty(state.get("createdAt"))
                .or_else(|| non_empty(state.get("created_at"))),
            updated_at: non_empty(state.get("updatedAt"))
                .or_else(|| non_empty(state.get("updated_at"))),
            endpoint_kind: GatewayEndpointKind::RpcHost,
            controls: Vec::new(),
            socket: String::new(),
        };
        let now = iso_from_ms(call_budget::now_ms());
        let mut row = opened_thread(&session, &now);
        row.device = Some(self.device.clone());
        row.worktree = step.worktree;
        if !warnings.is_empty() {
            row.warnings = Some(warnings);
        }
        Ok((row, route_ok))
    }

    pub async fn adopted(
        &self,
        durable_id: &str,
        step: FinishStep,
    ) -> ControlResult<Option<ThreadResultSummary>> {
        let Ok(rows) = self
            .remote
            .sessions
            .sessions(Some(self.device.clone()))
            .await
        else {
            return Ok(None);
        };
        let Some(row) = rows.into_iter().find(|candidate| {
            candidate.device == self.device
                && candidate.durable_id.as_deref() == Some(durable_id)
                && candidate.state != "ended"
                && candidate.source != Some(RowSource::Cache)
        }) else {
            return Ok(None);
        };
        let probe = self
            .remote
            .sessions
            .session_command(
                self.device.clone(),
                row.session_id.clone(),
                json!({ "type": "get_state", "sessionId": row.session_id }),
                SessionCommandOptions {
                    direct: true,
                    ..SessionCommandOptions::default()
                },
            )
            .await;
        let is_unlisted = matches!(
            &probe,
            RemoteCommandOutcome::Refused { reason, .. } if reason == ROUTE_UNLISTED
        );
        if !matches!(probe, RemoteCommandOutcome::Ok { .. }) && !is_unlisted {
            return Ok(None);
        }
        let cwd = row_folder(&row).to_owned();
        let mut state = Map::new();
        state.insert("durableSessionId".to_owned(), json!(durable_id));
        state.insert("cwd".to_owned(), json!(cwd));
        if !row.title.is_empty() {
            state.insert("sessionName".to_owned(), json!(row.title));
        }
        let mut data = Map::new();
        data.insert("state".to_owned(), Value::Object(state));
        let observed = matches!(probe, RemoteCommandOutcome::Ok { .. });
        let (done, route_ok) = self
            .finish(
                &data,
                FinishStep {
                    reopens: false,
                    located: Some(row.session_id),
                    cwd: Some(cwd),
                    ..step
                },
            )
            .await?;
        Ok((observed || route_ok).then_some(done))
    }
}

impl SessionHost for RelaySessionHost {
    fn device(&self) -> Option<&str> {
        Some(&self.device)
    }

    fn supports_adopt(&self) -> bool {
        true
    }

    fn visible(&self) -> BoxFuture<ControlResult<Vec<ThreadAddressEntry>>> {
        let remote = Arc::clone(&self.remote);
        let device = self.device.clone();
        Box::pin(async move {
            match remote.sessions.sessions(None).await {
                Ok(rows) => Ok(rows
                    .iter()
                    .filter(|row| row.device == device && row.state != "ended")
                    .map(|row| relay_address_entry(row, &[]))
                    .collect()),
                Err(error) => Err(refuse(
                    "host_unavailable",
                    format!("Device {device}'s threads could not be listed."),
                    "Retry when that device is reachable.",
                    Some(json!({ "device": device, "reason": error })),
                )),
            }
        })
    }

    fn forks(&self) -> BoxFuture<ControlResult<bool>> {
        let remote = Arc::clone(&self.remote);
        let device = self.device.clone();
        Box::pin(async move {
            let info = remote
                .sessions
                .endpoint_command(
                    device,
                    json!({ "type": "get_protocol_info", "observe": true }),
                )
                .await;
            let RemoteCommandOutcome::Ok { response, .. } = info else {
                return Ok(false);
            };
            Ok(reply_data(&response)
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|names| {
                    names
                        .iter()
                        .any(|name| name.as_str() == Some(OPEN_SESSION_FORK))
                }))
        })
    }

    fn open(
        &self,
        request: SessionOpenRequest,
        options: SessionOpenOptions,
    ) -> BoxFuture<ControlResult<ThreadResultSummary>> {
        let host = self.clone();
        Box::pin(async move {
            let resolved = resolve_typed_device(host.remote.client.as_ref(), &host.device, false)
                .await
                .map_err(|f| ControlError::Refused(Box::new(f)))?;
            if resolved.device != host.device {
                return Err(refuse(
                    "not_found",
                    format!(
                        "No device named {} is signed in to this account.",
                        host.device
                    ),
                    "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to see the account's devices and threads.",
                    Some(json!({ "device": host.device })),
                ));
            }
            let made_worktree = match &request.worktree {
                Some(wt_req) => Some(
                    prepare_remote_worktree(
                        &host.remote,
                        &host.device,
                        request.cwd.as_deref().unwrap_or(""),
                        wt_req,
                    )
                    .await
                    .map_err(ControlError::Refused)?,
                ),
                None => None,
            };
            let open_cwd = made_worktree
                .as_ref()
                .map(|wt| wt.path.clone())
                .or_else(|| request.cwd.clone());
            let mut frame = Map::new();
            frame.insert("type".to_owned(), json!("open_session"));
            frame.insert("retain_on_disconnect".to_owned(), json!(true));
            if let Some(ref cwd) = open_cwd {
                frame.insert("cwd".to_owned(), json!(cwd));
            }
            if let Some(ref path) = request.session_path {
                frame.insert("sessionPath".to_owned(), json!(path));
            }
            if let Some(ref remote_session) = request.remote_session {
                frame.insert("session".to_owned(), json!(remote_session));
            }
            if let Some(ref fork_from) = request.fork_from {
                frame.insert("forkFrom".to_owned(), json!(fork_from));
            }
            if let Some(ref provider) = request.provider {
                frame.insert("provider".to_owned(), json!(provider));
            }
            if let Some(ref model_id) = request.model_id {
                frame.insert("modelId".to_owned(), json!(model_id));
            }
            if let Some(ref level) = request.thinking_level {
                frame.insert("thinkingLevel".to_owned(), json!(level));
            }
            if let Some(ref durable) = request.durable_session_id {
                frame.insert("durableSessionId".to_owned(), json!(durable));
            }
            if !request.initial_skills.is_empty() {
                let encoded = serde_json::to_string(&request.initial_skills).unwrap_or_default();
                frame.insert(
                    "context".to_owned(),
                    json!({ INITIAL_SKILLS_CONTEXT_KEY: encoded }),
                );
            }
            if let Some(ref preset) = request.permission_preset {
                frame.insert("permissionPreset".to_owned(), json!(preset));
            }
            let reopens = request.session_path.is_some() || request.remote_session.is_some();
            let outcome = host
                .remote
                .sessions
                .endpoint_command(host.device.clone(), Value::Object(frame))
                .await;
            let (response, located) = match outcome {
                RemoteCommandOutcome::Ok {
                    response, located, ..
                } => (response, located),
                other => {
                    if matches!(other, RemoteCommandOutcome::Uncertain { .. })
                        && !reopens
                        && let Some(durable_id) = request.durable_session_id.as_deref()
                        && let Some(found) = host
                            .adopted(
                                durable_id,
                                FinishStep {
                                    name: request.name.clone(),
                                    reopens: false,
                                    confirm_reach: options.confirm_reach,
                                    located: None,
                                    cwd: open_cwd.clone(),
                                    worktree: made_worktree.clone(),
                                },
                            )
                            .await?
                    {
                        return Ok(found);
                    }
                    let model = request.provider.as_ref().map(|p| {
                        format!("{p}/{}", request.model_id.as_deref().unwrap_or_default())
                    });
                    let mut refused = open_refusal(
                        &other,
                        &host.device,
                        open_cwd.as_deref(),
                        model.as_deref(),
                        request.fork_from.as_deref(),
                    );
                    if let Some(wt) = made_worktree {
                        let left = if matches!(other, RemoteCommandOutcome::Refused { .. }) {
                            remove_remote_worktree(&host.remote, &host.device, &wt).await
                        } else {
                            None
                        };
                        let keep = !matches!(other, RemoteCommandOutcome::Refused { .. })
                            || left.is_some();
                        if keep {
                            let mut details = refused
                                .details
                                .take()
                                .and_then(|v| v.as_object().cloned())
                                .unwrap_or_default();
                            details.insert(
                                "worktree".to_owned(),
                                serde_json::to_value(&wt).unwrap_or(Value::Null),
                            );
                            if let Some(cleanup) = left {
                                details.insert(
                                    "cleanup".to_owned(),
                                    serde_json::to_value(cleanup).unwrap_or(Value::Null),
                                );
                            }
                            refused.details = Some(Value::Object(details));
                        }
                    }
                    return Err(ControlError::Refused(Box::new(refused)));
                }
            };
            let (row, _) = host
                .finish(
                    &reply_data(&response),
                    FinishStep {
                        name: request.name,
                        reopens,
                        confirm_reach: options.confirm_reach,
                        located,
                        cwd: open_cwd,
                        worktree: made_worktree,
                    },
                )
                .await?;
            Ok(row)
        })
    }

    fn adopt(
        &self,
        request: SessionAdoptRequest,
        options: SessionOpenOptions,
    ) -> BoxFuture<ControlResult<Option<ThreadResultSummary>>> {
        let host = self.clone();
        Box::pin(async move {
            host.adopted(
                &request.durable_session_id,
                FinishStep {
                    name: request.name,
                    reopens: false,
                    confirm_reach: options.confirm_reach,
                    located: None,
                    cwd: None,
                    worktree: request.worktree,
                },
            )
            .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::contracts_results::ThreadStatus;
    use crate::thread::contracts_state::ThreadState;

    #[test]
    fn displayed_thread_uses_row_identity_and_escapes_untrusted_text() {
        let row = RemoteSessionRow {
            device: "b".to_owned(),
            session_id: "rk-1".to_owned(),
            durable_id: None,
            title: "p\u{202e}q\nr\x1b[31ms\u{200d}t".to_owned(),
            project: "/work/proj".to_owned(),
            path: "/work/a\nb".to_owned(),
            state: "idle".to_owned(),
            updated_ms: 1_700_000_000_000,
            ..RemoteSessionRow::default()
        };
        let displayed = displayed_thread(&row, Some("Mac\u{202e}Book"));
        assert_eq!(displayed.thread, "b/rk-1");
        assert!(!displayed.name.contains('\n'));
        assert!(displayed.name.contains("\\u202e"));
        assert_eq!(displayed.cwd, "/work/a\\u000ab");
        assert_eq!(displayed.machine.as_deref(), Some("Mac\\u202eBook"));
        assert!(displayed.link.contains("omo-thread://v1/b/rk-1"));
        assert!(!displayed.link.contains('\n'));
        assert_eq!(displayed.status, ThreadStatus::Live);
        assert_eq!(displayed.state, ThreadState::Idle);
    }

    #[test]
    fn cached_row_reports_inactive_status_and_as_of_iso() {
        let row = RemoteSessionRow {
            device: "b".to_owned(),
            session_id: "rk-1".to_owned(),
            durable_id: Some("01932bf3-0280-7abc-8def-0123456789ab".to_owned()),
            title: "cached".to_owned(),
            project: "/work/proj".to_owned(),
            path: "unknown".to_owned(),
            state: "running".to_owned(),
            updated_ms: 1_700_000_000_000,
            source: Some(RowSource::Cache),
            as_of: Some(1_700_000_050_000),
            ..RemoteSessionRow::default()
        };
        let displayed = displayed_thread(&row, None);
        assert_eq!(displayed.thread, "01932bf3-0280-7abc-8def-0123456789ab");
        assert_eq!(displayed.status, ThreadStatus::Resumable);
        assert_eq!(displayed.state, ThreadState::Inactive);
        assert_eq!(displayed.cwd, "/work/proj");
        assert_eq!(displayed.as_of.as_deref(), Some("2023-11-14T22:14:10.000Z"));
    }
}
