//! One rpc watch (senpi `rpc-monitor-watch.ts`): an observer per endpoint, joined again every 30 s
//! so hosts that start later are watched too, every delivered record turned into at most one line
//! (rendered, or folded into a coalescing window), and a checkpoint of what it last saw so a
//! restore can report what changed while nothing was watching.
use crate::rpc_monitor_coalesce::{RpcMonitorCoalescer, js_string};
use crate::rpc_monitor_observer::{RpcObserver, RpcObserverHandlers};
use crate::rpc_monitor_render::{RpcMonitorRenderer, session_key};
use crate::rpc_monitor_targets::{
    RpcEvents, RpcTarget, endpoint_sockets, matches_task, subscription_for, wants_record,
};
use crate::{
    RpcRemoteEvents, RpcRemoteHandlers, RpcRemoteOpenOptions, RpcRemoteSettledTurn,
    RpcRemoteSource, RpcRemoteState, RpcRemoteSubscription, RpcRemoteTarget,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

pub const RPC_RESCAN_MS: u64 = 30_000;

/// One endpoint's host identity in a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcHostIdentity {
    pub instance_id: String,
    pub generation: String,
}

/// What a restore compares against: each endpoint's host identity and each session's last status
/// (upstream `RpcMonitorCheckpoint`, persisted as the manifest's `rpcCheckpoint`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcMonitorCheckpoint {
    pub hosts: BTreeMap<String, RpcHostIdentity>,
    pub statuses: BTreeMap<String, String>,
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
/// Where to look for endpoints (upstream `RpcWatchOptions.endpoints`).
pub type EndpointLister = Arc<dyn Fn() -> BoxFuture<Vec<String>> + Send + Sync>;
/// How an endpoint is observed: `openRpcObserver` in production (the upstream module seam its
/// lifecycle test mocks).
pub type ObserverOpener = Arc<
    dyn Fn(String, Value, RpcObserverHandlers) -> BoxFuture<Result<RpcObserver, String>>
        + Send
        + Sync,
>;
pub type LineSink = Arc<dyn Fn(String) + Send + Sync>;
pub type CheckpointSink = Arc<dyn Fn(RpcMonitorCheckpoint) + Send + Sync>;

pub type RemoteFeedRecord = Value;

pub type RemoteFeedBoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A remote target's resume position and settled turns, saved in the monitor checkpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RpcRemoteCheckpoint {
    pub cursor: Option<String>,
    pub settled: BTreeMap<String, Vec<String>>,
}

/// The watch's side of a remote source subscription.
pub struct RpcRemoteFeedHost {
    pub renderer: Arc<Mutex<RpcMonitorRenderer>>,
    pub hosts: Arc<Mutex<indexmap::IndexMap<String, RpcHostIdentity>>>,
    pub record: Arc<dyn Fn(&str, &Value) + Send + Sync>,
    pub line: Arc<dyn Fn(String) + Send + Sync>,
    pub checkpoint: Arc<dyn Fn() + Send + Sync>,
    pub ended: Arc<dyn Fn(&str) + Send + Sync>,
}

/// Options of one remote feed.
pub struct RpcRemoteFeedOptions {
    pub events: RpcEvents,
    pub exclude: Vec<String>,
    pub source: RpcRemoteSource,
    pub saved: Option<RpcRemoteCheckpoint>,
}

/// The remote half of an rpc watch: one `RpcRemoteSource` subscription for a `<device>/<session>` or `<device>/*`
/// target. Records go through the watch's own render path with the device in the socket's place.
///
/// The source reports from its own tasks, so everything a callback moves is interior-mutable: the settled turns and the
/// resume cursor a checkpoint saves, the subscription a stop ends, and the flags that keep a lost or an ended report
/// single.
pub struct RpcRemoteFeed {
    target: RpcRemoteTarget,
    options: RpcRemoteFeedOptions,
    host: RpcRemoteFeedHost,
    settled: Mutex<BTreeMap<String, Vec<String>>>,
    cursor: Mutex<Option<String>>,
    opened: Mutex<Vec<RpcRemoteSettledTurn>>,
    subscription: Mutex<Option<RpcRemoteSubscription>>,
    lost_reported: AtomicBool,
    stopped: AtomicBool,
}

const REMOTE_INSTANCE: &str = "remote";
const SETTLED_KEPT: usize = 16;

/// Callbacks a remote source delivered before its open resolved: they run, in order, once it does.
type PendingSteps = Arc<Mutex<Vec<Box<dyn FnOnce() + Send + Sync>>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn turn_key(device: &str, turn: &RpcRemoteSettledTurn) -> String {
    format!("{device}#{}", turn.session_id)
}

