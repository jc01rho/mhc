//! omo `thread/remote/monitor-source.ts`: senpi's `RpcRemoteSource` over the thread tools' remote
//! surface. `monitor({type:"rpc", target:"<device>/<session>"})` follows that session through
//! `RemoteSessionsPort::watch`; `<device>/*` follows the device's listing rows.
use std::sync::{Arc, Mutex};

use maho_ext_api::{
    RpcRemoteHandlers, RpcRemoteOpenOptions, RpcRemoteSource, RpcRemoteState,
    RpcRemoteSubscription, RpcRemoteTarget,
};
use maho_thread_address_core::{ResolveDeviceOptions, format_address, id_shaped, parse_address};
use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::address::account::account_devices;
use crate::thread::address::device::{resolve_account_device, resolve_typed_device};
use crate::thread::address::resolve_address;
use crate::thread::address::types::{ResolveContext, ResolveOptions};
use crate::thread::remote::ports::{
    RemoteSessionRow, RemoteSessionState, RemoteThreadSurface, RemoteWatchHandlers,
    RemoteWatchOptions, RemoteWatchOutcome,
};
use crate::thread::remote::remote_ops::remote_ready;

const LISTING_ROUND_MS: u64 = 30_000;
const LISTED_STATUS: [(&str, &str); 3] = [
    ("running", "running"),
    ("idle", "idle"),
    ("ended", "closed"),
];
const TRANSIENT: [&str; 2] = ["device_offline", "host_unavailable"];
const UNKNOWN_CALLER: &str = "";

fn row_of(label: &str, state: &RemoteSessionState) -> Value {
    let mut row = Map::new();
    row.insert("sessionId".into(), Value::String(label.to_owned()));
    row.insert("status".into(), Value::String(state.status.clone()));
    if let Some(durable) = &state.durable_session_id {
        row.insert("durableSessionId".into(), Value::String(durable.clone()));
    }
    Value::Object(row)
}

pub fn create_remote_monitor_source(remote: Arc<RemoteThreadSurface>) -> RpcRemoteSource {
    RpcRemoteSource {
        open: Arc::new(move |target, options, handlers| {
            let remote = Arc::clone(&remote);
            Box::pin(async move { open_source(&remote, target, options, handlers).await })
        }),
    }
}

async fn open_source(
    remote: &Arc<RemoteThreadSurface>,
    target: RpcRemoteTarget,
    options: RpcRemoteOpenOptions,
    handlers: RpcRemoteHandlers,
) -> Result<RpcRemoteSubscription, String> {
    if let Err(unavailable) = remote_ready(remote).await {
        return Err(format!("{}: {}", unavailable.code, unavailable.message));
    }
    if target.session == "*" {
        let device = match resolve_typed_device(remote.client.as_ref(), &target.device, false).await
        {
            Ok(device) => device.device,
            Err(error) => {
                if !TRANSIENT.contains(&error.code.as_str()) {
                    return Err(format!("{}: {}", error.code, error.message));
                }
                let roster = account_devices(remote.client.as_ref()).await.ok();
                stored_device(roster.as_ref(), &target.device)
            }
        };
        return open_device(remote, device, handlers).await;
    }
    let session = session_on(remote, &target.device, &target.session).await?;
    open_session(
        remote,
        &session.device,
        &session.session,
        &target.session,
        options.resume_from.as_deref(),
        handlers,
    )
    .await
}

async fn roster_once(
    remote: &RemoteThreadSurface,
) -> Option<crate::thread::address::device::AccountDevices> {
    account_devices(remote.client.as_ref()).await.ok()
}

fn stored_device(
    roster: Option<&crate::thread::address::device::AccountDevices>,
    device: &str,
) -> String {
    let Some(roster) = roster else {
        return device.to_owned();
    };
    match resolve_account_device(
        roster,
        device,
        ResolveDeviceOptions {
            online: false,
            exact_id: false,
        },
    ) {
        Ok(resolved) => resolved.id,
        Err(_) => device.to_owned(),
    }
}

async fn stored_session(
    remote: &RemoteThreadSurface,
    device: &str,
    thread: &str,
) -> (String, bool) {
    let rows = remote
        .sessions
        .sessions(Some(device.to_owned()))
        .await
        .unwrap_or_default();
    let listed = rows.iter().find(|row| {
        row.device == device
            && (row.session_id == thread || row.durable_id.as_deref() == Some(thread))
    });
    (
        listed.map_or_else(|| thread.to_owned(), |row| row.session_id.clone()),
        listed.is_some(),
    )
}

