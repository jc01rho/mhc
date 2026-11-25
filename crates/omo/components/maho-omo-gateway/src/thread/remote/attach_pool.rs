//! Per-session attach pool and bounded catalog hold (`thread/remote/attach-pool.ts`, `remote/port/device-service.ts`;
//! omo #14252, #13873).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::channel::oneshot;
use serde_json::Value;

use crate::thread::adapter::BoxFuture;

use super::ports::{RemoteCommandOutcome, SessionCommandOptions};

pub const HELD_RECORDS: [&str; 2] = ["get_available_models", "get_protocol_info"];
pub const CATALOG_MAX_AGE_MS: i64 = 600_000;
pub const ATTACH_IDLE_MS: u64 = 600_000;

pub type AttachClock = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type AttachTask = Box<dyn FnOnce() + Send>;
pub type CancelAttachTimer = Box<dyn FnOnce() + Send>;
pub type AttachSchedule = Arc<dyn Fn(AttachTask, u64) -> CancelAttachTimer + Send + Sync>;
pub type SnapshotCallback = Arc<dyn Fn() + Send + Sync>;
pub type SnapshotUnsubscribe = Box<dyn FnOnce() + Send>;
type OpenWaiter = oneshot::Sender<Result<Arc<dyn AttachedClient>, RemoteCommandOutcome>>;

pub fn is_held_record(record_type: &str) -> bool {
    HELD_RECORDS.contains(&record_type)
}

/// One live session attachment on the device service (`session.command` + `session.detach`).
pub trait AttachedClient: Send + Sync {
    fn command(
        &self,
        rpc: Value,
        options: SessionCommandOptions,
    ) -> BoxFuture<RemoteCommandOutcome>;
    fn detach(&self) -> BoxFuture<()>;
}

/// Transport connector that opens session attachments and forwards `snapshot` events.
pub trait AttachTransport: Send + Sync {
    fn attach(
        &self,
        device: &str,
        session_id: &str,
    ) -> BoxFuture<Result<Arc<dyn AttachedClient>, RemoteCommandOutcome>>;

    fn subscribe_snapshots(&self, _on_snapshot: SnapshotCallback) -> SnapshotUnsubscribe {
        Box::new(|| {})
    }
}

#[derive(Clone)]
struct HeldEntry {
    response: Value,
    at: i64,
}

#[derive(Default)]
struct CatalogHold {
    held: HashMap<String, HeldEntry>,
    snapshots: u64,
}

impl CatalogHold {
    fn get(&self, key: &str, now: i64) -> Option<Value> {
        let kept = self.held.get(key)?;
        // Bounded inclusively at `CATALOG_MAX_AGE_MS` (#14252): exact expiry still reads local, 1ms past refreshes.
        (now - kept.at <= CATALOG_MAX_AGE_MS).then(|| kept.response.clone())
    }

    fn keep(&mut self, key: String, response: Value, sent_at: u64, now: i64) {
        if sent_at == self.snapshots {
            self.held.insert(key, HeldEntry { response, at: now });
        }
    }

    fn on_snapshot(&mut self) {
        self.snapshots += 1;
        self.held.clear();
    }
}

struct PoolEntry {
    holders: usize,
    idle_cancel: Option<CancelAttachTimer>,
    hold: CatalogHold,
    opened: Option<Arc<dyn AttachedClient>>,
    opening: Option<Vec<OpenWaiter>>,
}

impl PoolEntry {
    fn new() -> Self {
        Self {
            holders: 0,
            idle_cancel: None,
            hold: CatalogHold::default(),
            opened: None,
            opening: None,
        }
    }
}

struct PoolState {
    closed: bool,
    entries: HashMap<String, Arc<Mutex<PoolEntry>>>,
}

/// Keeps at most one attached handle per `(device, session_id)`, caches `HELD_RECORDS` within [`CATALOG_MAX_AGE_MS`],
/// and detaches idle entries once `attach_idle_ms` elapses.
#[derive(Clone)]
pub struct AttachPool {
    transport: Arc<dyn AttachTransport>,
    now: AttachClock,
    schedule: AttachSchedule,
    attach_idle_ms: u64,
    state: Arc<Mutex<PoolState>>,
    unsubscribe: Arc<Mutex<Option<SnapshotUnsubscribe>>>,
}