fn settled_line(key: &str, turn_id: &str) -> String {
    let label: String = key
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("rpc {label} turn {turn_id} settled while detached")
}

impl RpcRemoteFeed {
    pub fn new(
        target: RpcRemoteTarget,
        options: RpcRemoteFeedOptions,
        host: RpcRemoteFeedHost,
    ) -> Self {
        let mut settled = BTreeMap::new();
        if let Some(saved) = &options.saved {
            for (key, turns) in &saved.settled {
                settled.insert(key.clone(), turns.clone());
            }
        }
        let cursor = options
            .saved
            .as_ref()
            .and_then(|saved| saved.cursor.clone());
        Self {
            target,
            options,
            host,
            settled: Mutex::new(settled),
            cursor: Mutex::new(cursor),
            opened: Mutex::new(Vec::new()),
            subscription: Mutex::new(None),
            lost_reported: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        }
    }

    /// Opens the subscription; resolves to the arm-time rows. Callbacks before the open resolves wait for it.
    pub fn start(self: Arc<Self>) -> RemoteFeedBoxFuture<Result<Vec<String>, String>> {
        Box::pin(async move {
            let pending: PendingSteps = Arc::new(Mutex::new(Vec::new()));
            let ready = Arc::new(AtomicBool::new(false));
            let source = self.options.source.clone();
            let target = self.target.clone();
            let events = match &self.options.events {
                RpcEvents::All => RpcRemoteEvents::All,
                RpcEvents::Kinds(kinds) => RpcRemoteEvents::Kinds(kinds.clone()),
            };
            let handlers = {
                let (record_self, resync_self, cursor_self, lost_self, ended_self) = (
                    self.clone(),
                    self.clone(),
                    self.clone(),
                    self.clone(),
                    self.clone(),
                );
                let (record_ready, resync_ready, cursor_ready, lost_ready, ended_ready) = (
                    ready.clone(),
                    ready.clone(),
                    ready.clone(),
                    ready.clone(),
                    ready.clone(),
                );
                let (record_pending, resync_pending, cursor_pending, lost_pending, ended_pending) = (
                    pending.clone(),
                    pending.clone(),
                    pending.clone(),
                    pending.clone(),
                    pending.clone(),
                );
                RpcRemoteHandlers {
                    record: Arc::new(move |record| {
                        run_when_ready(&record_ready, &record_pending, move || {
                            record_self.record(record);
                        });
                    }),
                    resynced: Arc::new(move |state| {
                        run_when_ready(&resync_ready, &resync_pending, move || {
                            resync_self.resync(state);
                        });
                    }),
                    cursor: Some(Arc::new(move |cursor| {
                        run_when_ready(&cursor_ready, &cursor_pending, move || {
                            cursor_self.set_cursor(cursor);
                        });
                    })),
                    lost: Arc::new(move |reason| {
                        run_when_ready(&lost_ready, &lost_pending, move || {
                            lost_self.lost(reason);
                        });
                    }),
                    ended: Arc::new(move |reason| {
                        run_when_ready(&ended_ready, &ended_pending, move || {
                            ended_self.ended(reason);
                        });
                    }),
                }
            };
            // Read before the open: a guard held across the await would make this future non-Send.
            let resume_from = lock(&self.cursor).clone();
            let opened = (source.open)(
                target,
                RpcRemoteOpenOptions {
                    events,
                    exclude: self.options.exclude.clone(),
                    resume_from,
                },
                handlers,
            )
            .await?;
            if self.stopped.load(Ordering::SeqCst) {
                opened.stop();
                return Ok(Vec::new());
            }
            let device = self.target.device.clone();
            {
                let mut hosts = lock(&self.host.hosts);
                hosts.insert(
                    device.clone(),
                    RpcHostIdentity {
                        instance_id: REMOTE_INSTANCE.to_owned(),
                        generation: opened.initial.generation.clone().unwrap_or_default(),
                    },
                );
            }
            let settled_turns = opened.initial.settled_turns.clone().unwrap_or_default();
            for turn in &settled_turns {
                self.remember(&turn_key(&device, turn), &turn.turn_id);
            }
            *lock(&self.opened) = settled_turns;
            if let Some(cursor) = &opened.initial.cursor {
                *lock(&self.cursor) = Some(cursor.clone());
            }
            let rows = {
                let mut renderer = lock(&self.host.renderer);
                renderer.seed(&device, &opened.initial.rows)
            };
            (self.host.checkpoint)();
            *lock(&self.subscription) = Some(opened);
            ready.store(true, Ordering::SeqCst);
            let steps: Vec<_> = std::mem::take(&mut *lock(&pending));
            for step in steps {
                step();
            }
            Ok(rows)
        })
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(subscription) = lock(&self.subscription).take() {
            subscription.stop();
        }
    }

