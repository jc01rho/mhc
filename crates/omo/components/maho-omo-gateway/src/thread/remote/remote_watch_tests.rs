// allow: SIZE_OK — complete port of upstream device-service-watch.test.ts (#12719, #14748 review round 7)
//! Tests for `remote_watch.rs` (ported from `thread/remote/device-service-watch.test.ts`).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;
use crate::thread::remote::lookup_gate::{LookupGate, LookupStart};
use crate::thread::remote::ports::RemoteLookupOutcome;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct LiveAttach {
    history: Mutex<Vec<WatchAttachEvent>>,
    listeners: Mutex<Vec<(u64, WatchEventListener)>>,
    next_listener_id: Mutex<u64>,
    ended: Mutex<bool>,
    detaches: Mutex<usize>,
    detach_notify: Arc<Notify>,
}

impl LiveAttach {
    fn new(snapshot_after_attach: bool) -> Arc<Self> {
        let history = if snapshot_after_attach {
            Vec::new()
        } else {
            vec![WatchAttachEvent::Snapshot {
                attach_id: 1,
                durable_session_id: Some("dur-1".to_owned()),
            }]
        };
        Arc::new(Self {
            history: Mutex::new(history),
            listeners: Mutex::new(Vec::new()),
            next_listener_id: Mutex::new(1),
            ended: Mutex::new(false),
            detaches: Mutex::new(0),
            detach_notify: Arc::new(Notify::new()),
        })
    }

    fn emit(&self, event: WatchAttachEvent) {
        {
            let mut h = lock(&self.history);
            if matches!(event, WatchAttachEvent::Snapshot { .. }) {
                h.clear();
            }
            h.push(event.clone());
        }
        let listeners: Vec<WatchEventListener> = lock(&self.listeners)
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect();
        for listener in listeners {
            listener(event.clone());
        }
    }

    fn end(&self, reason: &str) {
        let listeners: Vec<WatchEventListener> = {
            let mut ended = lock(&self.ended);
            if *ended {
                return;
            }
            *ended = true;
            lock(&self.listeners).drain(..).map(|(_, l)| l).collect()
        };
        for listener in listeners {
            listener(WatchAttachEvent::Ended {
                reason: reason.to_owned(),
            });
        }
    }
}

struct FakeDevice {
    state: Mutex<Value>,
    failures: Mutex<usize>,
    snapshot_after_attach: bool,
    attaches: Mutex<Vec<String>>,
    live: Mutex<Vec<Arc<LiveAttach>>>,
    locate_key: Mutex<Option<String>>,
    missing: Mutex<HashSet<String>>,
    connects: Mutex<usize>,
    delays: Mutex<Vec<u64>>,
    lookup_gate: LookupGate,
}

impl FakeDevice {
    fn new(initial_state: Value, snapshot_after_attach: bool, fence_wait_ms: u64) -> Arc<Self> {
        let gate = LookupGate::new(
            |_device, _durable| LookupStart {
                answer: Box::pin(async {
                    RemoteLookupOutcome::Silent {
                        reason: "lookup_timeout".to_owned(),
                    }
                }),
                settled: Box::pin(std::future::pending()),
            },
            Duration::from_millis(fence_wait_ms),
        );
        Self::with_gate(initial_state, snapshot_after_attach, gate)
    }