pub fn inert_attach_schedule() -> AttachSchedule {
    Arc::new(|_task, _delay| Box::new(|| {}))
}

fn entry_key(device: &str, session_id: &str) -> String {
    format!("{device}\0{session_id}")
}

fn lock_unpoison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn should_drop_entry(outcome: &RemoteCommandOutcome) -> bool {
    match outcome {
        RemoteCommandOutcome::Refused { reason, .. } => matches!(
            reason.as_str(),
            "closed"
                | "route_unlisted"
                | "unknown_session"
                | "session_not_found"
                | "session_not_attached"
                | "not_attached"
        ),
        RemoteCommandOutcome::Uncertain { reason } => reason == "closed",
        RemoteCommandOutcome::Ok { .. } => false,
    }
}

impl AttachPool {
    pub fn new(
        transport: Arc<dyn AttachTransport>,
        now: AttachClock,
        schedule: AttachSchedule,
        attach_idle_ms: u64,
    ) -> Self {
        let state = Arc::new(Mutex::new(PoolState {
            closed: false,
            entries: HashMap::new(),
        }));
        let state_for_snapshot = Arc::clone(&state);
        let unsub = transport.subscribe_snapshots(Arc::new(move || {
            let entries: Vec<Arc<Mutex<PoolEntry>>> = {
                let guard = lock_unpoison(&state_for_snapshot);
                guard.entries.values().cloned().collect()
            };
            for entry in entries {
                lock_unpoison(&entry).hold.on_snapshot();
            }
        }));
        Self {
            transport,
            now,
            schedule,
            attach_idle_ms,
            state,
            unsubscribe: Arc::new(Mutex::new(Some(unsub))),
        }
    }

    pub fn on_snapshot(&self) {
        let entries: Vec<Arc<Mutex<PoolEntry>>> = {
            let guard = lock_unpoison(&self.state);
            guard.entries.values().cloned().collect()
        };
        for entry in entries {
            lock_unpoison(&entry).hold.on_snapshot();
        }
    }

