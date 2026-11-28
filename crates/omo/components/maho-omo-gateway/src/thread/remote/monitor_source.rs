//! omo `thread/remote/monitor-source.ts`: senpi's `RpcRemoteSource` over the thread tools' remote surface.
//!
//! `monitor({type:"rpc", target:"<device>/<session>"})` follows that session through `RemoteSessionsPort::watch` and
//! reports its records with the addressed session as `sessionId`; `<device>/*` follows the device's listing rows, one
//! round every `LISTING_ROUND_MS`. The monitor itself stays senpi's: it hands this source the target and the callbacks,
//! renders what comes back with the local renderer, and keeps the resume cursor in its own checkpoint. A tool call
//! canonicalizes its target first through [`resolve_monitor_target`] (upstream `registerRemoteMonitorTargets`'s
//! `tool_call` hook), so an `open` here reads a `<device id>/<session>` or `<device id>/*` target.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use maho_ext_api::{
    RpcRemoteEvents, RpcRemoteHandlers, RpcRemoteOpenOptions, RpcRemoteOpenResult,
    RpcRemoteSettledTurn, RpcRemoteSource, RpcRemoteState, RpcRemoteSubscription, RpcRemoteTarget,
};
use maho_thread_address_core::{ParsedAddress, ResolveDeviceOptions, id_shaped, parse_address};
use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::address::account::account_devices;
use crate::thread::address::device::{
    AccountDevices, resolve_account_device, resolve_typed_device,
};
use crate::thread::address::{Resolved, ResolvedWhere, ThreadRefusal};
use crate::thread::errors::ThreadToolFailure;
use crate::thread::remote::ports::{
    RemoteResync, RemoteSessionRow, RemoteSessionState, RemoteStatusChange, RemoteThreadSurface,
    RemoteWatchHandlers, RemoteWatchOptions, RemoteWatchOutcome,
};
use crate::thread::remote::remote_ops::remote_ready;

/// How often a `<device>/*` watch re-reads the device's listing rows.
const LISTING_ROUND_MS: u64 = 30_000;
/// The refusals that mean "the device did not answer this round", not "there is no such device": a watch armed on a
/// stored row keeps working while its device is away.
const TRANSIENT: [&str; 2] = ["device_offline", "host_unavailable"];

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A listing state as a monitor reports it: an ended session is `closed`, every other state keeps its name.
fn listed_status(state: &str) -> &str {
    match state {
        "ended" => "closed",
        other => other,
    }
}

/// One session row of a remote state: what the renderer seeds from and what a resync diffs.
fn row_of(label: &str, state: &RemoteSessionState) -> Value {
    let mut row = Map::new();
    row.insert("sessionId".to_owned(), Value::String(label.to_owned()));
    row.insert("status".to_owned(), Value::String(state.status.clone()));
    if let Some(durable) = &state.durable_session_id {
        row.insert(
            "durableSessionId".to_owned(),
            Value::String(durable.clone()),
        );
    }
    Value::Object(row)
}

/// A `session_status` record: the one shape both a derived status change and a listing round report.
fn status_record(label: &str, status: &str, durable: Option<&str>, cause: &str, by: &str) -> Value {
    let mut record = Map::new();
    record.insert(
        "type".to_owned(),
        Value::String("session_status".to_owned()),
    );
    record.insert("sessionId".to_owned(), Value::String(label.to_owned()));
    record.insert("status".to_owned(), Value::String(status.to_owned()));
    if let Some(durable) = durable {
        record.insert(
            "durableSessionId".to_owned(),
            Value::String(durable.to_owned()),
        );
    }
    let mut last_change = Map::new();
    last_change.insert("cause".to_owned(), Value::String(cause.to_owned()));
    last_change.insert("by".to_owned(), Value::String(by.to_owned()));
    record.insert("lastChange".to_owned(), Value::Object(last_change));
    Value::Object(record)
}

/// Whether this watch reports `kind`: an excluded kind never, an `events` list only what it names.
fn wants(events: &RpcRemoteEvents, exclude: &[String], kind: &str) -> bool {
    if exclude.iter().any(|item| item == kind) {
        return false;
    }
    match events {
        RpcRemoteEvents::All => true,
        RpcRemoteEvents::Kinds(kinds) => kinds.iter().any(|item| item == kind),
    }
}

