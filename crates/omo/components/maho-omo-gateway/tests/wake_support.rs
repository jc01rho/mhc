//! Shared fixtures of the wake tests (`thread/wake/testing/` upstream): a real store on a hand-driven clock, session
//! files as the engine names them, an engine host that keeps per-session state and answers the carrier's verbs with
//! the close probe's measured replies, and a schedule the test runs by hand instead of a timer.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use maho_omo_gateway::gateway_database_path;
use maho_omo_gateway::thread::store::{
    GatewayStoreOptions, SqliteGatewayStore, create_gateway_store,
};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::store_ops::rfc3339;
use maho_omo_gateway::thread::types::{
    EnqueueOutcome, EnqueueRequest, EnvelopeOrigin, GatewayDeliveryMode, Provenance, ReceiptRef,
};
use maho_omo_gateway::thread::wake::carrier::{CancelScheduled, Schedule, ScheduledRun};
use maho_omo_gateway::thread::wake::carrier_connection::{
    CarrierConnect, CarrierFrameListener, CarrierHostConnection, ConnectionClosed, Unsubscribe,
};
use serde_json::{Value, json};

pub const START_MS: i64 = 1_763_000_000_000;
pub const GIB: u64 = 1024 * 1024 * 1024;
pub const MIB: u64 = 1024 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct WakeHarness {
    pub dir: tempfile::TempDir,
    clock: Arc<AtomicI64>,
    pub store: Arc<SqliteGatewayStore>,
    files: Mutex<BTreeMap<String, PathBuf>>,
    queued: Mutex<BTreeSet<String>>,
}

impl WakeHarness {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let clock = Arc::new(AtomicI64::new(START_MS));
        let store = open_store(&dir, &clock);
        Self {
            dir,
            clock,
            store,
            files: Mutex::default(),
            queued: Mutex::default(),
        }
    }

    /// Another process's store on the same agent directory and clock.
    pub fn other_store(&self) -> Arc<SqliteGatewayStore> {
        open_store(&self.dir, &self.clock)
    }

    pub fn sessions(&self) -> PathBuf {
        self.dir.path().join("sessions")
    }

    pub fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    pub fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn set_now(&self, ms: i64) {
        self.clock.store(ms, Ordering::SeqCst);
    }

    /// Writes the session file once, header first; later calls answer the same path.
    pub fn file(&self, durable_id: &str) -> PathBuf {
        let mut files = lock(&self.files);
        if let Some(found) = files.get(durable_id) {
            return found.clone();
        }
        let path = write_session_file(&self.sessions(), durable_id, &json!({}), "--tmp-project--");
        files.insert(durable_id.to_owned(), path.clone());
        path
    }

    /// The session file plus one same-device row queued for it: the queued row every open needs (A7.2.3).
    pub fn path(&self, durable_id: &str) -> PathBuf {
        let path = self.file(durable_id);
        let first = lock(&self.queued).insert(durable_id.to_owned());
        if first {
            self.queue_row(durable_id, &format!("to-{durable_id}"));
        }
        path
    }

    pub fn queue_row(&self, target: &str, delivery_id: &str) {
        let sent = self
            .store
            .enqueue(&EnqueueRequest {
                provenance: Provenance {
                    workspace: "w".into(),
                    project: "p".into(),
                    lane: None,
                    session: None,
                },
                now: self.now(),
                delivery_id: delivery_id.into(),
                target_durable_id: target.into(),
                sender_principal: "session:row-sender".into(),
                rate_principal: None,
                sender_node: "row-sender".into(),
                sender_turn: None,
                cause_delivery_id: None,
                claimed_root_id: None,
                origin: EnvelopeOrigin::Session {
                    session: "row-sender".into(),
                },
                actor: "row-sender".into(),
                body: format!("to {target}"),
                mode: GatewayDeliveryMode::FollowUp,
                expected_turn_id: None,
                binding: None,
                receipt: ReceiptRef {
                    idempotency_key: delivery_id.into(),
                    args_hash: delivery_id.into(),
                },
                endpoint_kind: None,
                collapse_key: None,
            })
            .expect("enqueue");
        assert!(
            matches!(sent, EnqueueOutcome::Inserted { .. }),
            "queueing {delivery_id} for {target}: {sent:?}"
        );
    }

    /// A row taken in from another device: queued, then given the ledger id and attested device an intake writes
    /// (`device:<device>/<delivery id>`), as another binary's intake leaves it in the shared store.
    pub fn intake_row(&self, target: &str, delivery_id: &str, device: &str) {
        self.queue_row(target, delivery_id);
        let changed = rusqlite::Connection::open(gateway_database_path(self.dir.path()))
            .expect("open the store as another binary")
            .execute(
                "UPDATE deliveries SET ledger_id = ?1, envelope = json_set(envelope, '$.provenance.device', ?2) WHERE delivery_id = ?3",
                rusqlite::params![format!("device:{device}/{delivery_id}"), device, delivery_id],
            )
            .expect("the row becomes one taken in from another device");
        assert_eq!(changed, 1, "{delivery_id}");
    }

    pub fn rows(&self, target: &str) -> Vec<(String, Option<String>)> {
        let mut rows = self.store.list(Some(target), None).expect("list");
        rows.sort_by_key(|row| row.created_at);
        rows.into_iter()
            .map(|row| (row.state.as_str().to_owned(), row.reason))
            .collect()
    }
}

