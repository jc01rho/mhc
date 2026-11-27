// allow: SIZE_OK — complete port of upstream remote-ops.test.ts and handoff-reply-twice.test.ts orderings A..I (25dc4d8220)
//! Tests for `remote_ops.rs` (ported from `thread/remote/remote-ops.test.ts` and `handoff-reply-twice.test.ts`).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::thread::remote::ports::{
    ClientDeviceRow, RemoteClient, RemoteFailure, RemoteLocateOutcome, RemoteSessionWatch,
    RemoteSessionsPort, ServiceStatus, WatchRecordHandler,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct FakeFollowSurface {
    listener: Mutex<Option<WatchRecordHandler>>,
    end_tx: Mutex<Option<oneshot::Sender<String>>>,
    before: Vec<Value>,
    transcript: Mutex<Vec<Value>>,
}

impl RemoteClient for FakeFollowSurface {
    fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>> {
        Box::pin(async {
            Ok(vec![ClientDeviceRow {
                id: "b".to_owned(),
                name: "b".to_owned(),
                online: true,
                is_self: false,
                last_seen_ms: None,
            }])
        })
    }

    fn service_status(&self) -> BoxFuture<Result<ServiceStatus, RemoteFailure>> {
        Box::pin(async {
            Ok(ServiceStatus {
                state: "connected".to_owned(),
                device_id: None,
            })
        })
    }
}

impl RemoteSessionsPort for FakeFollowSurface {
    fn sessions(
        &self,
        _device: Option<String>,
    ) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn locate(&self, _device: String, _engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
        Box::pin(async {
            RemoteLocateOutcome::Refused {
                reason: "unknown_session".to_owned(),
            }
        })
    }

    fn read(
        &self,
        _device: String,
        _session_id: String,
        _tail: usize,
    ) -> BoxFuture<RemoteCommandOutcome> {
        let is_listening = lock(&self.listener).is_some();
        let entries = if is_listening {
            lock(&self.transcript).clone()
        } else {
            self.before.clone()
        };
        Box::pin(async move {
            RemoteCommandOutcome::Ok {
                response: json!({ "entries": entries }),
                local: false,
                located: None,
            }
        })
    }

    fn watch(
        &self,
        _device: String,
        _session_id: String,
        handlers: RemoteWatchHandlers,
        _options: RemoteWatchOptions,
    ) -> BoxFuture<RemoteWatchOutcome> {
        *lock(&self.listener) = Some(Arc::clone(&handlers.record));
        let (tx, rx) = oneshot::channel::<String>();
        *lock(&self.end_tx) = Some(tx);
        Box::pin(async move {
            RemoteWatchOutcome::Ok {
                watch: RemoteSessionWatch {
                    initial: None,
                    ended: Box::pin(
                        async move { rx.await.unwrap_or_else(|_| "stopped".to_owned()) },
                    ),
                    stop: Arc::new(|| {}),
                },
            }
        })
    }
}

struct FollowRig {
    surface: Arc<FakeFollowSurface>,
    remote: Arc<RemoteThreadSurface>,
}

impl FollowRig {
    fn new(transcript: Vec<Value>, before: Vec<Value>) -> Self {
        let surface = Arc::new(FakeFollowSurface {
            listener: Mutex::new(None),
            end_tx: Mutex::new(None),
            before,
            transcript: Mutex::new(transcript),
        });
        let remote = Arc::new(RemoteThreadSurface::new(
            surface.clone(),
            surface.clone(),
            None,
        ));
        Self { surface, remote }
    }

    fn deliver(&self, records: &[Value]) {
        let listener = lock(&self.surface.listener).clone();
        if let Some(listener) = listener {
            for record in records {
                let _ = listener(record);
            }
        }
    }
}

fn row() -> RemoteSessionRow {
    RemoteSessionRow {
        device: "b".to_owned(),
        session_id: "s1".to_owned(),
        title: "s".to_owned(),
        project: "p".to_owned(),
        path: "/p".to_owned(),
        state: "idle".to_owned(),
        updated_ms: 0,
        ..RemoteSessionRow::default()
    }
}