/// One device's listing rows as `(sessionId, status)` pairs, in listing order.
fn listed_rows(rows: &[RemoteSessionRow], device: &str) -> Vec<(String, String)> {
    rows.iter()
        .filter(|row| row.device == device)
        .map(|row| {
            (
                row.session_id.clone(),
                listed_status(row.state.as_str()).to_owned(),
            )
        })
        .collect()
}

fn state_rows(listed: &[(String, String)]) -> Vec<Value> {
    listed
        .iter()
        .map(|(session_id, status)| json!({"sessionId": session_id, "status": status}))
        .collect()
}

/// The remote monitor source over `remote`: `open` follows one `<device>/<session>` or `<device>/*` target.
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
) -> RpcRemoteOpenResult {
    if let Err(unavailable) = remote_ready(remote).await {
        return Err(format!("{}: {}", unavailable.code, unavailable.message));
    }
    if target.session == "*" {
        let device = device_on(remote, &target.device).await?;
        return open_device(remote, device, options, handlers).await;
    }
    let (device, session) = session_on(remote, &target.device, &target.session).await?;
    open_session(remote, device, session, target.session, options, handlers).await
}

/// The device a typed name/id/prefix names, falling back to the stored roster when the round did not reach it.
async fn device_on(remote: &RemoteThreadSurface, typed: &str) -> Result<String, String> {
    match resolve_typed_device(remote.client.as_ref(), typed, false).await {
        Ok(resolved) => Ok(resolved.device),
        Err(error) if TRANSIENT.contains(&error.code.as_str()) => {
            let roster = account_devices(remote.client.as_ref()).await.ok();
            Ok(stored_device(roster.as_ref(), typed))
        }
        Err(error) => Err(format!("{}: {}", error.code, error.message)),
    }
}

/// The id the roster holds for `device`, or `device` itself when the roster does not answer.
fn stored_device(roster: Option<&AccountDevices>, device: &str) -> String {
    let Some(roster) = roster else {
        return device.to_owned();
    };
    resolve_account_device(
        roster,
        device,
        ResolveDeviceOptions {
            online: false,
            exact_id: false,
        },
    )
    .map_or_else(|_| device.to_owned(), |resolved| resolved.id)
}

/// The listing key of `thread` on `device`: the row listed under it, by listing key or by durable id.
async fn stored_session(
    remote: &RemoteThreadSurface,
    device: &str,
    thread: &str,
) -> Option<String> {
    let rows = remote
        .sessions
        .sessions(Some(device.to_owned()))
        .await
        .ok()?;
    rows.iter()
        .find(|row| {
            row.device == device
                && (row.session_id == thread || row.durable_id.as_deref() == Some(thread))
        })
        .map(|row| row.session_id.clone())
}

/// The `(device, session id)` a `<device>/<session>` target watches. A session this device runs is not a remote watch.
async fn session_on(
    remote: &RemoteThreadSurface,
    typed_device: &str,
    thread: &str,
) -> Result<(String, String), String> {
    let device = device_on(remote, typed_device).await?;
    if let Ok(roster) = account_devices(remote.client.as_ref()).await
        && roster.here.as_deref() == Some(device.as_str())
    {
        return Err(format!(
            "not_found: {thread} runs on this device, which a remote watch does not follow."
        ));
    }
    match stored_session(remote, &device, thread).await {
        Some(session) => Ok((device, session)),
        // A durable id or a listing key names a session this round did not list: follow it as written.
        None if id_shaped(thread) => Ok((device, thread.to_owned())),
        None => Err(format!(
            "not_found: device {device} lists no session {thread}."
        )),
    }
}