fn open_store(dir: &tempfile::TempDir, clock: &Arc<AtomicI64>) -> Arc<SqliteGatewayStore> {
    let mut options = GatewayStoreOptions::new(dir.path().to_string_lossy().into_owned());
    let clock = clock.clone();
    options.now = Some(Arc::new(move || clock.load(Ordering::SeqCst)));
    create_gateway_store(options).expect("store")
}

/// Writes a session file the way the engine names it (`<sessions>/<project dir>/<timestamp>_<id>.jsonl`), header
/// first; `header` fields override the defaults.
pub fn write_session_file(
    sessions: &Path,
    durable_id: &str,
    header: &Value,
    project_directory: &str,
) -> PathBuf {
    let directory = sessions.join(project_directory);
    std::fs::create_dir_all(&directory).expect("session directory");
    let path = directory.join(format!("2026-11-12T00-00-00-000Z_{durable_id}.jsonl"));
    let mut line = json!({
        "type": "session",
        "version": 3,
        "id": durable_id,
        "timestamp": "2026-11-12T00:00:00.000Z",
        "cwd": "/work/project",
    });
    if let (Some(line), Some(fields)) = (line.as_object_mut(), header.as_object()) {
        for (key, value) in fields {
            line.insert(key.clone(), value.clone());
        }
    }
    std::fs::write(&path, format!("{line}\n")).expect("session file");
    path
}

pub fn append_line(path: &Path, line: &Value) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("session file");
    writeln!(file, "{line}").expect("append");
}

/// The two transcript entries the engine's effect ledger writes for an `unload_session` carrying an `effectId`:
/// `accepted` before it is applied, then `done` (or `failed` for a refused unload).
pub fn record_unload_effect(path: &Path, durable_id: &str, at: i64, state: &str) {
    let effect = |entry_state: &str, iso: String| {
        let mut data = json!({
            "effectId": format!("omo-thread-close:{at}"),
            "kind": "unload",
            "state": entry_state,
            "by": { "connectionId": "c-1" },
            "acceptedAt": rfc3339(at - 5),
            "updatedAt": iso,
            "durableSessionId": durable_id,
            "fingerprint": "f",
        });
        if entry_state == "done" {
            data["disposition"] = json!("unloaded");
        }
        json!({
            "type": "custom",
            "customType": "rpc_effect",
            "id": format!("e-{entry_state}-{at}"),
            "parentId": null,
            "timestamp": iso,
            "data": data,
        })
    };
    append_line(path, &effect("accepted", rfc3339(at - 5)));
    append_line(path, &effect(state, rfc3339(at)));
}