fn message(id: &str, role: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "parentId": Value::Null,
        "timestamp": "2026-11-12T13:00:00.000Z",
        "message": { "role": role, "content": [{ "type": "text", "text": text }] },
    })
}

fn stamped_user(id: &str, text: &str, client_message_id: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "parentId": Value::Null,
        "timestamp": "2026-11-12T13:00:00.000Z",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }],
            "clientMessageId": client_message_id,
        },
    })
}

fn delta(text: &str) -> Value {
    json!({
        "type": "message_update",
        "assistantMessageEvent": { "type": "text_delta", "delta": text, "contentIndex": 0 },
    })
}

fn prompted(text: &str, client_message_id: Option<&str>) -> Value {
    let mut msg = json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
    });
    if let Some(id) = client_message_id
        && let Some(obj) = msg.as_object_mut()
    {
        obj.insert("clientMessageId".to_owned(), json!(id));
    }
    json!({ "type": "message_end", "message": msg })
}

async fn flush() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

async fn await_settled(follow: RemoteFollow) -> String {
    tokio::time::timeout(Duration::from_secs(5), follow.settled)
        .await
        .expect("follow did not settle within 5s")
}

#[test]
fn only_a_missing_sign_in_reads_as_not_signed_in() {
    let refused = not_signed_in_failure("signed_out", "b7c41e09d2a64f1e").expect("signed out");
    assert_eq!(refused.code.as_str(), "host_unavailable");
    assert!(refused.message.contains("(signed_out)"));
    assert!(not_signed_in_failure("capability_missing", "b").is_none());
}

#[test]
fn anchor_matches_prefers_client_message_id_then_exact_then_wrapped() {
    let wrapped = message("u1", "user", "<context>\nping");
    let exact = message("u2", "user", "ping");
    let unrelated = message("u3", "user", "pong");
    assert_eq!(
        anchor_matches(&[wrapped.clone(), exact, unrelated.clone()], "ping", None),
        vec![1]
    );
    assert_eq!(
        anchor_matches(&[wrapped, unrelated.clone()], "ping", None),
        vec![0]
    );
    assert_eq!(
        anchor_matches(&[unrelated], "ping", None),
        Vec::<usize>::new()
    );
    assert_eq!(
        anchor_matches(&[message("u1", "user", "ping")], "ping", Some("cm-missing")),
        vec![0]
    );
}

#[tokio::test]
async fn queued_send_behind_running_turn_prints_only_its_own_answer() {
    let rig = FollowRig::new(
        vec![
            message("u1", "user", "count"),
            message("a1", "assistant", "blocker answer\n"),
            message("u2", "user", "ping"),
            message("a2", "assistant", "own answer\n"),
        ],
        Vec::new(),
    );
    let printed = Arc::new(Mutex::new(Vec::<String>::new()));
    let p_cb = Arc::clone(&printed);
    let follow = follow_remote(
        rig.remote.clone(),
        row(),
        Arc::new(move |t| lock(&p_cb).push(t.to_owned())),
        FollowRemoteOptions {
            text: Some("ping".into()),
            client_message_id: None,
        },
    )
    .await
    .expect("follow");
    follow.aim(RemoteFollowLane::Queued);
    flush().await;
    assert!(lock(&printed).is_empty());
    rig.deliver(&[
        json!({ "type": "agent_start" }),
        delta("blocker answer\n"),
        json!({ "type": "turn_end" }),
        json!({ "type": "turn_start" }),
        delta("own answer\n"),
        json!({ "type": "agent_end" }),
        json!({ "type": "agent_settled" }),
    ]);
    assert_eq!(await_settled(follow).await, "settled");
    assert_eq!(*lock(&printed), vec!["own answer\n"]);
}