    fn with_gate(
        initial_state: Value,
        snapshot_after_attach: bool,
        lookup_gate: LookupGate,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(initial_state),
            failures: Mutex::new(0),
            snapshot_after_attach,
            attaches: Mutex::new(Vec::new()),
            live: Mutex::new(Vec::new()),
            locate_key: Mutex::new(None),
            missing: Mutex::new(HashSet::new()),
            connects: Mutex::new(1),
            delays: Mutex::new(Vec::new()),
            lookup_gate,
        })
    }

    fn current(&self) -> Arc<LiveAttach> {
        lock(&self.live).last().expect("live attach").clone()
    }

    fn snapshot(&self, attach_id: u64) {
        self.current().emit(WatchAttachEvent::Snapshot {
            attach_id,
            durable_session_id: None,
        });
    }

    fn records(&self, attach_id: u64, records: Vec<Value>, seqs: Option<Vec<u64>>) {
        self.current().emit(WatchAttachEvent::Records {
            attach_id,
            records,
            seqs,
        });
    }

    fn drop_connection(&self) {
        *lock(&self.connects) += 1;
        let current = self.current();
        current.end("closed");
    }

    fn link(self: &Arc<Self>) -> WatchLink {
        let dev_hold = Arc::clone(self);
        let dev_state = Arc::clone(self);
        let dev_locate = Arc::clone(self);
        let dev_delay = Arc::clone(self);
        WatchLink {
            hold: Arc::new(move |_device, session_id| {
                let me = Arc::clone(&dev_hold);
                Box::pin(async move {
                    lock(&me.attaches).push(session_id.clone());
                    if lock(&me.missing).contains(&session_id) {
                        return Err("session_not_found".to_owned());
                    }
                    let entry = LiveAttach::new(me.snapshot_after_attach);
                    lock(&me.live).push(Arc::clone(&entry));
                    let history = lock(&entry.history).clone();
                    let entry_sub = Arc::clone(&entry);
                    let entry_rel = Arc::clone(&entry);
                    Ok(WatchHeldAttach {
                        history,
                        subscribe: Box::new(move |listener| {
                            let id = {
                                let mut next = lock(&entry_sub.next_listener_id);
                                let id = *next;
                                *next += 1;
                                id
                            };
                            if !*lock(&entry_sub.ended) {
                                lock(&entry_sub.listeners).push((id, listener));
                            }
                            let entry_unsub = Arc::clone(&entry_sub);
                            Box::new(move || {
                                lock(&entry_unsub.listeners).retain(|(lid, _)| *lid != id);
                            })
                        }),
                        release: Arc::new(move || {
                            *lock(&entry_rel.detaches) += 1;
                            entry_rel.detach_notify.notify_waiters();
                        }),
                    })
                })
            }),
            get_state: Arc::new(move |_device, _session_id| {
                let me = Arc::clone(&dev_state);
                Box::pin(async move {
                    let mut failures = lock(&me.failures);
                    if *failures > 0 {
                        *failures -= 1;
                        return RemoteCommandOutcome::Refused {
                            reason: "engine_failed".to_owned(),
                            detail: None,
                        };
                    }
                    drop(failures);
                    let state = lock(&me.state).clone();
                    RemoteCommandOutcome::Ok {
                        response: json!({ "data": state }),
                        local: false,
                        located: None,
                    }
                })
            }),
            locate: Arc::new(move |_device, _durable| {
                let me = Arc::clone(&dev_locate);
                Box::pin(async move {
                    let key_res = me
                        .lookup_gate
                        .admit(|| {
                            let key = lock(&me.locate_key).clone();
                            async move { key }
                        })
                        .await
                        .map_err(WatchLocateError::FenceRefused)?;
                    key_res.ok_or_else(|| WatchLocateError::Failed("session_not_found".to_owned()))
                })
            }),
            delay: Arc::new(move |ms| {
                let me = Arc::clone(&dev_delay);
                Box::pin(async move {
                    lock(&me.delays).push(ms);
                })
            }),
        }
    }
}