struct Pending {
    delay_ms: u64,
    run: ScheduledRun,
    cancelled: Arc<AtomicBool>,
}

/// A schedule a test runs by hand: nothing runs until the test says so.
#[derive(Default)]
pub struct ManualSchedule {
    tasks: Mutex<Vec<Pending>>,
}

impl ManualSchedule {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn schedule(self: &Arc<Self>) -> Schedule {
        let me = Arc::clone(self);
        Arc::new(move |run: ScheduledRun, delay_ms: u64| -> CancelScheduled {
            let cancelled = Arc::new(AtomicBool::new(false));
            lock(&me.tasks).push(Pending {
                delay_ms,
                run,
                cancelled: cancelled.clone(),
            });
            Box::new(move || cancelled.store(true, Ordering::SeqCst))
        })
    }

    pub fn delays(&self) -> Vec<u64> {
        lock(&self.tasks)
            .iter()
            .filter(|task| !task.cancelled.load(Ordering::SeqCst))
            .map(|task| task.delay_ms)
            .collect()
    }

    /// Runs every run queued at this moment, in order, and none of the runs they queue.
    pub fn run_queued(&self) -> usize {
        let pending: Vec<Pending> = std::mem::take(&mut *lock(&self.tasks));
        let mut ran = 0;
        for task in pending {
            if !task.cancelled.load(Ordering::SeqCst) {
                (task.run)();
                ran += 1;
            }
        }
        ran
    }

    /// Runs the runs queued with no delay (an event's evaluation, a carrier's start sweep) until none is left.
    pub fn run_immediate(&self) -> usize {
        let mut ran = 0;
        loop {
            let ready: Vec<Pending> = {
                let mut tasks = lock(&self.tasks);
                let (ready, later): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut *tasks)
                    .into_iter()
                    .partition(|task| task.delay_ms == 0);
                *tasks = later;
                ready
            };
            if ready.is_empty() {
                return ran;
            }
            for task in ready {
                if !task.cancelled.load(Ordering::SeqCst) {
                    (task.run)();
                    ran += 1;
                }
            }
        }
    }
}

pub fn inert_schedule() -> Schedule {
    Arc::new(|_run: ScheduledRun, _delay_ms: u64| -> CancelScheduled { Box::new(|| {}) })
}

pub fn finished() -> Value {
    json!({
        "lifecycle": { "state": "done", "waitingOn": { "children": 0, "background": [], "human": [] }, "humanInputRevision": 0 },
        "holds": [], "paused": false, "isStreaming": false, "pendingDialogs": []
    })
}

pub fn streaming() -> Value {
    json!({
        "lifecycle": { "state": "running", "waitingOn": { "children": 0, "background": [{ "source": "delivery" }], "human": [] } },
        "holds": [], "paused": false, "isStreaming": true, "pendingDialogs": []
    })
}

pub fn background() -> Value {
    json!({
        "lifecycle": { "state": "waiting_background", "waitingOn": { "children": 0, "background": [{ "source": "terminal-background-sessions", "activeCount": 1 }], "human": [] } },
        "holds": [], "paused": false, "isStreaming": false, "pendingDialogs": []
    })
}

pub fn parked_approval() -> Value {
    json!({
        "lifecycle": { "state": "waiting_human", "waitingOn": { "children": 0, "background": [{ "source": "delivery" }], "human": [{ "kind": "question" }] } },
        "holds": [], "paused": false, "isStreaming": true,
        "pendingDialogs": [{ "method": "select", "title": "Permission required: eval" }]
    })
}

pub fn stalled() -> Value {
    json!({
        "lifecycle": { "state": "waiting_human", "waitingOn": { "children": 0, "background": [], "human": [{ "kind": "stalled" }] } },
        "holds": [], "paused": false, "isStreaming": false, "pendingDialogs": []
    })
}