    pub fn checkpoint(&self) -> RpcRemoteCheckpoint {
        RpcRemoteCheckpoint {
            cursor: lock(&self.cursor).clone(),
            settled: lock(&self.settled).clone(),
        }
    }

    pub fn diff_settled(&self, saved: Option<&RpcRemoteCheckpoint>) -> Vec<String> {
        let device = self.target.device.clone();
        let opened = lock(&self.opened).clone();
        opened
            .iter()
            .filter(|turn| {
                !saved
                    .and_then(|saved| saved.settled.get(&turn_key(&device, turn)))
                    .is_some_and(|turns| turns.contains(&turn.turn_id))
            })
            .map(|turn| settled_line(&turn_key(&device, turn), &turn.turn_id))
            .collect()
    }

    fn record(&self, record: Value) {
        let device = self.target.device.clone();
        if record.get("type").and_then(Value::as_str) != Some("agent_settled")
            || !record.get("turnId").is_some_and(Value::is_string)
        {
            (self.host.record)(&device, &record);
            return;
        }
        let Some(turn_id) = record.get("turnId").and_then(Value::as_str) else {
            (self.host.record)(&device, &record);
            return;
        };
        let key = session_key(&device, &record);
        if self.seen(&key, turn_id) {
            return;
        }
        self.remember(&key, turn_id);
        (self.host.record)(&device, &record);
        (self.host.checkpoint)();
    }

    fn resync(&self, state: RpcRemoteState) {
        let device = self.target.device.clone();
        let mut lines = Vec::new();
        {
            let mut hosts = lock(&self.host.hosts);
            if let Some(generation) = &state.generation {
                if let Some(host) = hosts.get(&device)
                    && host.generation != *generation
                {
                    lines.push(format!(
                        "rpc host {device} restarted (generation {} -> {generation})",
                        host.generation
                    ));
                }
                hosts.insert(
                    device.clone(),
                    RpcHostIdentity {
                        instance_id: REMOTE_INSTANCE.to_owned(),
                        generation: generation.clone(),
                    },
                );
            }
        }
        let prefix = format!("{device}#");
        let before: BTreeMap<String, String> = {
            let renderer = lock(&self.host.renderer);
            renderer
                .statuses()
                .into_iter()
                .filter(|(key, _)| key.starts_with(&prefix))
                .collect()
        };
        let now: BTreeMap<String, String> = state
            .rows
            .iter()
            .map(|row| {
                (
                    session_key(&device, row),
                    js_string(Some(row.get("status").unwrap_or(&Value::Null))),
                )
            })
            .collect();
        for (key, status) in &now {
            let was = before.get(key).map_or("unknown".to_owned(), Clone::clone);
            if &was != status {
                lines.push(format!(
                    "rpc {} status {was} -> {status} (changed while detached)",
                    settled_label(key)
                ));
            }
        }
        for key in before.keys() {
            if !now.contains_key(key) {
                lines.push(format!(
                    "rpc {} closed (gone while detached)",
                    settled_label(key)
                ));
            }
        }
        for turn in state.settled_turns.unwrap_or_default() {
            let key = turn_key(&device, &turn);
            if self.seen(&key, &turn.turn_id) {
                continue;
            }
            self.remember(&key, &turn.turn_id);
            lines.push(settled_line(&key, &turn.turn_id));
        }
        {
            let mut renderer = lock(&self.host.renderer);
            renderer.forget_socket(&device);
            renderer.seed(&device, &state.rows);
        }
        if let Some(cursor) = state.cursor {
            *lock(&self.cursor) = Some(cursor);
        }
        self.lost_reported.store(false, Ordering::SeqCst);
        for line in lines {
            (self.host.line)(line);
        }
        (self.host.checkpoint)();
    }

    fn lost(&self, reason: String) {
        if self.lost_reported.swap(true, Ordering::SeqCst) {
            return;
        }
        (self.host.line)(format!(
            "rpc device {} unreachable ({reason}); re-watching",
            self.target.device
        ));
    }

    fn ended(&self, reason: String) {
        (self.host.line)(format!(
            "rpc {}/{} access ended ({reason})",
            self.target.device, self.target.session
        ));
        (self.host.ended)(&format!("access ended ({reason})"));
    }