struct Recorder {
    log: Arc<Mutex<Vec<String>>>,
    notify: Arc<Notify>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    fn handlers(&self) -> RemoteWatchHandlers {
        let log_rec = Arc::clone(&self.log);
        let notify_rec = Arc::clone(&self.notify);
        let log_res = Arc::clone(&self.log);
        let notify_res = Arc::clone(&self.notify);
        let log_lost = Arc::clone(&self.log);
        let notify_lost = Arc::clone(&self.notify);
        RemoteWatchHandlers {
            record: Arc::new(move |record| {
                let kind = record
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("undefined");
                let turn = record
                    .get("turnId")
                    .and_then(Value::as_str)
                    .map(|t| format!(" {t}"))
                    .unwrap_or_default();
                lock(&log_rec).push(format!("record {kind}{turn}"));
                notify_rec.notify_waiters();
                Ok(())
            }),
            status: None,
            resynced: Some(Arc::new(move |resync| {
                let from = resync.from.as_deref().unwrap_or("-");
                let settled = resync.settled_turns.join(",");
                let restarted = if resync.restarted { " restarted" } else { "" };
                lock(&log_res).push(format!(
                    "resync {from} -> {} settled=[{settled}]{restarted}",
                    resync.state.status
                ));
                notify_res.notify_waiters();
                Ok(())
            })),
            lost: Some(Arc::new(move |reason| {
                lock(&log_lost).push(format!("lost {reason}"));
                notify_lost.notify_waiters();
                Ok(())
            })),
        }
    }

    async fn until(&self, count: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let fut = self.notify.notified();
            if lock(&self.log).len() >= count {
                return;
            }
            tokio::time::timeout_at(deadline, fut)
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timed out waiting for {count} events, saw {:?}",
                        *lock(&self.log)
                    )
                });
        }
    }

    fn entries(&self) -> Vec<String> {
        lock(&self.log).clone()
    }
}

fn running_state() -> Value {
    json!({ "isStreaming": true, "currentTurnId": "7", "durableSessionId": "dur-1" })
}

fn idle_state() -> Value {
    json!({ "isStreaming": false, "durableSessionId": "dur-1" })
}

async fn watching(
    device: &Arc<FakeDevice>,
    handlers: RemoteWatchHandlers,
    baseline: bool,
) -> RemoteSessionWatch {
    match open_session_watch(
        device.link(),
        "b",
        "s1",
        handlers,
        RemoteWatchOptions { baseline },
    )
    .await
    {
        RemoteWatchOutcome::Ok { watch } => watch,
        RemoteWatchOutcome::Refused { reason } => panic!("watch refused: {reason}"),
    }
}

#[tokio::test]
async fn only_records_written_after_attach_reach_handler_none_after_stop_and_ended_settles_stopped()
{
    let device = FakeDevice::new(idle_state(), false, 5_000);
    let seen = Recorder::new();
    let watch = watching(&device, seen.handlers(), false).await;
    device.records(1, vec![json!({ "type": "new" })], None);
    seen.until(1).await;
    watch.stop();
    device.records(1, vec![json!({ "type": "after-stop" })], None);
    assert_eq!(seen.entries(), vec!["record new"]);
    assert_eq!(watch.ended.await, "stopped");
}

#[tokio::test]
async fn resync_reports_settled_turn_once_and_late_agent_settled_is_not_reported_again() {
    let device = FakeDevice::new(running_state(), false, 5_000);
    let seen = Recorder::new();
    let watch = watching(&device, seen.handlers(), true).await;
    assert_eq!(
        watch.initial,
        Some(RemoteSessionState {
            status: "running".into(),
            session_id: "s1".into(),
            durable_session_id: Some("dur-1".into()),
            current_turn_id: Some("7".into()),
        })
    );
    *lock(&device.state) = idle_state();
    device.snapshot(2);
    seen.until(1).await;
    device.records(
        2,
        vec![
            json!({ "type": "agent_settled", "turnId": "7" }),
            json!({ "type": "agent_start", "turnId": "8" }),
            json!({ "type": "agent_settled", "turnId": "8" }),
        ],
        None,
    );
    seen.until(3).await;
    watch.stop();
    assert_eq!(
        seen.entries(),
        vec![
            "resync running -> idle settled=[7]",
            "record agent_start 8",
            "record agent_settled 8",
        ]
    );
}