pub fn stopped() -> Value {
    json!({
        "lifecycle": { "state": "paused", "waitingOn": { "children": 0, "background": [], "human": [] } },
        "holds": [{ "kind": "session_hold", "key": "session_hold:1" }], "paused": true, "isStreaming": false, "pendingDialogs": []
    })
}

pub fn host_stamped() -> Value {
    json!({
        "lifecycle": { "state": "held", "waitingOn": { "children": 0, "background": [], "human": [] } },
        "holds": [{ "kind": "unattended" }], "paused": true, "isStreaming": false, "pendingDialogs": []
    })
}

pub struct FakeSession {
    pub snapshot: Value,
    /// Attachments held by connections other than the carrier's.
    pub others: i64,
    /// Deliveries admitted into the session and held as notices; named in `dropped` by an interrupt release.
    pub held_deliveries: Vec<String>,
    pub routing_id: String,
    pub released: bool,
}

pub type OpenRefusal = Arc<dyn Fn(&str) -> Option<Value> + Send + Sync>;
pub type WakePass = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

#[derive(Default, Clone)]
pub struct FakeEngineOptions {
    /// Answers `release_session` as an unknown command (a host without the verb).
    pub release_unknown: bool,
    pub open_refusal: Option<OpenRefusal>,
    /// Runs the woken session's drain pass for `wake` and answers with what it admitted.
    pub wake_pass: Option<WakePass>,
}

type Listeners = Vec<(u64, CarrierFrameListener)>;

#[derive(Default)]
struct HostState {
    frames: Vec<Value>,
    sessions: BTreeMap<String, FakeSession>,
    by_routing: BTreeMap<String, String>,
    next_routing: u64,
    holders: BTreeMap<u64, BTreeSet<String>>,
    listeners: BTreeMap<u64, Listeners>,
    closed: BTreeMap<u64, Vec<ConnectionClosed>>,
    next_id: u64,
}

/// An engine host in process that keeps per-session state across connections.
#[derive(Clone)]
pub struct FakeEngineHost {
    state: Arc<Mutex<HostState>>,
    options: FakeEngineOptions,
}

fn key_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn session_entry<'a>(state: &'a mut HostState, path: &str) -> &'a mut FakeSession {
    match state.sessions.entry(path.to_owned()) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            state.next_routing += 1;
            let routing_id = format!("rpc-{}", state.next_routing);
            state.by_routing.insert(routing_id.clone(), path.to_owned());
            entry.insert(FakeSession {
                snapshot: finished(),
                others: 0,
                held_deliveries: Vec::new(),
                routing_id,
                released: false,
            })
        }
    }
}

fn attachments_of(state: &HostState, path: &str) -> i64 {
    let held = state
        .holders
        .values()
        .filter(|held| held.contains(path))
        .count() as i64;
    held + state.sessions.get(path).map_or(0, |session| session.others)
}

fn refuse(code: &str, error_data: Option<Value>) -> Value {
    let mut reply = json!({ "success": false, "error": code, "errorCode": code });
    if let Some(data) = error_data {
        reply["errorData"] = data;
    }
    reply
}

fn path_of(state: &HostState, frame: &Value) -> Option<String> {
    frame
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(|id| state.by_routing.get(id).cloned())
}

impl FakeEngineHost {
    pub fn new(options: FakeEngineOptions) -> Self {
        Self {
            state: Arc::default(),
            options,
        }
    }