async fn session_on(
    remote: &RemoteThreadSurface,
    device: &str,
    thread: &str,
) -> Result<(String, String), String> {
    let roster = roster_once(remote).await;
    let here = roster.as_ref().and_then(|roster| roster.here.clone());
    if let Some(here) = here
        && stored_device(roster.as_ref(), device) == here
    {
        let (session, _) = stored_session(remote, &here, thread).await;
        return Ok((here, session));
    }
    let context = ResolveContext {
        remote: Some(remote),
        caller_workspace_root: "",
        local: &[],
        local_for: None,
        caller_id: UNKNOWN_CALLER,
    };
    let opts = ResolveOptions {
        all_scope: true,
        probe: false,
        need_online: true,
        ..ResolveOptions::default()
    };
    match resolve_address(&context, &format_address(device, thread), &opts).await {
        Ok(resolved) => match resolved.where_ {
            crate::thread::address::types::ResolvedWhere::Device { row } => {
                Ok((row.device.clone(), row.session_id.clone()))
            }
            crate::thread::address::types::ResolvedWhere::Here { .. } => Err(format!(
                "not_found: {thread} is a session on this device, which a remote watch does not follow."
            )),
        },
        Err(refusal) => {
            if !TRANSIENT.contains(&refusal.code.as_str()) {
                return Err(format!("{}: {}", refusal.code, refusal.message));
            }
            let (session, listed) =
                stored_session(remote, &stored_device(roster.as_ref(), device), thread).await;
            if !listed && !id_shaped(thread) {
                return Err(format!("{}: {}", refusal.code, refusal.message));
            }
            Ok((stored_device(roster.as_ref(), device), session))
        }
    }
}