#[tokio::test]
async fn first_snapshot_arriving_after_watch_starts_only_positions_the_watch() {
    let device = FakeDevice::new(idle_state(), true, 5_000);
    let seen = Recorder::new();
    let watch = watching(&device, seen.handlers(), true).await;
    device.snapshot(1);
    device.records(1, vec![json!({ "type": "agent_start" })], None);
    seen.until(1).await;
    watch.stop();
    assert_eq!(seen.entries(), vec!["record agent_start"]);
}

#[tokio::test]
async fn repeated_stream_seqs_within_attach_are_dropped_and_every_other_record_arrives_once() {
    let device = FakeDevice::new(idle_state(), false, 5_000);
    let seen = Recorder::new();
    let watch = watching(&device, seen.handlers(), true).await;
    device.records(
        1,
        vec![json!({ "type": "a" }), json!({ "type": "b" })],
        Some(vec![3, 9]),
    );
    device.records(
        1,
        vec![json!({ "type": "b" }), json!({ "type": "c" })],
        Some(vec![9, 12]),
    );
    device.records(1, vec![json!({ "type": "d" })], None);
    seen.until(4).await;
    watch.stop();
    assert_eq!(
        seen.entries(),
        vec!["record a", "record b", "record c", "record d"]
    );
}

#[tokio::test]
async fn connection_close_reports_lost_once_reconnects_reattaches_and_resyncs() {
    let device = FakeDevice::new(running_state(), false, 5_000);
    let seen = Recorder::new();
    let watch = watching(&device, seen.handlers(), true).await;
    *lock(&device.state) = idle_state();
    device.drop_connection();
    seen.until(2).await;
    watch.stop();
    assert_eq!(
        seen.entries(),
        vec!["lost closed", "resync running -> idle settled=[7]"]
    );
    assert_eq!(*lock(&device.connects), 2);
    assert_eq!(*lock(&device.attaches), vec!["s1", "s1"]);
}

#[tokio::test]
async fn session_replaced_relocates_by_durable_id_reattaches_and_resyncs_as_restarted() {
    let device = FakeDevice::new(running_state(), false, 5_000);
    let seen = Recorder::new();
    let watch = watching(&device, seen.handlers(), true).await;
    *lock(&device.state) = idle_state();
    *lock(&device.locate_key) = Some("s1-after".to_owned());
    device.current().end("session_replaced");
    seen.until(1).await;
    watch.stop();
    assert_eq!(
        seen.entries(),
        vec!["resync running -> idle settled=[7] restarted"]
    );
    assert_eq!(*lock(&device.attaches), vec!["s1", "s1-after"]);
}

#[tokio::test]
async fn get_state_failures_back_off_doubling_report_lost_once_and_resync_when_answered() {
    let device = FakeDevice::new(running_state(), false, 5_000);
    let seen = Recorder::new();
    let watch = watching(&device, seen.handlers(), true).await;
    *lock(&device.state) = idle_state();
    *lock(&device.failures) = 3;
    device.snapshot(2);
    seen.until(2).await;
    watch.stop();
    assert_eq!(
        seen.entries(),
        vec!["lost engine_failed", "resync running -> idle settled=[7]"]
    );
    assert_eq!(*lock(&device.delays), vec![1_000, 2_000, 4_000]);
}