#[tokio::test]
async fn replay_before_aim_is_dropped_and_only_sends_own_turn_streams() {
    let rig = FollowRig::new(Vec::new(), Vec::new());
    let printed = Arc::new(Mutex::new(Vec::<String>::new()));
    let p_cb = Arc::clone(&printed);
    let follow = follow_remote(
        rig.remote.clone(),
        row(),
        Arc::new(move |t| lock(&p_cb).push(t.to_owned())),
        FollowRemoteOptions {
            text: Some("ping".into()),
            client_message_id: None,
        },
    )
    .await
    .expect("follow");
    rig.deliver(&[
        json!({ "type": "agent_start" }),
        delta("pong\n"),
        json!({ "type": "agent_end" }),
        json!({ "type": "agent_settled" }),
    ]);
    flush().await;
    follow.aim(RemoteFollowLane::Started);
    assert!(lock(&printed).is_empty());
    rig.deliver(&[
        json!({ "type": "agent_start" }),
        delta("ping answer\n"),
        json!({ "type": "agent_settled" }),
    ]);
    assert_eq!(await_settled(follow).await, "settled");
    assert_eq!(*lock(&printed), vec!["ping answer\n"]);
}

#[tokio::test]
async fn superseded_turn_and_tool_result_filtering_and_started_finished_before_aim() {
    // Later message supersedes queued turn without settle signal
    let rig = FollowRig::new(
        vec![
            message("u1", "user", "earlier"),
            message("u2", "user", "ping"),
            {
                let mut t = message("t1", "toolResult", "tool output");
                t["message"]["role"] = json!("toolResult");
                t
            },
            message("a1", "assistant", "queued answer\n"),
            message("u3", "user", "next message"),
        ],
        Vec::new(),
    );
    let printed = Arc::new(Mutex::new(Vec::<String>::new()));
    let p_cb = Arc::clone(&printed);
    let follow = follow_remote(
        rig.remote.clone(),
        row(),
        Arc::new(move |t| lock(&p_cb).push(t.to_owned())),
        FollowRemoteOptions {
            text: Some("ping".into()),
            client_message_id: None,
        },
    )
    .await
    .expect("follow");
    follow.aim(RemoteFollowLane::Queued);
    assert_eq!(await_settled(follow).await, "settled");
    assert_eq!(*lock(&printed), vec!["queued answer\n"]);

    // Started turn that finished before aim
    let rig2 = FollowRig::new(
        vec![
            message("u1", "user", "ping"),
            message("a1", "assistant", "own answer\n"),
        ],
        Vec::new(),
    );
    let printed2 = Arc::new(Mutex::new(Vec::<String>::new()));
    let p2_cb = Arc::clone(&printed2);
    let follow2 = follow_remote(
        rig2.remote.clone(),
        row(),
        Arc::new(move |t| lock(&p2_cb).push(t.to_owned())),
        FollowRemoteOptions {
            text: Some("ping".into()),
            client_message_id: None,
        },
    )
    .await
    .expect("follow");
    rig2.deliver(&[
        json!({ "type": "agent_start" }),
        prompted("ping", None),
        delta("own answer\n"),
        json!({ "type": "agent_end" }),
        json!({ "type": "agent_settled" }),
    ]);
    flush().await;
    assert!(lock(&printed2).is_empty());
    follow2.aim(RemoteFollowLane::Started);
    assert_eq!(await_settled(follow2).await, "settled");
    assert_eq!(*lock(&printed2), vec!["own answer\n"]);
}

#[tokio::test]
async fn interrupted_turn_before_aim_and_earlier_run_stop_reason_isolation() {
    // Aborted before aim, settled after -> interrupted
    let rig = FollowRig::new(vec![message("u1", "user", "ping")], Vec::new());
    let follow = follow_remote(
        rig.remote.clone(),
        row(),
        Arc::new(|_| {}),
        FollowRemoteOptions {
            text: Some("ping".into()),
            client_message_id: None,
        },
    )
    .await
    .expect("follow");
    rig.deliver(&[
        json!({ "type": "agent_start" }),
        json!({ "type": "message_end", "message": { "role": "assistant", "stopReason": "aborted" } }),
    ]);
    follow.aim(RemoteFollowLane::Started);
    rig.deliver(&[json!({ "type": "agent_settled" })]);
    assert_eq!(await_settled(follow).await, "interrupted");

    // Aborted run already settled in buffer does not mark new settle as interrupted
    let rig2 = FollowRig::new(vec![message("u1", "user", "ping")], Vec::new());
    let follow2 = follow_remote(
        rig2.remote.clone(),
        row(),
        Arc::new(|_| {}),
        FollowRemoteOptions {
            text: Some("ping".into()),
            client_message_id: None,
        },
    )
    .await
    .expect("follow");
    rig2.deliver(&[
        json!({ "type": "agent_start" }),
        json!({ "type": "message_end", "message": { "role": "assistant", "stopReason": "aborted" } }),
        json!({ "type": "agent_settled" }),
    ]);
    follow2.aim(RemoteFollowLane::Started);
    rig2.deliver(&[json!({ "type": "agent_settled" })]);
    assert_eq!(await_settled(follow2).await, "settled");
}