async fn open_session(
    remote: &Arc<RemoteThreadSurface>,
    device: &str,
    session_id: &str,
    label: &str,
    resume_from: Option<&str>,
    handlers: RpcRemoteHandlers,
) -> Result<RpcRemoteSubscription, String> {
    let durable: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let saw_status: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let (handlers_record, handlers_status) = (handlers.record.clone(), handlers.status.clone());
    let (handlers_resynced, handlers_lost) = (handlers.resynced.clone(), handlers.lost.clone());
    let (label_record, label_status, label_resync) =
        (label.to_owned(), label.to_owned(), label.to_owned());
    let record_handler = Arc::new(move |record: &Value| {
        let mut record = record.clone();
        if record.get("type").and_then(Value::as_str) == Some("session_status") {
            *saw_status.lock().unwrap_or_else(|e| e.into_inner()) = true;
        }
        record
            .as_object_mut()
            .map(|map| map.insert("sessionId".into(), Value::String(label_record.clone())));
        (handlers_record)(record);
        Ok(())
    });
    let (durable_status, saw_status_status) = (durable.clone(), saw_status.clone());
    let status_handler = Arc::new(
        move |change: &crate::thread::remote::ports::RemoteStatusChange| {
            let (durable, saw) = (
                durable_status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
                *saw_status_status.lock().unwrap_or_else(|e| e.into_inner()),
            );
            if !change.derived || saw {
                return Ok(());
            }
            let mut record = Map::new();
            record.insert("type".into(), Value::String("session_status".into()));
            record.insert("sessionId".into(), Value::String(label_status.clone()));
            record.insert("status".into(), Value::String(change.to.clone()));
            if let Some(durable) = durable {
                record.insert("durableSessionId".into(), Value::String(durable));
            }
            let mut last_change = Map::new();
            last_change.insert(
                "cause".into(),
                Value::String(
                    change
                        .cause
                        .clone()
                        .unwrap_or_else(|| "lifecycle".to_owned()),
                ),
            );
            last_change.insert("by".into(), Value::String("agent".to_owned()));
            record.insert("lastChange".into(), Value::Object(last_change));
            (handlers_status)(&Value::Object(record))?;
            Ok(())
        },
    );
    let (durable_resync, handlers_cursor) = (durable.clone(), handlers.cursor.clone());
    let resync_handler = Arc::new(move |resync: &crate::thread::remote::ports::RemoteResync| {
        if let Some(durable) = &resync.state.durable_session_id {
            *durable_resync.lock().unwrap_or_else(|e| e.into_inner()) = Some(durable.clone());
        }
        let mut rows = Vec::new();
        rows.push(row_of(&label_resync, &resync.state));
        let settled_turns: Vec<maho_ext_api::RpcRemoteSettledTurn> = resync
            .settled_turns
            .iter()
            .map(|turn_id| maho_ext_api::RpcRemoteSettledTurn {
                session_id: label_resync.clone(),
                turn_id: turn_id.clone(),
            })
            .collect();
        let mut state = RpcRemoteState {
            rows,
            ..RpcRemoteState::default()
        };
        if !settled_turns.is_empty() {
            state.settled_turns = Some(settled_turns);
        }
        (handlers_resynced)(state);
        if let Some(cursor) = &handlers_cursor {
            cursor(resync.state.current_turn_id.clone().unwrap_or_default());
        }
        Ok(())
    });
    let lost_handler = Arc::new(move |reason: &str| {
        (handlers_lost)(reason.to_owned());
        Ok(())
    });
    let watch_handlers = RemoteWatchHandlers {
        record: record_handler,
        status: Some(status_handler),
        resynced: Some(resync_handler),
        lost: Some(lost_handler),
    };
    let outcome = remote
        .sessions
        .watch(
            device.to_owned(),
            session_id.to_owned(),
            watch_handlers,
            RemoteWatchOptions { baseline: true },
        )
        .await;
    let watch = match outcome {
        RemoteWatchOutcome::Ok { watch } => watch,
        RemoteWatchOutcome::Refused { reason } => return Err(reason),
    };
    let Some(initial) = watch.initial.clone() else {
        watch.stop();
        return Err("state_unread".to_owned());
    };
    let ended_handlers = handlers.ended.clone();
    tokio::spawn(async move {
        let end = watch.ended.await;
        if end != "stopped" {
            let reason = end.strip_prefix("access:").unwrap_or(&end).to_owned();
            (ended_handlers)(reason);
        }
    });
    let mut rows = Vec::new();
    rows.push(row_of(label, &initial));
    let mut state = RpcRemoteState {
        rows,
        ..RpcRemoteState::default()
    };
    let resumed = resume_from
        .filter(|cursor| !cursor.is_empty())
        .filter(|cursor| {
            !(initial.status == "running" && initial.current_turn_id.as_deref() == Some(*cursor))
        })
        .map(|cursor| {
            vec![maho_ext_api::RpcRemoteSettledTurn {
                session_id: label.to_owned(),
                turn_id: cursor.to_owned(),
            }]
        });
    if let Some(resumed) = resumed {
        state.settled_turns = Some(resumed);
    }
    state.cursor = Some(initial.current_turn_id.clone().unwrap_or_default());
    Ok(RpcRemoteSubscription {
        initial: state,
        stop: watch.stop.clone(),
    })
}

fn listed_rows(rows: &[RemoteSessionRow], device: &str) -> Vec<Value> {
    rows.iter()
        .filter(|row| row.device == device)
        .map(|row| {
            let status = LISTED_STATUS
                .iter()
                .find(|(state, _)| *state == row.state)
                .map_or(row.state.as_str(), |(_, status)| *status);
            let mut map = Map::new();
            map.insert("sessionId".into(), Value::String(row.session_id.clone()));
            map.insert("status".into(), Value::String(status.to_owned()));
            Value::Object(map)
        })
        .collect()
}

