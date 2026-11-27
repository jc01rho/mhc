// allow: SIZE_OK — indivisible remote session watch state machine (attach, resync, relocate, reattach, queue drain)
//! Follows a remote session across attach ends, service restarts and engine restarts (omo
//! `thread/remote/remote-watch.ts`; #12719, #14748 review round 7).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::channel::oneshot;
use serde_json::Value;
use tokio::sync::Notify;

use crate::thread::adapter::BoxFuture;
use crate::thread::remote::ports::{
    LookupFenceRefused, RemoteCommandOutcome, RemoteResync, RemoteSessionState, RemoteSessionWatch,
    RemoteStatusChange, RemoteWatchHandlers, RemoteWatchOptions, RemoteWatchOutcome,
};
use crate::thread::remote::remote_ops::reply_data;
use crate::thread::remote::session_state::{
    ACCESS_ENDED, ACCESS_REFUSED, SESSION_GONE, SESSION_LEFT, cause_of, non_empty,
    session_state_of, status_of,
};
use crate::thread::remote::watch_queue::{WatchItem, WatchQueueState};

const RETRY_FIRST_MS: u64 = 1_000;
const RETRY_MAX_MS: u64 = 30_000;
const SETTLED_KEPT: usize = 256;

#[derive(Debug, Clone)]
pub enum WatchAttachEvent {
    Snapshot {
        attach_id: u64,
        durable_session_id: Option<String>,
    },
    Records {
        attach_id: u64,
        records: Vec<Value>,
        seqs: Option<Vec<u64>>,
    },
    Ended {
        reason: String,
    },
}

pub type WatchEventListener = Arc<dyn Fn(WatchAttachEvent) + Send + Sync>;
pub type WatchUnsubscribe = Box<dyn FnOnce() + Send>;
pub type WatchSubscribeFn = Box<dyn FnOnce(WatchEventListener) -> WatchUnsubscribe + Send>;

pub struct WatchHeldAttach {
    pub history: Vec<WatchAttachEvent>,
    pub subscribe: WatchSubscribeFn,
    pub release: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchLocateError {
    FenceRefused(LookupFenceRefused),
    Failed(String),
}

impl From<LookupFenceRefused> for WatchLocateError {
    fn from(value: LookupFenceRefused) -> Self {
        Self::FenceRefused(value)
    }
}

pub type WatchHoldFn =
    Arc<dyn Fn(String, String) -> BoxFuture<Result<WatchHeldAttach, String>> + Send + Sync>;
pub type WatchGetStateFn =
    Arc<dyn Fn(String, String) -> BoxFuture<RemoteCommandOutcome> + Send + Sync>;
pub type WatchLocateFn =
    Arc<dyn Fn(String, String) -> BoxFuture<Result<String, WatchLocateError>> + Send + Sync>;
pub type WatchDelayFn = Arc<dyn Fn(u64) -> BoxFuture<()> + Send + Sync>;

#[derive(Clone)]
pub struct WatchLink {
    pub hold: WatchHoldFn,
    pub get_state: WatchGetStateFn,
    pub locate: WatchLocateFn,
    pub delay: WatchDelayFn,
}

struct WatchCore {
    key: String,
    attach_epoch: u64,
    unsubscribe: Option<WatchUnsubscribe>,
    release_held: Option<Arc<dyn Fn() + Send + Sync>>,
    attach_id: Option<u64>,
    last_seq: u64,
    status: Option<String>,
    durable: Option<String>,
    running_turn: Option<String>,
    saw_status_record: bool,
    settled: VecDeque<String>,
    lost_reported: bool,
    left_reason: Option<String>,
    done: bool,
    end_tx: Option<oneshot::Sender<String>>,
}

struct SessionWatch {
    link: WatchLink,
    device: String,
    handlers: RemoteWatchHandlers,
    core: Mutex<WatchCore>,
    queue: Mutex<WatchQueueState>,
    wake: Notify,
}

fn lock_unpoison<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SessionWatch {
    fn core(&self) -> MutexGuard<'_, WatchCore> {
        lock_unpoison(&self.core)
    }

    fn enqueue(&self, item: WatchItem) {
        lock_unpoison(&self.queue).push(item);
        self.wake.notify_one();
    }

    fn finish(&self, end: &str) {
        let (unsub, rel, tx) = {
            let mut c = self.core();
            if c.done {
                return;
            }
            c.done = true;
            c.attach_epoch += 1;
            lock_unpoison(&self.queue).close();
            (c.unsubscribe.take(), c.release_held.take(), c.end_tx.take())
        };
        if let Some(unsub) = unsub {
            unsub();
        }
        if let Some(rel) = rel {
            rel();
        }
        if let Some(tx) = tx {
            let _ = tx.send(end.to_owned());
        }
        self.wake.notify_waiters();
    }