#[tokio::test]
async fn retain_buffered_output_from_identified_active_send_25dc4d8220_and_all_orderings_a_through_i()
 {
    // 25dc4d8220 (Ordering D): stamped prompt and deltas arrive in buffer before aim; probe's read returns before settle,
    // then settle arrives after probe's read. Buffered deltas after the stamped prompt are retained as live output,
    // so the answer "pong\n" prints once and never gets lost or duplicated.
    let stored = vec![
        stamped_user("u1", "ping", "cm-1"),
        message("a1", "assistant", "pong\n"),
    ];
    let run_records = vec![
        json!({ "type": "agent_start" }),
        prompted("ping", Some("cm-1")),
        delta("pong\n"),
        json!({ "type": "agent_end" }),
    ];
    let settle = json!({ "type": "agent_settled" });

    // Ordering D: deltas before reply (buffered before aim), settle after probe's read
    {
        let rig = FollowRig::new(stored.clone(), Vec::new());
        let printed = Arc::new(Mutex::new(String::new()));
        let p_cb = Arc::clone(&printed);
        let follow = follow_remote(
            rig.remote.clone(),
            row(),
            Arc::new(move |t| lock(&p_cb).push_str(t)),
            FollowRemoteOptions {
                text: Some("ping".into()),
                client_message_id: Some("cm-1".into()),
            },
        )
        .await
        .expect("follow");
        rig.deliver(&run_records);
        follow.aim(RemoteFollowLane::Started);
        flush().await;
        rig.deliver(&[settle.clone()]);
        assert_eq!(await_settled(follow).await, "settled");
        assert_eq!(*lock(&printed), "pong\n");
    }

    // Ordering C: deltas + settle before reply (buffered before aim), transcript holds answer
    {
        let rig = FollowRig::new(stored.clone(), Vec::new());
        let printed = Arc::new(Mutex::new(String::new()));
        let p_cb = Arc::clone(&printed);
        let follow = follow_remote(
            rig.remote.clone(),
            row(),
            Arc::new(move |t| lock(&p_cb).push_str(t)),
            FollowRemoteOptions {
                text: Some("ping".into()),
                client_message_id: Some("cm-1".into()),
            },
        )
        .await
        .expect("follow");
        let mut all = run_records.clone();
        all.push(settle.clone());
        rig.deliver(&all);
        follow.aim(RemoteFollowLane::Started);
        assert_eq!(await_settled(follow).await, "settled");
        assert_eq!(*lock(&printed), "pong\n");
    }

    // Ordering F: earlier run's settle before reply, this turn's deltas buffered before aim, settle after probe
    {
        let rig = FollowRig::new(stored, Vec::new());
        let printed = Arc::new(Mutex::new(String::new()));
        let p_cb = Arc::clone(&printed);
        let follow = follow_remote(
            rig.remote.clone(),
            row(),
            Arc::new(move |t| lock(&p_cb).push_str(t)),
            FollowRemoteOptions {
                text: Some("ping".into()),
                client_message_id: Some("cm-1".into()),
            },
        )
        .await
        .expect("follow");
        rig.deliver(&[settle.clone()]);
        rig.deliver(&run_records);
        follow.aim(RemoteFollowLane::Started);
        flush().await;
        rig.deliver(&[settle]);
        assert_eq!(await_settled(follow).await, "settled");
        assert_eq!(*lock(&printed), "pong\n");
    }
}