async fn open_session(
    remote: &Arc<RemoteThreadSurface>,
    device: String,
    session_id: String,
    label: String,
    options: RpcRemoteOpenOptions,
    handlers: RpcRemoteHandlers,
) -> RpcRemoteOpenResult {
    let RpcRemoteOpenOptions {
        events,
        exclude,
        resume_from,
    } = options;
    let RpcRemoteHandlers {
        record: ext_record,
        resynced: ext_resynced,
        cursor: ext_cursor,
        lost: ext_lost,
        ended: ext_ended,
    } = handlers;
    let durable: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let saw_status = Arc::new(AtomicBool::new(false));

    // Every value two callbacks read is cloned for the earlier one and moved into the later one.
    let record_label = label.clone();
    let record_events = events.clone();
    let record_exclude = exclude.clone();
    let record_saw = saw_status.clone();
    let status_record_sink = ext_record.clone();
    let record = Arc::new(move |value: &Value| -> Result<(), String> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !wants(&record_events, &record_exclude, kind) {
            return Ok(());
        }
        if kind == "session_status" {
            record_saw.store(true, Ordering::SeqCst);
        }
        let mut value = value.clone();
        if let Some(map) = value.as_object_mut() {
            map.insert("sessionId".to_owned(), Value::String(record_label.clone()));
        }
        (ext_record)(value);
        Ok(())
    });

    let status_label = label.clone();
    let status_durable = durable.clone();
    let status = Arc::new(move |change: &RemoteStatusChange| -> Result<(), String> {
        if !change.derived || saw_status.load(Ordering::SeqCst) {
            return Ok(());
        }
        if !wants(&events, &exclude, "session_status") {
            return Ok(());
        }
        let durable = lock(&status_durable).clone();
        let cause = change
            .cause
            .clone()
            .unwrap_or_else(|| "lifecycle".to_owned());
        (status_record_sink)(status_record(
            &status_label,
            &change.to,
            durable.as_deref(),
            &cause,
            "agent",
        ));
        Ok(())
    });

    let resync_label = label.clone();
    let durable_slot = durable;
    let resynced = Arc::new(move |resync: &RemoteResync| -> Result<(), String> {
        if let Some(durable) = &resync.state.durable_session_id {
            *lock(&durable_slot) = Some(durable.clone());
        }
        if let Some(cursor) = &ext_cursor {
            cursor(resync.state.current_turn_id.clone().unwrap_or_default());
        }
        let mut state = RpcRemoteState {
            rows: vec![row_of(&resync_label, &resync.state)],
            ..RpcRemoteState::default()
        };
        if !resync.settled_turns.is_empty() {
            state.settled_turns = Some(
                resync
                    .settled_turns
                    .iter()
                    .map(|turn_id| RpcRemoteSettledTurn {
                        session_id: resync_label.clone(),
                        turn_id: turn_id.clone(),
                    })
                    .collect(),
            );
        }
        (ext_resynced)(state);
        Ok(())
    });

    let lost = Arc::new(move |reason: &str| -> Result<(), String> {
        (ext_lost)(reason.to_owned());
        Ok(())
    });

    let watch_handlers = RemoteWatchHandlers {
        record,
        status: Some(status),
        resynced: Some(resynced),
        lost: Some(lost),
    };
    let outcome = remote
        .sessions
        .watch(
            device,
            session_id,
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
    let stop = watch.stop.clone();
    tokio::spawn(async move {
        let end = watch.ended.await;
        if end != "stopped" {
            (ext_ended)(end.strip_prefix("access:").unwrap_or(&end).to_owned());
        }
    });

    let mut state = RpcRemoteState {
        rows: vec![row_of(&label, &initial)],
        ..RpcRemoteState::default()
    };
    let resumed = resume_from
        .filter(|cursor| !cursor.is_empty())
        .filter(|cursor| {
            !(initial.status == "running"
                && initial.current_turn_id.as_deref() == Some(cursor.as_str()))
        })
        .map(|cursor| RpcRemoteSettledTurn {
            session_id: label.clone(),
            turn_id: cursor,
        });
    if let Some(turn) = resumed {
        state.settled_turns = Some(vec![turn]);
    }
    state.cursor = Some(initial.current_turn_id.clone().unwrap_or_default());
    Ok(RpcRemoteSubscription {
        initial: state,
        stop,
    })
}