    fn release(&self) {
        let (unsub, rel) = {
            let mut c = self.core();
            c.attach_epoch += 1;
            (c.unsubscribe.take(), c.release_held.take())
        };
        if let Some(unsub) = unsub {
            unsub();
        }
        if let Some(rel) = rel {
            rel();
        }
    }

    fn deliver(&self, call: impl FnOnce() -> Result<(), String>) -> bool {
        if call().is_err() {
            self.finish("listener_failed");
            false
        } else {
            true
        }
    }

    fn report_lost(&self, reason: &str) {
        {
            let mut c = self.core();
            if c.lost_reported {
                return;
            }
            c.lost_reported = true;
        }
        if let Some(ref lost) = self.handlers.lost {
            let _ = self.deliver(|| lost(reason));
        }
    }

    fn mark_settled(c: &mut WatchCore, turn: String) {
        c.settled.push_back(turn);
        if c.settled.len() > SETTLED_KEPT {
            c.settled.pop_front();
        }
    }

    fn adopt(c: &mut WatchCore, state: &RemoteSessionState) {
        c.status = Some(state.status.clone());
        if let Some(ref dur) = state.durable_session_id {
            c.durable = Some(dur.clone());
        }
        c.running_turn = (state.status == "running")
            .then(|| state.current_turn_id.clone())
            .flatten();
    }

    async fn read_state(&self) -> Result<RemoteSessionState, String> {
        let key = self.core().key.clone();
        match (self.link.get_state)(self.device.clone(), key.clone()).await {
            RemoteCommandOutcome::Ok { response, .. } => {
                Ok(session_state_of(&key, &reply_data(&response)))
            }
            RemoteCommandOutcome::Refused { reason, .. }
            | RemoteCommandOutcome::Uncertain { reason } => Err(reason),
        }
    }

    async fn attach(self: &Arc<Self>) -> Result<(), String> {
        let key = self.core().key.clone();
        let held = (self.link.hold)(self.device.clone(), key).await?;
        let epoch = {
            let mut c = self.core();
            if c.done {
                drop(c);
                (held.release)();
                return Ok(());
            }
            c.attach_epoch += 1;
            c.attach_id = None;
            for event in held.history {
                match event {
                    WatchAttachEvent::Snapshot {
                        attach_id,
                        durable_session_id,
                    } => {
                        if let Some(dur) = durable_session_id {
                            c.durable = Some(dur);
                        }
                        c.attach_id = Some(attach_id);
                        c.last_seq = 0;
                    }
                    WatchAttachEvent::Records {
                        seqs: Some(seqs), ..
                    } => {
                        if let Some(max_seq) = seqs.into_iter().max() {
                            c.last_seq = c.last_seq.max(max_seq);
                        }
                    }
                    _ => {}
                }
            }
            c.release_held = Some(Arc::clone(&held.release));
            c.attach_epoch
        };
        let me = Arc::clone(self);
        let unsub = (held.subscribe)(Arc::new(move |event| me.accept_live(epoch, event)));
        let mut c = self.core();
        if c.done || c.attach_epoch != epoch {
            drop(c);
            unsub();
        } else {
            c.unsubscribe = Some(unsub);
        }
        Ok(())
    }

    fn accept_live(&self, epoch: u64, event: WatchAttachEvent) {
        let mut c = self.core();
        if c.done || c.attach_epoch != epoch {
            return;
        }
        match event {
            WatchAttachEvent::Snapshot {
                attach_id,
                durable_session_id,
            } => {
                if let Some(dur) = durable_session_id {
                    c.durable = Some(dur);
                }
                if c.attach_id.is_some() {
                    drop(c);
                    self.enqueue(WatchItem::Snapshot { attach_id });
                } else {
                    c.attach_id = Some(attach_id);
                    c.last_seq = 0;
                }
            }
            WatchAttachEvent::Records {
                attach_id,
                records,
                seqs,
            } => {
                drop(c);
                self.enqueue(WatchItem::Records {
                    attach_id,
                    records,
                    seqs,
                });
            }
            WatchAttachEvent::Ended { reason } => {
                drop(c);
                self.enqueue(WatchItem::Ended { reason });
            }
        }
    }