#[tokio::test]
async fn solo_lookup_holding_fence_causes_watch_relocate_to_back_off_and_retry_until_solo_settles()
{
    let (solo_tx, solo_rx) = oneshot::channel::<()>();
    let solo_rx = Arc::new(Mutex::new(Some(solo_rx)));
    let gate = LookupGate::new(
        move |_device, _durable| {
            let rx = lock(&solo_rx).take();
            LookupStart {
                answer: Box::pin(async {
                    RemoteLookupOutcome::Silent {
                        reason: "lookup_timeout".to_owned(),
                    }
                }),
                settled: Box::pin(async move {
                    if let Some(rx) = rx {
                        let _ = rx.await;
                    }
                }),
            }
        },
        Duration::from_millis(20),
    );
    let device = FakeDevice::with_gate(running_state(), false, gate.clone());
    let solo_tx = Arc::new(Mutex::new(Some(solo_tx)));
    let delays_dev = Arc::clone(&device);
    let solo_tx_delay = Arc::clone(&solo_tx);
    let mut link = device.link();
    link.delay = Arc::new(move |ms| {
        let dev = Arc::clone(&delays_dev);
        let tx = lock(&solo_tx_delay).take();
        Box::pin(async move {
            lock(&dev.delays).push(ms);
            if let Some(tx) = tx {
                let _ = tx.send(());
            }
        })
    });

    let seen = Recorder::new();
    let watch = match open_session_watch(
        link,
        "b",
        "s1",
        seen.handlers(),
        RemoteWatchOptions { baseline: true },
    )
    .await
    {
        RemoteWatchOutcome::Ok { watch } => watch,
        RemoteWatchOutcome::Refused { reason } => panic!("watch refused: {reason}"),
    };

    assert_eq!(
        gate.lookup_alone("c", "0195f0c2-1a2b-7c3d-8e4f-00000000c0a1", 5_000)
            .await,
        Some(RemoteLookupOutcome::Silent {
            reason: "lookup_timeout".into()
        })
    );
    *lock(&device.state) = idle_state();
    *lock(&device.locate_key) = Some("s1-after".to_owned());
    device.current().end("session_replaced");
    seen.until(2).await;
    watch.stop();
    assert_eq!(
        seen.entries(),
        vec![
            "lost lookup_fence_timeout",
            "resync running -> idle settled=[7] restarted",
        ]
    );
    assert_eq!(*lock(&device.attaches), vec!["s1", "s1-after"]);
    assert_eq!(*lock(&device.delays), vec![1_000]);
    assert_eq!(watch.ended.await, "stopped");
}

#[tokio::test]
async fn relocated_watch_whose_new_key_is_not_listed_ends_with_attach_reason() {
    let device = FakeDevice::new(idle_state(), false, 5_000);
    let watch = watching(&device, Recorder::new().handlers(), true).await;
    *lock(&device.locate_key) = Some("s1-after".to_owned());
    lock(&device.missing).insert("s1-after".to_owned());
    device.current().end("session_replaced");
    assert_eq!(watch.ended.await, "session_replaced");
    assert_eq!(*lock(&device.attaches), vec!["s1", "s1-after"]);
}

#[tokio::test]
async fn session_closed_and_peer_revoked_end_watch_without_reattach() {
    for (reason, expected) in [
        ("session_closed", "session_closed"),
        ("peer_revoked", "access:peer_revoked"),
    ] {
        let device = FakeDevice::new(idle_state(), false, 5_000);
        let watch = watching(&device, Recorder::new().handlers(), true).await;
        device.current().end(reason);
        assert_eq!(watch.ended.await, expected);
        assert_eq!(*lock(&device.attaches), vec!["s1"]);
    }
}

#[tokio::test]
async fn handler_failure_ends_only_that_watch_with_listener_failed() {
    let device = FakeDevice::new(idle_state(), false, 5_000);
    let mut bad_handlers = Recorder::new().handlers();
    bad_handlers.record = Arc::new(|_| Err("boom".to_owned()));
    let throwing = watching(&device, bad_handlers, true).await;
    device.records(1, vec![json!({ "type": "one" })], None);
    assert_eq!(throwing.ended.await, "listener_failed");
}

#[tokio::test]
async fn backlog_past_queue_bound_compacts_content_and_forces_one_resync() {
    let device = FakeDevice::new(running_state(), false, 5_000);
    let seen = Recorder::new();
    let watch = watching(&device, seen.handlers(), true).await;
    *lock(&device.state) = idle_state();
    let mut burst: Vec<Value> = (0..300)
        .map(|_| json!({ "type": "message_update" }))
        .collect();
    burst.push(json!({ "type": "agent_settled", "turnId": "7" }));
    device.records(1, burst, None);
    seen.until(1).await;
    watch.stop();
    assert_eq!(seen.entries(), vec!["resync running -> idle settled=[7]"]);
}