    fn lock(&self) -> MutexGuard<'_, HostState> {
        lock(&self.state)
    }

    pub fn connect(&self) -> CarrierConnect {
        let host = self.clone();
        Arc::new(move || -> Result<Arc<dyn CarrierHostConnection>, String> {
            let id = {
                let mut state = host.lock();
                state.next_id += 1;
                state.next_id
            };
            Ok(Arc::new(FakeConnection {
                id,
                host: host.clone(),
                closed: AtomicBool::new(false),
            }))
        })
    }

    pub fn frames(&self, kind: &str) -> Vec<Value> {
        self.lock()
            .frames
            .iter()
            .filter(|frame| frame.get("type").and_then(Value::as_str) == Some(kind))
            .cloned()
            .collect()
    }

    pub fn set_snapshot(&self, path: &Path, snapshot: Value) {
        session_entry(&mut self.lock(), &key_of(path)).snapshot = snapshot;
    }

    pub fn set_others(&self, path: &Path, others: i64) {
        session_entry(&mut self.lock(), &key_of(path)).others = others;
    }

    pub fn set_held_deliveries(&self, path: &Path, held: &[&str]) {
        session_entry(&mut self.lock(), &key_of(path)).held_deliveries =
            held.iter().map(|id| (*id).to_owned()).collect();
    }

    /// The engine's `session_lifecycle` line, written to every connection attached to the session.
    pub fn emit_lifecycle(&self, path: &Path) {
        let key = key_of(path);
        let (line, listeners) = {
            let mut state = self.lock();
            let session = session_entry(&mut state, &key);
            let line = json!({
                "type": "session_lifecycle",
                "sessionId": session.routing_id,
                "lifecycle": session.snapshot.get("lifecycle").cloned().unwrap_or(Value::Null),
            });
            let holders: Vec<u64> = state
                .holders
                .iter()
                .filter(|(_, held)| held.contains(&key))
                .map(|(id, _)| *id)
                .collect();
            let listeners: Vec<CarrierFrameListener> = holders
                .iter()
                .filter_map(|id| state.listeners.get(id))
                .flatten()
                .map(|(_, listener)| listener.clone())
                .collect();
            (line, listeners)
        };
        for listener in listeners {
            listener(&line);
        }
    }

    fn answer(
        &self,
        state: &mut HostState,
        connection: u64,
        frame: &Value,
    ) -> (Value, Option<String>) {
        let command = frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match command {
            "open_session" => {
                let path = frame
                    .get("sessionPath")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if let Some(refusal) = self
                    .options
                    .open_refusal
                    .as_ref()
                    .and_then(|refuse| refuse(&path))
                {
                    return (refusal, None);
                }
                let attached = attachments_of(state, &path) > 0;
                let session = session_entry(state, &path);
                session.released = false;
                let reply = json!({
                    "success": true,
                    "data": { "sessionId": session.routing_id, "attached": attached, "state": session.snapshot },
                });
                state.holders.entry(connection).or_default().insert(path);
                (reply, None)
            }
            "get_state" => match path_of(state, frame) {
                None => (refuse("unknown_session", None), None),
                Some(path) => (
                    json!({ "success": true, "data": session_entry(state, &path).snapshot }),
                    None,
                ),
            },
            "list_sessions" => {
                let sessions: Vec<Value> = state
                    .sessions
                    .iter()
                    .filter(|(_, session)| !session.released)
                    .map(|(path, session)| {
                        json!({
                            "sessionId": session.routing_id,
                            "sessionPath": path,
                            "status": "open",
                            "attachments": attachments_of(state, path),
                        })
                    })
                    .collect();
                (
                    json!({ "success": true, "data": { "sessions": sessions } }),
                    None,
                )
            }
            "wake" => match path_of(state, frame) {
                None => (refuse("unknown_session", None), None),
                Some(path) => (
                    json!({ "success": true, "data": { "admitted": [] } }),
                    Some(path),
                ),
            },
            "release_session" => {
                let Some(path) = path_of(state, frame) else {
                    return (refuse("unknown_session", None), None);
                };
                if self.options.release_unknown {
                    return (refuse("unknown_command", None), None);
                }
                let interrupt = frame.get("interrupt") == Some(&Value::Bool(true));
                let session = session_entry(state, &path);
                if session.others > 0 {
                    return (
                        refuse("attached", Some(json!({ "attachments": session.others }))),
                        None,
                    );
                }
                let snapshot = &session.snapshot;
                let asking = snapshot
                    .get("pendingDialogs")
                    .and_then(Value::as_array)
                    .is_some_and(|dialogs| !dialogs.is_empty())
                    || snapshot.pointer("/lifecycle/state").and_then(Value::as_str)
                        == Some("running");
                if asking {
                    return (
                        refuse(
                            "turn_active",
                            Some(json!({ "attachments": 1, "busy": ["turn", "session_work"] })),
                        ),
                        None,
                    );
                }
                let holding = !session.held_deliveries.is_empty();
                if holding && !interrupt {
                    return (
                        refuse(
                            "turn_active",
                            Some(json!({ "attachments": 1, "busy": ["delivery"] })),
                        ),
                        None,
                    );
                }
                let dropped = if interrupt {
                    std::mem::take(&mut session.held_deliveries)
                } else {
                    Vec::new()
                };
                session.held_deliveries.clear();
                session.released = true;
                if let Some(held) = state.holders.get_mut(&connection) {
                    held.remove(&path);
                }
                (
                    json!({
                        "success": true,
                        "data": { "released": true, "attachments": 1, "dropped": { "deliveries": dropped, "user_messages": [] } },
                    }),
                    None,
                )
            }
            "close_session" => {
                let Some(path) = path_of(state, frame) else {
                    return (refuse("unknown_session", None), None);
                };
                if let Some(held) = state.holders.get_mut(&connection) {
                    held.remove(&path);
                }
                let session = session_entry(state, &path);
                if session.others == 0 {
                    session.released = true;
                    session.snapshot = stalled();
                }
                (json!({ "success": true, "data": {} }), None)
            }
            "release_attachment" => {
                if let Some(path) = path_of(state, frame)
                    && let Some(held) = state.holders.get_mut(&connection)
                {
                    held.remove(&path);
                }
                (json!({ "success": true, "data": {} }), None)
            }
            "detach_connection" => {
                if let Some(held) = state.holders.get_mut(&connection) {
                    held.clear();
                }
                (json!({ "success": true, "data": { "deferred": [] } }), None)
            }
            _ => (refuse("unknown_command", None), None),
        }
    }
}