    fn apply(&self, record: &Value) -> bool {
        let record_type = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        {
            let mut c = self.core();
            if record_type == "agent_settled" {
                let turn = non_empty(record.get("turnId")).or_else(|| c.running_turn.clone());
                if let Some(ref turn_id) = turn {
                    if c.settled.contains(turn_id) {
                        return true;
                    }
                    Self::mark_settled(&mut c, turn_id.clone());
                }
                c.running_turn = None;
            }
            if matches!(record_type, "agent_start" | "turn_start")
                && let Some(turn) = non_empty(record.get("turnId"))
            {
                c.running_turn = Some(turn);
            }
            if record_type == "session_status"
                && let Some(dur) = non_empty(record.get("durableSessionId"))
            {
                c.durable = Some(dur);
            }
        }
        if !self.deliver(|| (self.handlers.record)(record)) {
            return false;
        }
        let change = {
            let mut c = self.core();
            let next = status_of(record, c.saw_status_record);
            if record_type == "session_status" {
                c.saw_status_record = true;
            }
            let Some(next) = next else {
                return true;
            };
            if c.status.as_deref() == Some(next.as_str()) {
                return true;
            }
            let from = c.status.replace(next.clone());
            let cause = if record_type == "session_status" {
                cause_of(record)
            } else {
                (!record_type.is_empty()).then(|| record_type.to_owned())
            };
            RemoteStatusChange {
                from,
                to: next,
                derived: record_type != "session_status",
                cause,
            }
        };
        self.handlers
            .status
            .as_ref()
            .is_none_or(|h| self.deliver(|| h(&change)))
    }

    async fn resync_once(&self, restarted: bool) -> Result<bool, String> {
        let read = self.read_state().await;
        if self.core().done {
            return Ok(false);
        }
        let state = match read {
            Ok(state) => state,
            Err(reason) => {
                if !SESSION_GONE.contains(&reason.as_str()) {
                    return Err(reason);
                }
                if restarted {
                    let end = self
                        .core()
                        .left_reason
                        .clone()
                        .unwrap_or_else(|| "session_closed".to_owned());
                    self.finish(&end);
                    return Ok(false);
                }
                return Ok(true);
            }
        };
        let resync = {
            let mut c = self.core();
            c.lost_reported = false;
            let from = c.status.clone();
            let settled_turns = match c.running_turn.clone() {
                Some(turn)
                    if !(c.settled.contains(&turn)
                        || state.status == "running"
                            && state.current_turn_id.as_deref() == Some(turn.as_str())) =>
                {
                    Self::mark_settled(&mut c, turn.clone());
                    vec![turn]
                }
                _ => Vec::new(),
            };
            Self::adopt(&mut c, &state);
            RemoteResync {
                from,
                state,
                settled_turns,
                restarted,
            }
        };
        if let Some(ref resynced) = self.handlers.resynced {
            let _ = self.deliver(|| resynced(&resync));
        }
        Ok(false)
    }

    async fn relocate_key(&self, reason: &str) -> bool {
        self.release();
        let durable = {
            let mut c = self.core();
            c.left_reason = Some(reason.to_owned());
            c.durable.clone()
        };
        let Some(durable) = durable else {
            self.finish(reason);
            return false;
        };
        let mut wait = RETRY_FIRST_MS;
        loop {
            match (self.link.locate)(self.device.clone(), durable.clone()).await {
                Ok(new_key) => {
                    self.core().key = new_key;
                    return true;
                }
                Err(WatchLocateError::FenceRefused(_)) => {
                    self.report_lost("lookup_fence_timeout");
                    (self.link.delay)(wait).await;
                    wait = (wait * 2).min(RETRY_MAX_MS);
                    if self.core().done {
                        return false;
                    }
                }
                Err(WatchLocateError::Failed(_)) => {
                    self.finish(reason);
                    return false;
                }
            }
        }
    }

    async fn reattach(self: &Arc<Self>, lost: Option<&str>, mut restarted: bool, wait_first: bool) {
        self.release();
        if let Some(lost) = lost {
            self.report_lost(lost);
        }
        let mut wait = RETRY_FIRST_MS;
        if wait_first {
            (self.link.delay)(wait).await;
            wait = (wait * 2).min(RETRY_MAX_MS);
        }
        while !self.core().done {
            if let Err(code) = self.attach().await {
                if ACCESS_REFUSED.contains(&code.as_str()) {
                    self.finish(&format!("access:{code}"));
                    return;
                }
                if SESSION_GONE.contains(&code.as_str()) {
                    let end = self
                        .core()
                        .left_reason
                        .clone()
                        .unwrap_or_else(|| "session_closed".to_owned());
                    self.finish(&end);
                    return;
                }
                self.report_lost(if code.is_empty() {
                    "attach_failed"
                } else {
                    &code
                });
                (self.link.delay)(wait).await;
                wait = (wait * 2).min(RETRY_MAX_MS);
                continue;
            }
            match self.resync_once(restarted).await {
                Ok(false) => return,
                Ok(true) => {
                    if !self.relocate_key("session_closed").await {
                        return;
                    }
                    restarted = true;
                    wait = RETRY_FIRST_MS;
                }
                Err(reason) => {
                    self.release();
                    self.report_lost(&reason);
                    (self.link.delay)(wait).await;
                    wait = (wait * 2).min(RETRY_MAX_MS);
                }
            }
        }
    }