async fn open_device(
    remote: &Arc<RemoteThreadSurface>,
    device: String,
    handlers: RpcRemoteHandlers,
) -> Result<RpcRemoteSubscription, String> {
    let initial_rows = remote.sessions.sessions(None).await.unwrap_or_default();
    let known = Arc::new(Mutex::new(
        listed_rows(&initial_rows, &device)
            .into_iter()
            .map(|row| {
                (
                    row.get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    row.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>(),
    ));
    let lost = Arc::new(Mutex::new(false));
    let (record, resynced, lost_handler) = (
        handlers.record.clone(),
        handlers.resynced.clone(),
        handlers.lost.clone(),
    );
    let (known_round, lost_round, device_round) = (known.clone(), lost.clone(), device.clone());
    let (known_tick, lost_tick, device_tick) = (known.clone(), lost.clone(), device.clone());
    let (record_tick, resynced_tick) = (record.clone(), resynced.clone());
    let tick = Arc::new(Mutex::new(false));
    let in_round = tick.clone();
    let session_port = Arc::clone(&remote.sessions);
    let timer = tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(LISTING_ROUND_MS));
        loop {
            interval.tick().await;
            {
                let mut in_round = in_round.lock().unwrap_or_else(|e| e.into_inner());
                if *in_round {
                    continue;
                }
                *in_round = true;
            }
            let rows = session_port.sessions(None).await;
            let (was_lost, before) = {
                let lost = lost_round.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let before = known_round
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                (lost, before)
            };
            match rows {
                Ok(rows) => {
                    let rows = listed_rows(&rows, &device_round);
                    let now: std::collections::BTreeMap<String, String> = rows
                        .iter()
                        .map(|row| {
                            (
                                row.get("sessionId")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                row.get("status")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            )
                        })
                        .collect();
                    if was_lost {
                        (resynced)(RpcRemoteState {
                            rows,
                            ..RpcRemoteState::default()
                        });
                    } else {
                        for (session_id, status) in &now {
                            if before.get(session_id) != Some(status) {
                                let mut record = Map::new();
                                record
                                    .insert("type".into(), Value::String("session_status".into()));
                                record
                                    .insert("sessionId".into(), Value::String(session_id.clone()));
                                record.insert("status".into(), Value::String(status.clone()));
                                let mut last_change = Map::new();
                                last_change.insert("cause".into(), Value::String("listing".into()));
                                last_change.insert("by".into(), Value::String("device".into()));
                                record.insert("lastChange".into(), Value::Object(last_change));
                                (record)(Value::Object(record));
                            }
                        }
                    }
                    *known_round.lock().unwrap_or_else(|e| e.into_inner()) = now;
                    *lost_round.lock().unwrap_or_else(|e| e.into_inner()) = false;
                }
                Err(error) => {
                    if !was_lost {
                        (lost_handler)(error);
                    }
                    *lost_round.lock().unwrap_or_else(|e| e.into_inner()) = true;
                }
            }
            *in_round.lock().unwrap_or_else(|e| e.into_inner()) = false;
        }
    });
    let initial: Vec<Value> = known
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(session_id, status)| json!({"sessionId": session_id, "status": status}))
        .collect();
    Ok(RpcRemoteSubscription {
        initial: RpcRemoteState {
            rows: initial,
            ..RpcRemoteState::default()
        },
        stop: Arc::new(move || timer.abort()),
    })
}

/// The `tool_call` hook resolver (omo `thread/tools/monitor-target.ts`): what `monitor({type:"rpc",target})` arms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorTargetResolution {
    Keep,
    Target(String),
    Refused(crate::thread::errors::ThreadToolFailure),
}

/// A bare thread id, alias, name or `<device>/<thread>`, resolved through the one address ladder.
pub async fn resolve_monitor_target(
    remote: Option<&Arc<RemoteThreadSurface>>,
    target: &str,
    resolve: impl Fn(
        &str,
    ) -> BoxFuture<
        Result<
            crate::thread::address::resolve::Resolved,
            crate::thread::address::refusals::ThreadRefusal,
        >,
    >,
) -> MonitorTargetResolution {
    let text = target.trim();
    if text.is_empty() || text == "children" || text == "all" || text.starts_with("st_") {
        return MonitorTargetResolution::Keep;
    }
    let on_device = match parse_address(text) {
        maho_thread_address_core::ParsedAddress::Device { device, thread, .. } => {
            Some((device, thread))
        }
        _ => None,
    };
    if on_device.is_none()
        && (text.contains('/')
            || text.contains('\\')
            || text.to_ascii_lowercase().ends_with(".jsonl"))
    {
        return MonitorTargetResolution::Keep;
    }
    if on_device.is_some() && remote.is_none() {
        return MonitorTargetResolution::Keep;
    }
    let mut armed = text.to_owned();
    if let (Some((device, session)), Some(remote)) = (&on_device, remote)
        && session == "*"
    {
        match resolve_typed_device(remote.client.as_ref(), device, false).await {
            Ok(resolved) => armed = format!("{}/*", resolved.device),
            Err(error) => return MonitorTargetResolution::Refused(error),
        }
    } else {
        match resolve(text).await {
            Ok(resolved) => {
                armed = match resolved.where_ {
                    crate::thread::address::types::ResolvedWhere::Device { .. } => resolved.address,
                    crate::thread::address::types::ResolvedWhere::Here { .. } => resolved.thread,
                };
            }
            Err(refusal) => return MonitorTargetResolution::Refused(*refusal),
        }
    }
    if armed == text {
        MonitorTargetResolution::Keep
    } else {
        MonitorTargetResolution::Target(armed)
    }
}