    fn set_cursor(&self, cursor: String) {
        *lock(&self.cursor) = Some(cursor);
        (self.host.checkpoint)();
    }

    fn seen(&self, key: &str, turn_id: &str) -> bool {
        lock(&self.settled)
            .get(key)
            .is_some_and(|turns| turns.iter().any(|turn| turn == turn_id))
    }

    fn remember(&self, key: &str, turn_id: &str) {
        let mut settled = lock(&self.settled);
        let turns = settled.entry(key.to_owned()).or_default();
        if turns.iter().any(|turn| turn == turn_id) {
            return;
        }
        turns.push(turn_id.to_owned());
        if turns.len() > SETTLED_KEPT {
            turns.remove(0);
        }
    }
}

fn settled_label(key: &str) -> String {
    let mut chars: Vec<char> = key.chars().rev().take(8).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn run_when_ready(
    ready: &Arc<AtomicBool>,
    pending: &PendingSteps,
    step: impl FnOnce() + Send + Sync + 'static,
) {
    if ready.load(Ordering::SeqCst) {
        step();
    } else {
        lock(pending).push(Box::new(step));
    }
}

/// Upstream `RpcWatchOptions`.
pub struct RpcWatchOptions {
    pub agent_dir: std::path::PathBuf,
    pub target: RpcTarget,
    pub events: RpcEvents,
    pub exclude: Vec<String>,
    /// Defaults to every endpoint registered under `agent_dir`.
    pub endpoints: Option<EndpointLister>,
    /// Every line this watch reports.
    pub line: LineSink,
    /// The watch's view changed (a host joined, a status moved): persist it.
    pub checkpoint: Option<CheckpointSink>,
    /// Defaults to `open_rpc_observer`.
    pub opener: Option<ObserverOpener>,
    /// The source serving a remote target.
    pub remote_source: Option<RpcRemoteSource>,
    /// A restore's saved view: a remote target resumes from its cursor.
    pub restored_from: Option<RpcMonitorCheckpoint>,
    /// A remote target's access ended for good: the monitor settles with this summary.
    pub ended: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

type Close = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct WatchState {
    observers: HashMap<String, (u64, Close)>,
    /// Upstream `#hosts` Map: insertion order drives `diffSince`.
    hosts: indexmap::IndexMap<String, RpcHostIdentity>,
    renderer: RpcMonitorRenderer,
    task_sessions: HashSet<String>,
    rescan: Option<tokio::task::JoinHandle<()>>,
    /// The remote subscription a `<device>/<session>` or `<device>/*` target opened.
    feed: Option<Arc<RpcRemoteFeed>>,
    stopped: bool,
    next_token: u64,
}

struct WatchInner {
    options: RpcWatchOptions,
    state: Mutex<WatchState>,
    coalescer: RpcMonitorCoalescer,
}

/// Upstream `RpcMonitorWatch`. Cloning shares the watch.
#[derive(Clone)]
pub struct RpcMonitorWatch {
    inner: Arc<WatchInner>,
}

fn default_opener() -> ObserverOpener {
    Arc::new(
        |socket: String,
         subscription: Value,
         handlers: RpcObserverHandlers|
         -> BoxFuture<Result<RpcObserver, String>> {
            Box::pin(async move {
                #[cfg(unix)]
                {
                    crate::rpc_monitor_observer::open_rpc_observer(
                        &socket,
                        &subscription,
                        handlers,
                        None,
                    )
                    .await
                }
                #[cfg(not(unix))]
                {
                    let _ = (socket, subscription, handlers);
                    Err("rpc monitor: unix sockets only".to_owned())
                }
            })
        },
    )
}

impl WatchInner {
    fn state(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn checkpoint(&self) -> RpcMonitorCheckpoint {
        let state = self.state();
        RpcMonitorCheckpoint {
            hosts: state.hosts.clone().into_iter().collect(),
            statuses: state.renderer.statuses(),
        }
    }

    fn emit_checkpoint(&self) {
        if let Some(sink) = &self.options.checkpoint {
            sink(self.checkpoint());
        }
    }

    async fn join(self: Arc<Self>) -> Vec<String> {
        let sockets = match &self.options.endpoints {
            Some(list) => list().await,
            None => endpoint_sockets(&self.options.agent_dir),
        };
        let fresh: Vec<String> = {
            let state = self.state();
            sockets
                .into_iter()
                .filter(|socket| !state.observers.contains_key(socket))
                .collect()
        };
        let tasks: Vec<_> = fresh
            .into_iter()
            .map(|socket| tokio::spawn(self.clone().observe(socket)))
            .collect();
        let mut rows = Vec::new();
        for task in tasks {
            rows.extend(task.await.unwrap_or_default());
        }
        rows
    }

    async fn observe(self: Arc<Self>, socket: String) -> Vec<String> {
        let token = {
            let mut state = self.state();
            if state.stopped {
                return vec![];
            }
            state.next_token += 1;
            state.next_token
        };
        let target = &self.options.target;
        let weak: Weak<Self> = Arc::downgrade(&self);
        let handlers = {
            let (record_watch, record_socket) = (weak.clone(), socket.clone());
            let (closed_watch, closed_socket) = (weak.clone(), socket.clone());
            let batch_watch = weak.clone();
            RpcObserverHandlers {
                record: Box::new(move |record| {
                    if let Some(watch) = record_watch.upgrade() {
                        watch.record(&record_socket, &record);
                    }
                }),
                closed: Box::new(move |_| {
                    if let Some(watch) = closed_watch.upgrade() {
                        watch.lost(&closed_socket, token);
                    }
                }),
                batch_end: Box::new(move || {
                    if let Some(watch) = batch_watch.upgrade() {
                        watch.state().renderer.end_batch();
                    }
                }),
            }
        };
        let opener = self.options.opener.clone().unwrap_or_else(default_opener);
        let subscription = subscription_for(target, &self.options.events);
        let Ok(observer) = opener(socket.clone(), subscription, handlers).await else {
            return vec![];
        };
        let (subscribed, close) = observer.into_parts();
        let close: Close = Arc::from(close);
        // A stop that landed while this observer was connecting has already closed every other one.
        {
            let mut state = self.state();
            if !state.stopped {
                state
                    .observers
                    .insert(socket.clone(), (token, close.clone()));
            }
        }
        if self.state().stopped {
            close();
            return vec![];
        }
        let answer = subscribed.await.ok().and_then(Result::ok);
        if self.state().stopped {
            close();
            return vec![];
        }
        let Some(data) = answer
            .as_ref()
            .and_then(|answer| answer.get("data"))
            .filter(|data| !data.is_null())
        else {
            return vec![];
        };
        let rows = {
            let mut state = self.state();
            state.hosts.insert(
                socket.clone(),
                RpcHostIdentity {
                    instance_id: js_string(data.get("hostInstanceId")),
                    generation: js_string(data.get("hostGeneration")),
                },
            );
            let matched: Vec<Value> = data
                .get("snapshot")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|row| matches_task(target, row))
                .cloned()
                .collect();
            for row in &matched {
                state.task_sessions.insert(session_key(&socket, row));
            }
            // An explicit id names one session: an endpoint whose snapshot did not match it is dropped.
            if !matches!(target, RpcTarget::All | RpcTarget::Children { .. }) && matched.is_empty()
            {
                state.observers.remove(&socket);
                drop(state);
                close();
                return vec![];
            }
            state.renderer.seed(&socket, &matched)
        };
        self.emit_checkpoint();
        rows
    }

    /// An endpoint's connection ended: say so once, and forget what it reported until it is joined again.
    fn lost(&self, socket: &str, token: u64) {
        let line = {
            let mut state = self.state();
            if state.observers.get(socket).map(|(own, _)| *own) != Some(token) {
                return;
            }
            state.observers.remove(socket);
            if state.stopped {
                return;
            }
            let forgotten = state.renderer.forget_socket(socket);
            let prefix = format!("{socket}#");
            state.task_sessions.retain(|key| !key.starts_with(&prefix));
            if state.hosts.shift_remove(socket).is_none() && forgotten == 0 {
                return;
            }
            format!(
                "rpc host {socket} gone ({forgotten} session{} no longer watched)",
                if forgotten == 1 { "" } else { "s" }
            )
        };
        (self.options.line)(line);
        self.emit_checkpoint();
    }

    fn record(self: &Arc<Self>, socket: &str, record: &Value) {
        let kind = js_string(record.get("type"));
        if !wants_record(&self.options.events, &self.options.exclude, &kind) {
            return;
        }
        let key = session_key(socket, record);
        {
            let mut state = self.state();
            if state.stopped {
                return;
            }
            if let RpcTarget::Task { .. } = self.options.target {
                if matches_task(&self.options.target, record) {
                    state.task_sessions.insert(key.clone());
                }
                if !state.task_sessions.contains(&key) {
                    return;
                }
            }
        }
        let label = {
            let (watch, socket, record) = (Arc::downgrade(self), socket.to_owned(), record.clone());
            move || {
                watch.upgrade().map_or_else(String::new, |watch| {
                    let mut state = watch.state();
                    let label = state.renderer.label(&socket, &record);
                    // The flush is its own batch (upstream's microtask clear after the timer).
                    state.renderer.end_batch();
                    label
                })
            }
        };
        if self.coalescer.accept(&key, label, record) {
            return;
        }
        let Some(line) = self.state().renderer.render(socket, record) else {
            return;
        };
        (self.options.line)(line);
        if kind == "session_status" {
            self.emit_checkpoint();
        }
    }
}

impl RpcMonitorWatch {
    pub fn new(options: RpcWatchOptions) -> Self {
        let line = options.line.clone();
        Self {
            inner: Arc::new(WatchInner {
                options,
                state: Mutex::default(),
                coalescer: RpcMonitorCoalescer::new(move |text| line(text)),
            }),
        }
    }

    /// Joins every endpoint now; resolves to the arm-time RESULT rows (one per matched session),
    /// then rejoins every `RPC_RESCAN_MS`.
    pub async fn start(&self) -> Vec<String> {
        if let RpcTarget::Remote { device, session } = &self.inner.options.target {
            let device = device.clone();
            let session = session.clone();
            return self.start_remote(device, session).await;
        }
        let rows = self.inner.clone().join().await;
        let weak = Arc::downgrade(&self.inner);
        let rescan = tokio::spawn(async move {
            let period = std::time::Duration::from_millis(RPC_RESCAN_MS);
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            loop {
                interval.tick().await;
                let Some(watch) = weak.upgrade() else {
                    return;
                };
                watch.join().await;
            }
        });
        let mut state = self.inner.state();
        if state.stopped {
            rescan.abort();
        } else {
            state.rescan = Some(rescan);
        }
        rows
    }

    /// The current checkpoint: host identities and last statuses.
    pub fn checkpoint(&self) -> RpcMonitorCheckpoint {
        self.inner.checkpoint()
    }

    /// Lines for what changed between a saved checkpoint and what this watch sees now.
    pub fn diff_since(&self, saved: &RpcMonitorCheckpoint) -> Vec<String> {
        let state = self.inner.state();
        let mut lines = Vec::new();
        for (socket, host) in &state.hosts {
            // seq restarts with a generation, so only the generation says the host restarted.
            if let Some(before) = saved.hosts.get(socket)
                && before.generation != host.generation
            {
                lines.push(format!(
                    "rpc host {socket} restarted (generation {} -> {})",
                    before.generation, host.generation
                ));
            }
        }
        let now = state.renderer.statuses();
        let children = matches!(self.inner.options.target, RpcTarget::Children { .. });
        for (key, status) in &saved.statuses {
            let (socket, id) = match key.rfind('#') {
                Some(index) => (&key[..index], &key[index + 1..]),
                // `lastIndexOf` -1: the whole key labels, the socket drops its last character.
                None => (
                    key.char_indices()
                        .last()
                        .map_or("", |(index, _)| &key[..index]),
                    key.as_str(),
                ),
            };
            let label: String = id.chars().take(8).collect();
            // Only a host that answered can say a session is gone; otherwise it may be alive but
            // unreachable or no longer matched by the re-resolved target.
            let answered = state.hosts.contains_key(socket);
            match now.get(key) {
                None if answered && !children => {
                    lines.push(format!("rpc {label} closed (gone while detached)"))
                }
                None if answered => lines.push(format!(
                    "rpc {label} no longer listed (closed or no longer a child while detached)"
                )),
                None => lines.push(format!("rpc {label} unreachable (its host did not answer)")),
                Some(current) if current != status => lines.push(format!(
                    "rpc {label} status {status} -> {current} (changed while detached)"
                )),
                Some(_) => {}
            }
        }
        lines
    }

    pub fn start_remote(
        &self,
        device: String,
        session: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let source = inner
                .options
                .remote_source
                .clone()
                .or_else(crate::current_rpc_remote_source);
            let Some(source) = source else {
                return vec![];
            };
            let (renderer, hosts) = {
                let state = inner.state();
                (state.renderer.clone(), state.hosts.clone())
            };
            let renderer = Arc::new(Mutex::new(renderer));
            let hosts = Arc::new(Mutex::new(hosts));
            let record_watch = Arc::downgrade(&inner);
            let line = inner.options.line.clone();
            let checkpoint_sink = inner.options.checkpoint.clone();
            let ended_sink = inner.options.ended.clone();
            let (checkpoint_renderer, checkpoint_hosts) = (renderer.clone(), hosts.clone());
            let host = RpcRemoteFeedHost {
                renderer: renderer.clone(),
                hosts: hosts.clone(),
                record: Arc::new(move |socket, record| {
                    if let Some(watch) = record_watch.upgrade() {
                        watch.record(socket, record);
                    }
                }),
                line,
                checkpoint: Arc::new(move || {
                    if let Some(sink) = &checkpoint_sink {
                        let checkpoint = RpcMonitorCheckpoint {
                            hosts: lock(&checkpoint_hosts).clone().into_iter().collect(),
                            statuses: lock(&checkpoint_renderer).statuses(),
                        };
                        sink(checkpoint);
                    }
                }),
                ended: Arc::new(move |summary| {
                    if let Some(sink) = &ended_sink {
                        sink(summary);
                    }
                }),
            };
            let feed = Arc::new(RpcRemoteFeed::new(
                RpcRemoteTarget { device, session },
                RpcRemoteFeedOptions {
                    events: inner.options.events.clone(),
                    exclude: inner.options.exclude.clone(),
                    source,
                    saved: None,
                },
                host,
            ));
            let rows = Arc::clone(&feed).start().await.unwrap_or_default();
            let mut state = inner.state();
            state.renderer = lock(&renderer).clone();
            state.hosts = lock(&hosts).clone();
            if state.stopped {
                drop(state);
                feed.stop();
            } else {
                state.feed = Some(feed);
            }
            rows
        })
    }

    pub fn stop(&self) {
        let (observers, feed): (Vec<Close>, Option<Arc<RpcRemoteFeed>>) = {
            let mut state = self.inner.state();
            state.stopped = true;
            if let Some(rescan) = state.rescan.take() {
                rescan.abort();
            }
            (
                state
                    .observers
                    .drain()
                    .map(|(_, (_, close))| close)
                    .collect(),
                state.feed.take(),
            )
        };
        self.inner.coalescer.dispose();
        for close in observers {
            close();
        }
        if let Some(feed) = feed {
            feed.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Opened {
        closed: Arc<AtomicBool>,
        handlers: RpcObserverHandlers,
    }
    /// The lifecycle test's mocked `openRpcObserver`: each open waits for `release`, then answers
    /// one busy session.
    #[derive(Clone, Default)]
    struct Mock {
        opened: Arc<Mutex<Vec<Opened>>>,
        release: Arc<tokio::sync::Notify>,
        entered: Arc<tokio::sync::Notify>,
    }
    impl Mock {
        fn opener(&self) -> ObserverOpener {
            let mock = self.clone();
            Arc::new(
                move |_socket: String,
                      _subscription: Value,
                      handlers: RpcObserverHandlers|
                      -> BoxFuture<Result<RpcObserver, String>> {
                    let mock = mock.clone();
                    Box::pin(async move {
                        mock.entered.notify_one();
                        mock.release.notified().await;
                        let closed = Arc::new(AtomicBool::new(false));
                        mock.opened.lock().unwrap().push(Opened {
                            closed: closed.clone(),
                            handlers,
                        });
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        let _ = tx.send(Ok(json!({"type":"response","success":true,"data":{
                        "hostInstanceId":"i-1","hostGeneration":"1",
                        "snapshot":[{"sessionId":"s1","status":"busy","attachments":1}]}})));
                        Ok(RpcObserver::new(rx, move || {
                            closed.store(true, Ordering::SeqCst)
                        }))
                    })
                },
            )
        }
    }

    fn watch(mock: &Mock, lines: &Arc<Mutex<Vec<String>>>, target: RpcTarget) -> RpcMonitorWatch {
        let sink = lines.clone();
        RpcMonitorWatch::new(RpcWatchOptions {
            agent_dir: "/tmp".into(),
            target,
            events: RpcEvents::Kinds(vec!["session_status".into()]),
            exclude: vec![],
            endpoints: Some(Arc::new(|| -> BoxFuture<Vec<String>> {
                Box::pin(async { vec!["/tmp/h.sock".to_owned()] })
            })),
            line: Arc::new(move |line| sink.lock().unwrap().push(line)),
            checkpoint: None,
            opener: Some(mock.opener()),
            remote_source: None,
            restored_from: None,
            ended: None,
        })
    }

    #[tokio::test]
    async fn closes_an_observer_that_finished_connecting_after_the_watch_was_stopped() {
        let (mock, lines) = (Mock::default(), Arc::default());
        let subject = watch(&mock, &lines, RpcTarget::All);
        let started = tokio::spawn({
            let subject = subject.clone();
            async move { subject.start().await }
        });
        mock.entered.notified().await;
        subject.stop();
        mock.release.notify_one();
        assert!(started.await.unwrap().is_empty());
        let opened = mock.opened.lock().unwrap();
        assert_eq!(opened.len(), 1);
        assert!(opened[0].closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn reports_an_endpoint_whose_connection_ended_once_and_forgets_its_sessions() {
        let (mock, lines) = (Mock::default(), Arc::<Mutex<Vec<String>>>::default());
        let subject = watch(&mock, &lines, RpcTarget::All);
        mock.release.notify_one();
        assert_eq!(subject.start().await.len(), 1);
        {
            let opened = mock.opened.lock().unwrap();
            (opened[0].handlers.closed)("host gone".into());
            (opened[0].handlers.closed)("host gone".into());
        }
        let gone: Vec<String> = lines
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with("rpc host /tmp/h.sock gone"))
            .cloned()
            .collect();
        assert_eq!(
            gone,
            vec!["rpc host /tmp/h.sock gone (1 session no longer watched)"]
        );
        assert_eq!(subject.checkpoint(), RpcMonitorCheckpoint::default());
        subject.stop();
    }

    #[tokio::test]
    async fn says_unreachable_not_closed_for_a_saved_session_whose_host_did_not_answer() {
        let subject = watch(&Mock::default(), &Arc::default(), RpcTarget::All);
        let saved: RpcMonitorCheckpoint = serde_json::from_value(json!({
            "hosts":{"/tmp/other.sock":{"instanceId":"i","generation":"1"}},
            "statuses":{"/tmp/other.sock#abcdef123":"busy"}}))
        .unwrap();
        assert_eq!(
            subject.diff_since(&saved),
            vec!["rpc abcdef12 unreachable (its host did not answer)"]
        );
    }

    /// A status record is one line and re-checkpoints; a restore diff names the moved status, the
    /// session gone from an answering host and the host's new generation.
    #[tokio::test]
    async fn a_status_line_checkpoints_and_a_restore_diff_names_what_changed() {
        let mock = Mock::default();
        let lines = Arc::<Mutex<Vec<String>>>::default();
        let checkpoints = Arc::<Mutex<Vec<RpcMonitorCheckpoint>>>::default();
        let sink = lines.clone();
        let saved_sink = checkpoints.clone();
        let subject = RpcMonitorWatch::new(RpcWatchOptions {
            agent_dir: "/tmp".into(),
            target: RpcTarget::All,
            events: RpcEvents::Kinds(vec!["session_status".into()]),
            exclude: vec![],
            endpoints: Some(Arc::new(|| -> BoxFuture<Vec<String>> {
                Box::pin(async { vec!["/tmp/h.sock".to_owned()] })
            })),
            line: Arc::new(move |line| sink.lock().unwrap().push(line)),
            checkpoint: Some(Arc::new(move |checkpoint| {
                saved_sink.lock().unwrap().push(checkpoint)
            })),
            opener: Some(mock.opener()),
            remote_source: None,
            restored_from: None,
            ended: None,
        });
        mock.release.notify_one();
        assert_eq!(subject.start().await, vec!["s1 busy (1 attached)"]);
        {
            let opened = mock.opened.lock().unwrap();
            (opened[0].handlers.record)(json!({"type":"agent_idle","sessionId":"s1"}));
            (opened[0].handlers.record)(
                json!({"type":"session_status","sessionId":"s1","status":"idle","seq":4}),
            );
            (opened[0].handlers.batch_end)();
        }
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["rpc s1 (s1) status busy -> idle seq=4"]
        );
        let latest = checkpoints.lock().unwrap().last().cloned().unwrap();
        assert_eq!(latest.statuses["/tmp/h.sock#s1"], "idle");
        assert_eq!(latest.hosts["/tmp/h.sock"].generation, "1");
        let saved: RpcMonitorCheckpoint = serde_json::from_value(json!({
            "hosts":{"/tmp/h.sock":{"instanceId":"i-0","generation":"0"}},
            "statuses":{"/tmp/h.sock#s1":"busy","/tmp/h.sock#s2":"idle"}}))
        .unwrap();
        assert_eq!(
            subject.diff_since(&saved),
            vec![
                "rpc host /tmp/h.sock restarted (generation 0 -> 1)",
                "rpc s1 status busy -> idle (changed while detached)",
                "rpc s2 closed (gone while detached)",
            ]
        );
        subject.stop();
    }
}