    async fn resync_or_reattach(self: &Arc<Self>) {
        match self.resync_once(false).await {
            Ok(false) => {}
            Ok(true) => {
                if self.relocate_key("session_closed").await {
                    self.reattach(None, true, false).await;
                }
            }
            Err(reason) => {
                self.reattach(Some(&reason), false, true).await;
            }
        }
    }

    async fn process(self: &Arc<Self>, item: WatchItem) {
        match item {
            WatchItem::Records {
                attach_id,
                records,
                seqs,
            } => {
                {
                    let mut c = self.core();
                    if c.attach_id != Some(attach_id) {
                        c.attach_id = Some(attach_id);
                        c.last_seq = 0;
                    }
                }
                for (index, record) in records.iter().enumerate() {
                    if let Some(seq) = seqs.as_ref().and_then(|s| s.get(index).copied()) {
                        let mut c = self.core();
                        if seq <= c.last_seq {
                            continue;
                        }
                        c.last_seq = seq;
                    }
                    if !self.apply(record) {
                        return;
                    }
                }
            }
            WatchItem::Snapshot { attach_id } => {
                {
                    let mut c = self.core();
                    c.attach_id = Some(attach_id);
                    c.last_seq = 0;
                }
                self.resync_or_reattach().await;
            }
            WatchItem::Resync => {
                self.resync_or_reattach().await;
            }
            WatchItem::Ended { reason } => {
                self.release();
                if ACCESS_ENDED.contains(&reason.as_str()) {
                    self.finish(&format!("access:{reason}"));
                } else if SESSION_LEFT.contains(&reason.as_str()) {
                    let left = if reason == "session_replaced" {
                        "session_replaced"
                    } else {
                        "session_closed"
                    };
                    if self.relocate_key(left).await {
                        self.reattach(None, true, false).await;
                    }
                } else {
                    self.reattach(Some(&reason), false, false).await;
                }
            }
        }
    }

    async fn drain_loop(self: Arc<Self>) {
        loop {
            let next = lock_unpoison(&self.queue).pop();
            match next {
                Some(item) => self.process(item).await,
                None => {
                    if self.core().done {
                        return;
                    }
                    self.wake.notified().await;
                }
            }
        }
    }
}

/// Opens a remote session watch (`remote-watch.ts` `openSessionWatch`).
pub async fn open_session_watch(
    link: WatchLink,
    device: &str,
    session_id: &str,
    handlers: RemoteWatchHandlers,
    options: RemoteWatchOptions,
) -> RemoteWatchOutcome {
    let (end_tx, end_rx) = oneshot::channel::<String>();
    let watch = Arc::new(SessionWatch {
        link,
        device: device.to_owned(),
        handlers,
        core: Mutex::new(WatchCore {
            key: session_id.to_owned(),
            attach_epoch: 0,
            unsubscribe: None,
            release_held: None,
            attach_id: None,
            last_seq: 0,
            status: None,
            durable: None,
            running_turn: None,
            saw_status_record: false,
            settled: VecDeque::new(),
            lost_reported: false,
            left_reason: None,
            done: false,
            end_tx: Some(end_tx),
        }),
        queue: Mutex::new(WatchQueueState::default()),
        wake: Notify::new(),
    });
    if let Err(code) = watch.attach().await {
        watch.finish("stopped");
        return RemoteWatchOutcome::Refused {
            reason: if code.is_empty() {
                "attach_failed".to_owned()
            } else {
                code
            },
        };
    }
    let initial = if options.baseline {
        match watch.read_state().await {
            Ok(state) => {
                SessionWatch::adopt(&mut watch.core(), &state);
                Some(state)
            }
            Err(reason) => {
                watch.finish("stopped");
                return RemoteWatchOutcome::Refused { reason };
            }
        }
    } else {
        None
    };
    let runner = Arc::clone(&watch);
    tokio::spawn(async move {
        runner.drain_loop().await;
    });
    let stop_watch = Arc::clone(&watch);
    RemoteWatchOutcome::Ok {
        watch: RemoteSessionWatch {
            initial,
            ended: Box::pin(async move { end_rx.await.unwrap_or_else(|_| "stopped".to_owned()) }),
            stop: Arc::new(move || stop_watch.finish("stopped")),
        },
    }
}

#[cfg(test)]
#[path = "remote_watch_tests.rs"]
mod tests;