struct FakeConnection {
    id: u64,
    host: FakeEngineHost,
    closed: AtomicBool,
}

impl CarrierHostConnection for FakeConnection {
    fn request(&self, frame: Value) -> Result<Value, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("the engine host closed the carrier's connection".to_owned());
        }
        let command = frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let (reply, woken) = {
            let mut state = self.host.lock();
            state.frames.push(frame.clone());
            self.host.answer(&mut state, self.id, &frame)
        };
        let reply = match (woken, &self.host.options.wake_pass) {
            (Some(path), Some(pass)) => {
                json!({ "success": true, "data": { "admitted": pass(&path) } })
            }
            _ => reply,
        };
        let mut framed = json!({ "type": "response", "command": command });
        if let (Some(target), Some(source)) = (framed.as_object_mut(), reply.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        Ok(framed)
    }

    fn on_event(&self, listener: CarrierFrameListener) -> Unsubscribe {
        let listener_id = {
            let mut state = self.host.lock();
            state.next_id += 1;
            let listener_id = state.next_id;
            state
                .listeners
                .entry(self.id)
                .or_default()
                .push((listener_id, listener));
            listener_id
        };
        let host = self.host.clone();
        let connection = self.id;
        Box::new(move || {
            if let Some(listeners) = host.lock().listeners.get_mut(&connection) {
                listeners.retain(|(id, _)| *id != listener_id);
            }
        })
    }

    fn on_closed(&self, callback: ConnectionClosed) {
        if self.closed.load(Ordering::SeqCst) {
            callback();
            return;
        }
        self.host
            .lock()
            .closed
            .entry(self.id)
            .or_default()
            .push(callback);
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let callbacks = {
            let mut state = self.host.lock();
            state.holders.remove(&self.id);
            state.listeners.remove(&self.id);
            state.closed.remove(&self.id).unwrap_or_default()
        };
        for callback in callbacks {
            callback();
        }
    }
}