async fn open_device(
    remote: &Arc<RemoteThreadSurface>,
    device: String,
    options: RpcRemoteOpenOptions,
    handlers: RpcRemoteHandlers,
) -> RpcRemoteOpenResult {
    let listed = listed_rows(
        &remote.sessions.sessions(None).await.unwrap_or_default(),
        &device,
    );
    let known = Arc::new(Mutex::new(
        listed.iter().cloned().collect::<BTreeMap<String, String>>(),
    ));
    let lost = Arc::new(AtomicBool::new(false));
    let events = options.events;
    let exclude = options.exclude;
    let ext_record = handlers.record;
    let ext_resynced = handlers.resynced;
    let ext_lost = handlers.lost;
    let session_port = Arc::clone(&remote.sessions);
    let timer = tokio::spawn(async move {
        let period = std::time::Duration::from_millis(LISTING_ROUND_MS);
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        loop {
            interval.tick().await;
            let was_lost = lost.load(Ordering::SeqCst);
            let before = lock(&known).clone();
            match session_port.sessions(None).await {
                Ok(rows) => {
                    let now: BTreeMap<String, String> =
                        listed_rows(&rows, &device).into_iter().collect();
                    if was_lost {
                        let listed_now: Vec<(String, String)> = now
                            .iter()
                            .map(|(session_id, status)| (session_id.clone(), status.clone()))
                            .collect();
                        (ext_resynced)(RpcRemoteState {
                            rows: state_rows(&listed_now),
                            ..RpcRemoteState::default()
                        });
                    } else if wants(&events, &exclude, "session_status") {
                        for (session_id, status) in &now {
                            if before.get(session_id) == Some(status) {
                                continue;
                            }
                            (ext_record)(status_record(
                                session_id, status, None, "listing", "device",
                            ));
                        }
                    }
                    *lock(&known) = now;
                    lost.store(false, Ordering::SeqCst);
                }
                Err(error) => {
                    if !was_lost {
                        (ext_lost)(error);
                    }
                    lost.store(true, Ordering::SeqCst);
                }
            }
        }
    });
    Ok(RpcRemoteSubscription {
        initial: RpcRemoteState {
            rows: state_rows(&listed),
            ..RpcRemoteState::default()
        },
        stop: Arc::new(move || timer.abort()),
    })
}

/// The canonical address a resolved target arms: `<device id>/<thread id>`, or the thread id on this device.
fn armed_address(resolved: &Resolved) -> String {
    match &resolved.where_ {
        ResolvedWhere::Device { .. } => resolved.address.clone(),
        ResolvedWhere::Here { .. } => resolved.thread.clone(),
    }
}

/// What `monitor({type:"rpc", target})` arms (omo `thread/tools/monitor-target.ts`): the target as written, the
/// canonical address to arm instead, or why the tool call is refused.
#[derive(Clone, Debug, PartialEq)]
pub enum MonitorTargetResolution {
    Keep,
    Target(String),
    Refused(ThreadToolFailure),
}