    async fn acquire(
        &self,
        device: &str,
        session_id: &str,
    ) -> Result<Arc<Mutex<PoolEntry>>, RemoteCommandOutcome> {
        let key = entry_key(device, session_id);
        let entry = {
            let mut state = lock_unpoison(&self.state);
            if state.closed {
                return Err(RemoteCommandOutcome::Refused {
                    reason: "closed".to_owned(),
                    detail: None,
                });
            }
            let entry = state
                .entries
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(PoolEntry::new())))
                .clone();
            let mut guard = lock_unpoison(&entry);
            guard.holders += 1;
            if let Some(cancel) = guard.idle_cancel.take() {
                cancel();
            }
            drop(guard);
            entry
        };

        let open_outcome = self.ensure_open(&key, &entry).await;
        if let Err(err) = open_outcome {
            self.release_without_idle(&key, &entry);
            return Err(err);
        }
        Ok(entry)
    }

    async fn ensure_open(
        &self,
        key: &str,
        entry: &Arc<Mutex<PoolEntry>>,
    ) -> Result<Arc<dyn AttachedClient>, RemoteCommandOutcome> {
        let rx = {
            let mut guard = lock_unpoison(entry);
            if let Some(client) = &guard.opened {
                return Ok(Arc::clone(client));
            }
            if let Some(waiters) = &mut guard.opening {
                let (tx, rx) = oneshot::channel();
                waiters.push(tx);
                Some(rx)
            } else {
                guard.opening = Some(Vec::new());
                None
            }
        };

        if let Some(rx) = rx {
            return rx.await.unwrap_or_else(|_| {
                Err(RemoteCommandOutcome::Refused {
                    reason: "closed".to_owned(),
                    detail: None,
                })
            });
        }

        let (device, session_id) = key.split_once('\0').unwrap_or(("", key));
        let attached = self.transport.attach(device, session_id).await;
        match attached {
            Ok(client) => {
                let is_live = {
                    let state = lock_unpoison(&self.state);
                    !state.closed
                        && state
                            .entries
                            .get(key)
                            .is_some_and(|e| Arc::ptr_eq(e, entry))
                };
                if is_live {
                    let mut guard = lock_unpoison(entry);
                    guard.opened = Some(Arc::clone(&client));
                    let waiters = guard.opening.take().unwrap_or_default();
                    drop(guard);
                    for waiter in waiters {
                        let _ = waiter.send(Ok(Arc::clone(&client)));
                    }
                    Ok(client)
                } else {
                    client.detach().await;
                    let closed = RemoteCommandOutcome::Refused {
                        reason: "closed".to_owned(),
                        detail: None,
                    };
                    let waiters = lock_unpoison(entry).opening.take().unwrap_or_default();
                    for waiter in waiters {
                        let _ = waiter.send(Err(closed.clone()));
                    }
                    Err(closed)
                }
            }
            Err(err) => {
                let waiters = lock_unpoison(entry).opening.take().unwrap_or_default();
                for waiter in waiters {
                    let _ = waiter.send(Err(err.clone()));
                }
                Err(err)
            }
        }
    }

    fn release_without_idle(&self, key: &str, entry: &Arc<Mutex<PoolEntry>>) {
        let should_remove = {
            let mut guard = lock_unpoison(entry);
            guard.holders = guard.holders.saturating_sub(1);
            guard.holders == 0 && guard.opened.is_none()
        };
        if should_remove {
            let mut state = lock_unpoison(&self.state);
            if state
                .entries
                .get(key)
                .is_some_and(|e| Arc::ptr_eq(e, entry))
            {
                state.entries.remove(key);
            }
        }
    }

    fn release(&self, key: &str, entry: &Arc<Mutex<PoolEntry>>) {
        let mut guard = lock_unpoison(entry);
        guard.holders = guard.holders.saturating_sub(1);
        if guard.holders > 0 {
            return;
        }
        if let Some(cancel) = guard.idle_cancel.take() {
            cancel();
        }
        let state_ref = Arc::clone(&self.state);
        let entry_ref = Arc::clone(entry);
        let key_owned = key.to_owned();
        let cancel = (self.schedule)(
            Box::new(move || {
                let client = {
                    let mut state = lock_unpoison(&state_ref);
                    if state.closed {
                        return;
                    }
                    let current = state.entries.get(&key_owned).cloned();
                    if !current
                        .as_ref()
                        .is_some_and(|candidate| Arc::ptr_eq(candidate, &entry_ref))
                    {
                        return;
                    }
                    let mut guard = lock_unpoison(&entry_ref);
                    guard.idle_cancel = None;
                    if guard.holders > 0 {
                        return;
                    }
                    state.entries.remove(&key_owned);
                    guard.opened.take()
                };
                if let Some(client) = client {
                    futures::executor::block_on(client.detach());
                }
            }),
            self.attach_idle_ms,
        );
        guard.idle_cancel = Some(cancel);
    }

    async fn drop_entry(&self, key: &str, entry: &Arc<Mutex<PoolEntry>>) {
        let (cancel, client) = {
            let mut state = lock_unpoison(&self.state);
            if state
                .entries
                .get(key)
                .is_some_and(|e| Arc::ptr_eq(e, entry))
            {
                state.entries.remove(key);
            }
            let mut guard = lock_unpoison(entry);
            (guard.idle_cancel.take(), guard.opened.take())
        };
        if let Some(cancel) = cancel {
            cancel();
        }
        if let Some(client) = client {
            client.detach().await;
        }
    }

    /// Runs `rpc` on `(device, session_id)`, returning a bounded local catalog hit when eligible or forwarding to
    /// the pooled attachment (or a one-shot direct attach when `options.direct` is set).
    pub async fn session_command(
        &self,
        device: &str,
        session_id: &str,
        rpc: Value,
        options: SessionCommandOptions,
    ) -> RemoteCommandOutcome {
        if options.direct {
            let client = match self.transport.attach(device, session_id).await {
                Ok(client) => client,
                Err(outcome) => return outcome,
            };
            let outcome = client.command(rpc, options).await;
            client.detach().await;
            return outcome;
        }

        let key = entry_key(device, session_id);
        let entry = match self.acquire(device, session_id).await {
            Ok(entry) => entry,
            Err(outcome) => return outcome,
        };

        let rpc_type = rpc
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let holdable = is_held_record(&rpc_type);

        let (sent_at, cached) = {
            let guard = lock_unpoison(&entry);
            let now = (self.now)();
            let cached = if holdable && !options.fresh {
                guard.hold.get(&rpc_type, now)
            } else {
                None
            };
            (guard.hold.snapshots, cached)
        };
        if let Some(response) = cached {
            self.release(&key, &entry);
            return RemoteCommandOutcome::Ok {
                response,
                local: true,
                located: None,
            };
        }

        let client = {
            let guard = lock_unpoison(&entry);
            guard.opened.clone()
        };
        let Some(client) = client else {
            self.release(&key, &entry);
            return RemoteCommandOutcome::Refused {
                reason: "closed".to_owned(),
                detail: None,
            };
        };

        let outcome = client.command(rpc, options).await;
        if let RemoteCommandOutcome::Ok { response, .. } = &outcome
            && holdable
        {
            let now = (self.now)();
            lock_unpoison(&entry)
                .hold
                .keep(rpc_type, response.clone(), sent_at, now);
        }
        if should_drop_entry(&outcome) {
            self.drop_entry(&key, &entry).await;
        } else {
            self.release(&key, &entry);
        }
        outcome
    }

    pub async fn close(&self) {
        if let Some(unsub) = lock_unpoison(&self.unsubscribe).take() {
            unsub();
        }
        let drained: Vec<Arc<Mutex<PoolEntry>>> = {
            let mut state = lock_unpoison(&self.state);
            state.closed = true;
            state.entries.drain().map(|(_, entry)| entry).collect()
        };
        let mut to_detach = Vec::new();
        for entry in drained {
            let mut guard = lock_unpoison(&entry);
            if let Some(cancel) = guard.idle_cancel.take() {
                cancel();
            }
            if let Some(client) = guard.opened.take() {
                to_detach.push(client);
            }
            if let Some(waiters) = guard.opening.take() {
                let closed = RemoteCommandOutcome::Refused {
                    reason: "closed".to_owned(),
                    detail: None,
                };
                for waiter in waiters {
                    let _ = waiter.send(Err(closed.clone()));
                }
            }
        }
        for client in to_detach {
            client.detach().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

    use futures::executor::block_on;
    use serde_json::json;

    use super::*;

    struct ManualTimer {
        id: u64,
        due_ms: i64,
        cancelled: bool,
        run: Option<AttachTask>,
    }

    struct ManualSchedule {
        now: Arc<AtomicI64>,
        next_id: AtomicU64,
        timers: Arc<Mutex<Vec<ManualTimer>>>,
    }

    impl ManualSchedule {
        fn new(initial_ms: i64) -> Arc<Self> {
            Arc::new(Self {
                now: Arc::new(AtomicI64::new(initial_ms)),
                next_id: AtomicU64::new(1),
                timers: Arc::new(Mutex::new(Vec::new())),
            })
        }

        fn clock(&self) -> AttachClock {
            let now = Arc::clone(&self.now);
            Arc::new(move || now.load(Ordering::SeqCst))
        }

        fn schedule(self: &Arc<Self>) -> AttachSchedule {
            let me = Arc::clone(self);
            Arc::new(
                move |task: AttachTask, delay_ms: u64| -> CancelAttachTimer {
                    let id = me.next_id.fetch_add(1, Ordering::SeqCst);
                    let due_ms =
                        me.now.load(Ordering::SeqCst) + i64::try_from(delay_ms).unwrap_or(i64::MAX);
                    lock_unpoison(&me.timers).push(ManualTimer {
                        id,
                        due_ms,
                        cancelled: false,
                        run: Some(task),
                    });
                    let timers = Arc::clone(&me.timers);
                    Box::new(move || {
                        let mut guard = lock_unpoison(&timers);
                        for timer in guard.iter_mut() {
                            if timer.id == id {
                                timer.cancelled = true;
                                timer.run = None;
                            }
                        }
                    })
                },
            )
        }

        fn active_count(&self) -> usize {
            lock_unpoison(&self.timers)
                .iter()
                .filter(|timer| !timer.cancelled && timer.run.is_some())
                .count()
        }

        fn advance_by(&self, delta_ms: i64) {
            let target = self.now.fetch_add(delta_ms, Ordering::SeqCst) + delta_ms;
            loop {
                let next_idx = {
                    let guard = lock_unpoison(&self.timers);
                    guard
                        .iter()
                        .enumerate()
                        .filter(|(_, timer)| {
                            !timer.cancelled && timer.run.is_some() && timer.due_ms <= target
                        })
                        .min_by_key(|(_, timer)| timer.due_ms)
                        .map(|(idx, _)| idx)
                };
                let Some(idx) = next_idx else {
                    break;
                };
                let task = {
                    let mut guard = lock_unpoison(&self.timers);
                    guard[idx].run.take()
                };
                if let Some(task) = task {
                    task();
                }
            }
        }
    }

    struct ScriptedAttached {
        session_id: String,
        replies: Arc<Mutex<VecDeque<RemoteCommandOutcome>>>,
        commands: Arc<Mutex<Vec<Value>>>,
        detaches: Arc<Mutex<Vec<String>>>,
    }

    impl AttachedClient for ScriptedAttached {
        fn command(
            &self,
            rpc: Value,
            _options: SessionCommandOptions,
        ) -> BoxFuture<RemoteCommandOutcome> {
            lock_unpoison(&self.commands).push(rpc);
            let outcome = lock_unpoison(&self.replies).pop_front().unwrap_or_else(|| {
                RemoteCommandOutcome::Ok {
                    response: json!({ "success": true, "data": { "models": [] } }),
                    local: false,
                    located: None,
                }
            });
            Box::pin(async move { outcome })
        }

        fn detach(&self) -> BoxFuture<()> {
            lock_unpoison(&self.detaches).push(self.session_id.clone());
            Box::pin(async {})
        }
    }

    struct FakeTransport {
        replies: Arc<Mutex<VecDeque<RemoteCommandOutcome>>>,
        commands: Arc<Mutex<Vec<Value>>>,
        attaches: Arc<Mutex<Vec<String>>>,
        detaches: Arc<Mutex<Vec<String>>>,
        snapshot_cb: Arc<Mutex<Option<SnapshotCallback>>>,
    }

    impl FakeTransport {
        fn new(replies: Vec<RemoteCommandOutcome>) -> Arc<Self> {
            Arc::new(Self {
                replies: Arc::new(Mutex::new(replies.into())),
                commands: Arc::new(Mutex::new(Vec::new())),
                attaches: Arc::new(Mutex::new(Vec::new())),
                detaches: Arc::new(Mutex::new(Vec::new())),
                snapshot_cb: Arc::new(Mutex::new(None)),
            })
        }

        fn fire_snapshot(&self) {
            if let Some(cb) = lock_unpoison(&self.snapshot_cb).clone() {
                cb();
            }
        }
    }

    impl AttachTransport for FakeTransport {
        fn attach(
            &self,
            _device: &str,
            session_id: &str,
        ) -> BoxFuture<Result<Arc<dyn AttachedClient>, RemoteCommandOutcome>> {
            lock_unpoison(&self.attaches).push(session_id.to_owned());
            let attached: Arc<dyn AttachedClient> = Arc::new(ScriptedAttached {
                session_id: session_id.to_owned(),
                replies: Arc::clone(&self.replies),
                commands: Arc::clone(&self.commands),
                detaches: Arc::clone(&self.detaches),
            });
            Box::pin(async move { Ok(attached) })
        }

        fn subscribe_snapshots(&self, on_snapshot: SnapshotCallback) -> SnapshotUnsubscribe {
            *lock_unpoison(&self.snapshot_cb) = Some(on_snapshot);
            let slot = Arc::clone(&self.snapshot_cb);
            Box::new(move || {
                *lock_unpoison(&slot) = None;
            })
        }
    }

    #[test]
    fn held_catalog_survives_to_catalog_max_age_and_expires_1ms_later() {
        let manual = ManualSchedule::new(1_000_000);
        let transport = FakeTransport::new(vec![
            RemoteCommandOutcome::Ok {
                response: json!({ "success": true, "data": { "v": 1 } }),
                local: false,
                located: None,
            },
            RemoteCommandOutcome::Ok {
                response: json!({ "success": true, "data": { "v": 2 } }),
                local: false,
                located: None,
            },
            RemoteCommandOutcome::Ok {
                response: json!({ "success": true, "data": { "v": 3 } }),
                local: false,
                located: None,
            },
        ]);
        let pool = AttachPool::new(
            transport.clone(),
            manual.clock(),
            manual.schedule(),
            ATTACH_IDLE_MS * 2,
        );

        let first = block_on(pool.session_command(
            "b",
            "s1",
            json!({ "type": "get_available_models" }),
            SessionCommandOptions::default(),
        ));
        assert!(matches!(
            first,
            RemoteCommandOutcome::Ok { local: false, .. }
        ));

        manual.advance_by(CATALOG_MAX_AGE_MS);
        let boundary = block_on(pool.session_command(
            "b",
            "s1",
            json!({ "type": "get_available_models" }),
            SessionCommandOptions::default(),
        ));
        assert!(matches!(
            boundary,
            RemoteCommandOutcome::Ok { local: true, ref response, .. } if response["data"]["v"] == 1
        ));

        manual.advance_by(1);
        let expired = block_on(pool.session_command(
            "b",
            "s1",
            json!({ "type": "get_available_models" }),
            SessionCommandOptions::default(),
        ));
        assert!(matches!(
            expired,
            RemoteCommandOutcome::Ok { local: false, ref response, .. } if response["data"]["v"] == 2
        ));

        let fresh = block_on(pool.session_command(
            "b",
            "s1",
            json!({ "type": "get_available_models" }),
            SessionCommandOptions {
                fresh: true,
                ..SessionCommandOptions::default()
            },
        ));
        assert!(matches!(
            fresh,
            RemoteCommandOutcome::Ok { local: false, ref response, .. } if response["data"]["v"] == 3
        ));
        assert_eq!(lock_unpoison(&transport.commands).len(), 3);
    }

    #[test]
    fn snapshot_invalidates_held_catalog_immediately() {
        let manual = ManualSchedule::new(50_000);
        let transport = FakeTransport::new(vec![
            RemoteCommandOutcome::Ok {
                response: json!({ "success": true, "data": { "v": 1 } }),
                local: false,
                located: None,
            },
            RemoteCommandOutcome::Ok {
                response: json!({ "success": true, "data": { "v": 2 } }),
                local: false,
                located: None,
            },
        ]);
        let pool = AttachPool::new(
            transport.clone(),
            manual.clock(),
            manual.schedule(),
            ATTACH_IDLE_MS,
        );

        let first = block_on(pool.session_command(
            "b",
            "s1",
            json!({ "type": "get_protocol_info" }),
            SessionCommandOptions::default(),
        ));
        assert!(matches!(
            first,
            RemoteCommandOutcome::Ok { local: false, .. }
        ));

        transport.fire_snapshot();

        let after_snapshot = block_on(pool.session_command(
            "b",
            "s1",
            json!({ "type": "get_protocol_info" }),
            SessionCommandOptions::default(),
        ));
        assert!(matches!(
            after_snapshot,
            RemoteCommandOutcome::Ok { local: false, ref response, .. } if response["data"]["v"] == 2
        ));
    }

    #[test]
    fn idle_attach_detaches_after_bound_and_close_cancels_pending_timer() {
        let manual = ManualSchedule::new(0);
        let transport = FakeTransport::new(vec![
            RemoteCommandOutcome::Ok {
                response: json!({ "success": true, "data": {} }),
                local: false,
                located: None,
            },
            RemoteCommandOutcome::Ok {
                response: json!({ "success": true, "data": {} }),
                local: false,
                located: None,
            },
        ]);
        let pool = AttachPool::new(transport.clone(), manual.clock(), manual.schedule(), 1_000);

        let _ = block_on(pool.session_command(
            "b",
            "s1",
            json!({ "type": "get_state" }),
            SessionCommandOptions::default(),
        ));
        assert_eq!(manual.active_count(), 1);
        manual.advance_by(1_000);
        assert_eq!(lock_unpoison(&transport.detaches).as_slice(), ["s1"]);

        let _ = block_on(pool.session_command(
            "b",
            "s1",
            json!({ "type": "get_state" }),
            SessionCommandOptions::default(),
        ));
        assert_eq!(lock_unpoison(&transport.attaches).len(), 2);
        assert_eq!(manual.active_count(), 1);

        block_on(pool.close());
        assert_eq!(manual.active_count(), 0);
        manual.advance_by(1_000);
        assert_eq!(lock_unpoison(&transport.detaches).len(), 2);
    }
}