/// The `tool_call` hook's answer for a monitor target: `children`, `all`, a `st_` task and an empty target stay as
/// written; `<device>/*` arms the device's id; any other address arms what the resolver reads it as.
pub async fn resolve_monitor_target(
    remote: Option<&Arc<RemoteThreadSurface>>,
    target: &str,
    resolve: impl Fn(&str) -> BoxFuture<Result<Resolved, ThreadRefusal>>,
) -> MonitorTargetResolution {
    let text = target.trim();
    if text.is_empty() || text == "children" || text == "all" || text.starts_with("st_") {
        return MonitorTargetResolution::Keep;
    }
    let ParsedAddress::Device { device, thread, .. } = parse_address(text) else {
        return match resolve(text).await {
            Ok(resolved) => MonitorTargetResolution::Target(armed_address(&resolved)),
            Err(refusal) => MonitorTargetResolution::Refused(*refusal),
        };
    };
    if thread != "*" {
        return match resolve(text).await {
            Ok(resolved) => MonitorTargetResolution::Target(armed_address(&resolved)),
            Err(refusal) => MonitorTargetResolution::Refused(*refusal),
        };
    }
    let Some(remote) = remote else {
        return MonitorTargetResolution::Keep;
    };
    match resolve_typed_device(remote.client.as_ref(), &device, false).await {
        Ok(resolved) => MonitorTargetResolution::Target(format!("{}/*", resolved.device)),
        Err(error) => MonitorTargetResolution::Refused(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use super::*;
    use crate::thread::remote::ports::{
        ClientDeviceRow, RemoteClient, RemoteFailure, RemoteLocateOutcome, RemoteSessionWatch,
        RemoteSessionsPort, ServiceStatus,
    };

    const HERE: &str = "a11ce0de5e1f0123";
    const STUDIO: &str = "b7c41e09d2a64f1e";
    const SESSION: &str = "sess-b-1";
    const DURABLE: &str = "0f9c1d7e-3a44-4c8b-9d21-77aa55bb01cc";

    struct FakeClient;

    impl RemoteClient for FakeClient {
        fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>> {
            Box::pin(async {
                Ok(vec![
                    ClientDeviceRow {
                        id: HERE.to_owned(),
                        name: "here".to_owned(),
                        online: true,
                        is_self: true,
                        last_seen_ms: None,
                    },
                    ClientDeviceRow {
                        id: STUDIO.to_owned(),
                        name: "studio".to_owned(),
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
                    device_id: Some(HERE.to_owned()),
                })
            })
        }
    }

    fn row(device: &str, session_id: &str, state: &str) -> RemoteSessionRow {
        RemoteSessionRow {
            device: device.to_owned(),
            session_id: session_id.to_owned(),
            durable_id: Some(DURABLE.to_owned()),
            state: state.to_owned(),
            ..RemoteSessionRow::default()
        }
    }

    #[derive(Default)]
    struct FakeSessions {
        rows: Vec<RemoteSessionRow>,
        watched: StdMutex<Vec<(String, String)>>,
        handlers: StdMutex<Option<RemoteWatchHandlers>>,
    }

    impl RemoteSessionsPort for FakeSessions {
        fn sessions(
            &self,
            device: Option<String>,
        ) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
            let rows: Vec<RemoteSessionRow> = self
                .rows
                .iter()
                .filter(|row| device.as_deref().is_none_or(|device| row.device == device))
                .cloned()
                .collect();
            Box::pin(async move { Ok(rows) })
        }

        fn locate(&self, _device: String, _engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
            Box::pin(async {
                RemoteLocateOutcome::Refused {
                    reason: "unsupported".to_owned(),
                }
            })
        }

        fn watch(
            &self,
            device: String,
            session_id: String,
            handlers: RemoteWatchHandlers,
            _options: RemoteWatchOptions,
        ) -> BoxFuture<RemoteWatchOutcome> {
            lock(&self.watched).push((device, session_id.clone()));
            *lock(&self.handlers) = Some(handlers);
            Box::pin(async move {
                RemoteWatchOutcome::Ok {
                    watch: RemoteSessionWatch {
                        initial: Some(RemoteSessionState {
                            status: "idle".to_owned(),
                            session_id,
                            durable_session_id: Some(DURABLE.to_owned()),
                            current_turn_id: Some("turn-1".to_owned()),
                        }),
                        ended: Box::pin(async { "stopped".to_owned() }),
                        stop: Arc::new(|| {}),
                    },
                }
            })
        }
    }

    fn surface(sessions: &Arc<FakeSessions>) -> Arc<RemoteThreadSurface> {
        Arc::new(RemoteThreadSurface::new(
            sessions.clone(),
            Arc::new(FakeClient),
            None,
        ))
    }

    fn open_handlers(records: &Arc<StdMutex<Vec<Value>>>) -> RpcRemoteHandlers {
        let collected = records.clone();
        RpcRemoteHandlers {
            record: Arc::new(move |record| lock(&collected).push(record)),
            resynced: Arc::new(|_| {}),
            cursor: None,
            lost: Arc::new(|_| {}),
            ended: Arc::new(|_| {}),
        }
    }

    async fn open(
        source: &RpcRemoteSource,
        device: &str,
        session: &str,
        handlers: RpcRemoteHandlers,
    ) -> RpcRemoteSubscription {
        tokio::time::timeout(
            Duration::from_secs(5),
            (source.open)(
                RpcRemoteTarget {
                    device: device.to_owned(),
                    session: session.to_owned(),
                },
                RpcRemoteOpenOptions {
                    events: RpcRemoteEvents::All,
                    exclude: vec![],
                    resume_from: None,
                },
                handlers,
            ),
        )
        .await
        .expect("an open answers within the timeout")
        .expect("the target opens")
    }

    #[tokio::test]
    async fn a_session_target_opens_on_its_row_and_reports_a_derived_status_as_a_record() {
        let sessions = Arc::new(FakeSessions {
            rows: vec![
                row(HERE, "sess-a-1", "running"),
                row(STUDIO, SESSION, "idle"),
            ],
            ..FakeSessions::default()
        });
        let source = create_remote_monitor_source(surface(&sessions));
        let records = Arc::new(StdMutex::new(Vec::new()));
        let opened = open(&source, STUDIO, SESSION, open_handlers(&records)).await;

        assert_eq!(
            lock(&sessions.watched).clone(),
            vec![(STUDIO.to_owned(), SESSION.to_owned())]
        );
        assert_eq!(
            opened.initial.rows,
            vec![json!({"sessionId": SESSION, "status": "idle", "durableSessionId": DURABLE}),]
        );
        assert_eq!(opened.initial.cursor.as_deref(), Some("turn-1"));

        let Some(handlers) = lock(&sessions.handlers).clone() else {
            panic!("the source started a watch");
        };
        let Some(status) = handlers.status.clone() else {
            panic!("the source listens for status changes");
        };
        status(&RemoteStatusChange {
            from: Some("idle".to_owned()),
            to: "running".to_owned(),
            derived: true,
            cause: None,
        })
        .expect("a derived status is reported");

        let records = lock(&records);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].get("type").and_then(Value::as_str),
            Some("session_status")
        );
        assert_eq!(
            records[0].get("sessionId").and_then(Value::as_str),
            Some(SESSION)
        );
        assert_eq!(
            records[0].get("status").and_then(Value::as_str),
            Some("running")
        );
        assert_eq!(
            records[0]
                .pointer("/lastChange/cause")
                .and_then(Value::as_str),
            Some("lifecycle")
        );
        opened.stop();
    }

    #[tokio::test]
    async fn a_device_target_opens_on_the_devices_listed_rows() {
        let sessions = Arc::new(FakeSessions {
            rows: vec![
                row(HERE, "sess-a-1", "running"),
                row(STUDIO, "sess-b-1", "running"),
                row(STUDIO, "sess-b-2", "ended"),
            ],
            ..FakeSessions::default()
        });
        let source = create_remote_monitor_source(surface(&sessions));
        let records = Arc::new(StdMutex::new(Vec::new()));
        let opened = open(&source, STUDIO, "*", open_handlers(&records)).await;

        assert_eq!(
            opened.initial.rows,
            vec![
                json!({"sessionId": "sess-b-1", "status": "running"}),
                json!({"sessionId": "sess-b-2", "status": "closed"}),
            ]
        );
        assert!(lock(&sessions.watched).is_empty());
        opened.stop();
    }

    #[tokio::test]
    async fn a_monitor_target_keeps_the_shapes_that_are_not_addresses() {
        let never = |_: &str| -> BoxFuture<Result<Resolved, ThreadRefusal>> {
            Box::pin(async { panic!("these targets are decided without resolving") })
        };
        for target in ["children", "all", "st_01j9", "", "   "] {
            assert_eq!(
                resolve_monitor_target(None, target, never).await,
                MonitorTargetResolution::Keep
            );
        }
    }

    #[tokio::test]
    async fn a_device_wildcard_arms_the_devices_id() {
        let sessions = Arc::new(FakeSessions::default());
        let surface = surface(&sessions);
        let never = |_: &str| -> BoxFuture<Result<Resolved, ThreadRefusal>> {
            Box::pin(async { panic!("a wildcard target is decided without resolving") })
        };
        assert_eq!(
            resolve_monitor_target(Some(&surface), "studio/*", never).await,
            MonitorTargetResolution::Target(format!("{STUDIO}/*"))
        );
    }
}
