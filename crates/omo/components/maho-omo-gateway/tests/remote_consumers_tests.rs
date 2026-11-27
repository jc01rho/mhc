// allow: SIZE_OK — end-to-end integration tests over FakeAgentd for #14748, 25dc4d8220, thread-create-remote-unreachable and thread-create-remote-lost-reply
//! Integration tests over the in-process `FakeAgentd` (real loopback TCP + real LF-delimited JSON frames) for the
//! `remote-consumers` track:
//! - `#14748`: lookup gate fence and admission rules, and `remote_watch` retrying a lookup-fence refusal
//! - `25dc4d8220`: `follow_remote` retaining buffered output from the identified active send
//! - `#14394`, `#14397`, `#14560`, `#14569`, `#14623` review M4/M5: `thread_create` remote unreachable route unload,
//!   far worktree creation and removal only after confirmed unload, and lost open reply adoption on the spot and on
//!   same-key retry.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use maho_omo_gateway::thread::adapter::{
    BoxFuture, GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort,
};
use maho_omo_gateway::thread::address_book::{
    AddressBookHost, HostListSessions, HostSessionStatus,
};
use maho_omo_gateway::thread::component::{
    GatewayThreadComponent, GatewayThreadComponentOptions, create_gateway_thread_component,
};
use maho_omo_gateway::thread::control::relay_control::RelayControlLocator;
use maho_omo_gateway::thread::control::session_host::created_thread_id;
use maho_omo_gateway::thread::engine::GatewayResolution;
use maho_omo_gateway::thread::errors::thread_tool_failure;
use maho_omo_gateway::thread::host_view::{
    SessionCommandPort, ThreadHost, ThreadHostSession, ThreadHostView,
};
use maho_omo_gateway::thread::remote::agentd::{AgentdClient, AgentdPeer, AgentdPorts};
use maho_omo_gateway::thread::remote::ports::{
    RemoteCommandOutcome, RemoteLocateOutcome, RemoteLookupOutcome, RemoteSessionRow,
    RemoteSessionsPort, RemoteThreadSurface, RemoteWatchHandlers, RemoteWatchOptions,
    RemoteWatchOutcome,
};
use maho_omo_gateway::thread::remote::remote_ops::{
    FollowRemoteOptions, RemoteFollowLane, follow_remote,
};
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::types::{AdmissionLedger, GatewayEndpointKind, Provenance};
use maho_omo_gateway::thread::worktree::worktree_rpc::{
    THREAD_WORKTREE_RPC, answer_worktree_request,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

const TOKEN: &str = "shared-agentd-token";
const CALLER: &str = "dur-caller";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
struct GateSignal {
    open: Arc<(Mutex<bool>, Condvar)>,
}

impl GateSignal {
    fn new() -> Self {
        Self {
            open: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    fn release(&self) {
        let (m, cv) = &*self.open;
        *lock(m) = true;
        cv.notify_all();
    }

    fn wait(&self, timeout: Duration) {
        let (m, cv) = &*self.open;
        let deadline = Instant::now() + timeout;
        let mut ready = lock(m);
        while !*ready {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            let (next, _) = cv
                .wait_timeout(ready, left)
                .unwrap_or_else(PoisonError::into_inner);
            ready = next;
        }
    }
}

enum Reply {
    Data(Value),
    Error(String),
    /// Sends no response frame at all, so the client's call times out (lost reply).
    Silence,
    /// Waits for the signal before sending the response frame on the live writer.
    After(GateSignal, Box<Reply>),
}

type AnswerFn = dyn Fn(&str, &Value) -> Option<Reply> + Send + Sync;

struct FakeAgentd {
    listener: TcpListener,
    port: u16,
    commands: Mutex<Vec<Value>>,
    writer: Arc<Mutex<Option<TcpStream>>>,
    stop: AtomicBool,
    answer: Box<AnswerFn>,
}

impl FakeAgentd {
    fn start<F>(answer: F) -> Arc<Self>
    where
        F: Fn(&str, &Value) -> Option<Reply> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        listener.set_nonblocking(true).expect("non-blocking");
        let daemon = Arc::new(Self {
            listener,
            port,
            commands: Mutex::new(Vec::new()),
            writer: Arc::new(Mutex::new(None)),
            stop: AtomicBool::new(false),
            answer: Box::new(answer),
        });
        let serving = Arc::clone(&daemon);
        std::thread::Builder::new()
            .name("fake-agentd-rc".to_owned())
            .spawn(move || serving.serve())
            .expect("spawn fake agentd");
        daemon
    }

    fn serve(self: Arc<Self>) {
        while !self.stop.load(Ordering::Relaxed) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nodelay(true).expect("nodelay");
                    let _ = self.converse(stream);
                    *lock(&self.writer) = None;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::yield_now();
                }
                Err(_) => return,
            }
        }
    }

    fn converse(&self, stream: TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_millis(25)))?;
        *lock(&self.writer) = Some(stream.try_clone()?);
        let mut reader = BufReader::new(stream);
        let mut pending = String::new();
        let Some(hello) = self.read_frame(&mut reader, &mut pending)? else {
            return Ok(());
        };
        if hello.get("t").and_then(Value::as_str) != Some("hello")
            || hello.get("token").and_then(Value::as_str) != Some(TOKEN)
        {
            self.write_frame(&json!({ "t": "refused", "reason": "bad_token" }))?;
            return Ok(());
        }
        self.write_frame(&json!({
            "t": "welcome",
            "v": 1,
            "node": "fake-agentd",
            "streams": {},
            "floors": {},
        }))?;
        loop {
            let Some(frame) = self.read_frame(&mut reader, &mut pending)? else {
                return Ok(());
            };
            match frame.get("t").and_then(Value::as_str).unwrap_or_default() {
                "cmd" => {
                    lock(&self.commands).push(frame.clone());
                    let id = frame
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let op = frame.get("op").and_then(Value::as_str).unwrap_or_default();
                    let args = frame.get("args").cloned().unwrap_or(Value::Null);
                    let validation_error = if op == "mhc.session.open" {
                        crate::validate_open_args(&args).err()
                    } else {
                        None
                    };
                    let reply = match validation_error {
                        Some(error) => Reply::Error(error),
                        None => (self.answer)(op, &args)
                            .unwrap_or_else(|| Reply::Error(format!("unsupported:{op}"))),
                    };
                    self.dispatch_reply(id, reply)?;
                }
                "ping" => {
                    let n = frame.get("n").cloned().unwrap_or(json!(0));
                    let at = frame.get("at").cloned().unwrap_or(json!(0));
                    self.write_frame(&json!({ "t": "pong", "n": n, "at": at }))?;
                }
                _ => {}
            }
        }
    }

    fn dispatch_reply(&self, id: String, reply: Reply) -> std::io::Result<()> {
        match reply {
            Reply::Data(data) => {
                self.write_frame(&json!({ "t": "res", "id": id, "ok": true, "data": data }))
            }
            Reply::Error(error) => {
                self.write_frame(&json!({ "t": "res", "id": id, "ok": false, "error": error }))
            }
            Reply::Silence => Ok(()),
            Reply::After(gate, inner) => {
                let writer = Arc::clone(&self.writer);
                std::thread::spawn(move || {
                    gate.wait(Duration::from_secs(5));
                    let frame = match *inner {
                        Reply::Data(data) => {
                            Some(json!({ "t": "res", "id": id, "ok": true, "data": data }))
                        }
                        Reply::Error(error) => {
                            Some(json!({ "t": "res", "id": id, "ok": false, "error": error }))
                        }
                        _ => None,
                    };
                    if let Some(frame) = frame
                        && let Some(stream) = lock(&writer).as_mut()
                    {
                        let mut line = serde_json::to_string(&frame).unwrap_or_default();
                        line.push('\n');
                        let _ = stream.write_all(line.as_bytes());
                        let _ = stream.flush();
                    }
                });
                Ok(())
            }
        }
    }

    fn write_frame(&self, frame: &Value) -> std::io::Result<()> {
        let mut guard = lock(&self.writer);
        let Some(writer) = guard.as_mut() else {
            return Ok(());
        };
        let mut line = serde_json::to_string(frame).unwrap_or_default();
        line.push('\n');
        writer.write_all(line.as_bytes())?;
        writer.flush()
    }

    fn read_frame(
        &self,
        reader: &mut BufReader<TcpStream>,
        pending: &mut String,
    ) -> std::io::Result<Option<Value>> {
        loop {
            if let Some(index) = pending.find('\n') {
                let line: String = pending.drain(..=index).collect();
                let text = line.trim();
                if text.is_empty() {
                    continue;
                }
                return Ok(serde_json::from_str(text).ok());
            }
            match reader.read_line(pending) {
                Ok(0) => return Ok(None),
                Ok(_) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    if self.stop.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn emit(&self, frame: &Value) {
        let _ = self.write_frame(frame);
    }

    fn commands(&self) -> Vec<Value> {
        lock(&self.commands).clone()
    }
}

impl Drop for FakeAgentd {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn make_peer(device: &str, daemon: &FakeAgentd, dir: &Path) -> AgentdPeer {
    let token_file = dir.join(format!("token-{device}"));
    std::fs::write(&token_file, TOKEN).expect("write token");
    AgentdPeer {
        device: device.to_owned(),
        address: "127.0.0.1".to_owned(),
        port: daemon.port,
        token_file,
    }
}

#[tokio::test]
async fn lookup_gate_over_agentd_caps_slow_locates_serializes_solo_retries_and_fences_admitted_locates()
 {
    let dir = tempfile::tempdir().expect("tempdir");
    let hold_b = GateSignal::new();
    let hold_b_ans = hold_b.clone();
    let started: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let started_b = Arc::clone(&started);
    let started_c = Arc::clone(&started);

    let daemon_b = FakeAgentd::start(move |op, _args| {
        if op != "mhc.session.locate" {
            return None;
        }
        lock(&started_b).push("b".to_owned());
        Some(Reply::After(
            hold_b_ans.clone(),
            Box::new(Reply::Data(
                json!({ "found": true, "row": { "session_id": "rk-b" } }),
            )),
        ))
    });
    let daemon_c = FakeAgentd::start(move |op, _args| {
        if op != "mhc.session.locate" {
            return None;
        }
        lock(&started_c).push("c".to_owned());
        Some(Reply::Data(
            json!({ "found": true, "row": { "session_id": "rk-c" } }),
        ))
    });

    let client = Arc::new(
        AgentdClient::with_peers(
            "test-node",
            vec![
                make_peer("b", &daemon_b, dir.path()),
                make_peer("c", &daemon_c, dir.path()),
            ],
        )
        .with_lookup_cap(Duration::from_millis(40)),
    );
    let ports = AgentdPorts::new(Arc::clone(&client));

    // A slow solo lookup on b answers Silent at lookup_cap while its locate stays in flight and keeps the fence closed.
    let solo_b = ports
        .lookup_alone(
            "b".to_owned(),
            "0195f0c2-1a2b-7c3d-8e4f-00000000c001".to_owned(),
            5_000,
        )
        .await;
    assert_eq!(
        solo_b,
        Some(RemoteLookupOutcome::Silent {
            reason: "lookup_timeout".to_owned()
        })
    );

    // While b's solo locate never settles, an admitted post-create locate on c times out at the fence and sends nothing.
    let post_create = ports.locate("c".to_owned(), "engine-1".to_owned()).await;
    assert_eq!(
        post_create,
        RemoteLocateOutcome::Refused {
            reason: "locate_failed".to_owned()
        }
    );
    assert_eq!(*lock(&started), vec!["b".to_owned()]);

    // And a session_command to a durable UUID on c also refuses at the closed fence without dispatching to c.
    let attach_cmd = ports
        .session_command(
            "c".to_owned(),
            "0195f0c2-1a2b-7c3d-8e4f-00000000c001".to_owned(),
            json!({ "type": "get_state", "sessionId": "0195f0c2-1a2b-7c3d-8e4f-00000000c001" }),
            Default::default(),
        )
        .await;
    assert_eq!(
        attach_cmd,
        RemoteCommandOutcome::Refused {
            reason: "lookup_fence_timeout".to_owned(),
            detail: None,
        }
    );
    assert_eq!(*lock(&started), vec!["b".to_owned()]);

    // Once b's in-flight locate settles, the fence opens and c's locate dispatches and succeeds.
    hold_b.release();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let located_c = ports.locate("c".to_owned(), "engine-1".to_owned()).await;
    assert_eq!(
        located_c,
        RemoteLocateOutcome::Located {
            session_id: "rk-c".to_owned()
        }
    );
    assert_eq!(*lock(&started), vec!["b".to_owned(), "c".to_owned()]);
}

#[tokio::test]
async fn agentd_watch_and_follow_remote_stream_over_fake_agentd_and_retry_fence_refusal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let hold_solo = GateSignal::new();
    let hold_solo_ans = hold_solo.clone();
    let streaming = Arc::new(AtomicBool::new(true));
    let streaming_ans = Arc::clone(&streaming);

    let daemon_b = FakeAgentd::start(move |op, args| match op {
        "mhc.session.follow" => {
            let sid = args
                .get("session")
                .and_then(Value::as_str)
                .unwrap_or("rk-1");
            Some(Reply::Data(json!({
                "stream": format!("mhc-session:{sid}"),
                "seq": 0,
            })))
        }
        "mhc.session.command" => {
            let is_streaming = streaming_ans.load(Ordering::Relaxed);
            Some(Reply::Data(json!({
                "state": "engine_applied",
                "receipt": {
                    "isStreaming": is_streaming,
                    "currentTurnId": if is_streaming { Value::String("7".into()) } else { Value::Null },
                    "durableSessionId": "dur-1",
                }
            })))
        }
        "mhc.session.locate" => Some(Reply::Data(json!({
            "found": true,
            "row": { "session_id": "rk-1-after" },
        }))),
        "mhc.session.read" => Some(Reply::Data(json!({
            "messages": [
                {
                    "type": "message",
                    "id": "u1",
                    "message": {
                        "role": "user",
                        "content": [{ "type": "text", "text": "ping" }],
                        "clientMessageId": "cm-1",
                    }
                },
                {
                    "type": "message",
                    "id": "a1",
                    "message": {
                        "role": "assistant",
                        "content": [{ "type": "text", "text": "pong\n" }],
                    }
                }
            ],
            "state": "idle",
        }))),
        _ => None,
    });

    let daemon_c = FakeAgentd::start(move |op, _args| {
        if op != "mhc.session.locate" {
            return None;
        }
        Some(Reply::After(
            hold_solo_ans.clone(),
            Box::new(Reply::Data(json!({ "found": false }))),
        ))
    });

    let delays: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let delays_cb = Arc::clone(&delays);
    let hold_solo_release = hold_solo.clone();
    let client = Arc::new(
        AgentdClient::with_peers(
            "test-node",
            vec![
                make_peer("b", &daemon_b, dir.path()),
                make_peer("c", &daemon_c, dir.path()),
            ],
        )
        .with_lookup_cap(Duration::from_millis(30))
        .with_retry_delay(Arc::new(move |ms| {
            let d = Arc::clone(&delays_cb);
            let rel = hold_solo_release.clone();
            Box::pin(async move {
                lock(&d).push(ms);
                rel.release();
                tokio::time::sleep(Duration::from_millis(15)).await;
            })
        })),
    );
    let ports = Arc::new(AgentdPorts::new(Arc::clone(&client)));

    // 1. Watch over FakeAgentd: solo lookup on c holds fence, session_replaced on b hits lookup_fence_timeout,
    // backs off, then relocates to rk-1-after and resyncs once solo settles (#14748 review round 7).
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let notify = Arc::new(Notify::new());
    let log_lost = Arc::clone(&log);
    let notify_lost = Arc::clone(&notify);
    let log_res = Arc::clone(&log);
    let notify_res = Arc::clone(&notify);
    let handlers = RemoteWatchHandlers {
        record: Arc::new(|_| Ok(())),
        status: None,
        resynced: Some(Arc::new(move |resync| {
            lock(&log_res).push(format!(
                "resync {} -> {} settled=[{}] restarted={}",
                resync.from.as_deref().unwrap_or("-"),
                resync.state.status,
                resync.settled_turns.join(","),
                resync.restarted
            ));
            notify_res.notify_waiters();
            Ok(())
        })),
        lost: Some(Arc::new(move |reason| {
            lock(&log_lost).push(format!("lost {reason}"));
            notify_lost.notify_waiters();
            Ok(())
        })),
    };
    let RemoteWatchOutcome::Ok { watch } = ports
        .watch(
            "b".to_owned(),
            "rk-1".to_owned(),
            handlers,
            RemoteWatchOptions { baseline: true },
        )
        .await
    else {
        panic!("watch should succeed");
    };

    // Start a capped solo lookup on c that holds the port's lookup fence closed.
    assert_eq!(
        ports
            .lookup_alone("c".to_owned(), "dur-solo".to_owned(), 5_000)
            .await,
        Some(RemoteLookupOutcome::Silent {
            reason: "lookup_timeout".into()
        })
    );

    streaming.store(false, Ordering::Relaxed);
    daemon_b.emit(&json!({
        "t": "evt",
        "s": "mhc-session:rk-1",
        "seq": 1,
        "e": { "kind": "ended", "reason": "session_replaced" },
    }));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while lock(&log).len() < 2 {
        tokio::time::timeout_at(deadline, notify.notified())
            .await
            .expect("watch events");
    }
    watch.stop();
    assert_eq!(
        *lock(&log),
        vec![
            "lost lookup_fence_timeout".to_owned(),
            "resync running -> idle settled=[7] restarted=true".to_owned(),
        ]
    );
    assert_eq!(*lock(&delays), vec![1_000]);

    // 2. follow_remote over FakeAgentd (25dc4d8220): buffered deltas after stamped prompt are retained.
    let surface = Arc::new(RemoteThreadSurface::new(ports.clone(), ports.clone(), None));
    let printed: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let printed_cb = Arc::clone(&printed);
    let follow = follow_remote(
        surface,
        RemoteSessionRow {
            device: "b".to_owned(),
            session_id: "rk-1".to_owned(),
            state: "idle".to_owned(),
            ..RemoteSessionRow::default()
        },
        Arc::new(move |text| lock(&printed_cb).push_str(text)),
        FollowRemoteOptions {
            text: Some("ping".to_owned()),
            client_message_id: Some("cm-1".to_owned()),
        },
    )
    .await
    .expect("follow_remote over FakeAgentd");

    // Emit agent_start, stamped prompt, and text_delta BEFORE aim (buffered).
    for (seq, event) in [
        (2u64, json!({ "type": "agent_start" })),
        (
            3u64,
            json!({
                "type": "message_end",
                "message": {
                    "role": "user",
                    "content": [{ "type": "text", "text": "ping" }],
                    "clientMessageId": "cm-1",
                }
            }),
        ),
        (
            4u64,
            json!({
                "type": "message_update",
                "assistantMessageEvent": { "type": "text_delta", "delta": "pong\n", "contentIndex": 0 },
            }),
        ),
    ] {
        daemon_b.emit(&json!({
            "t": "evt",
            "s": "mhc-session:rk-1",
            "seq": seq,
            "e": event,
        }));
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
    follow.aim(RemoteFollowLane::Started);
    tokio::time::sleep(Duration::from_millis(25)).await;
    daemon_b.emit(&json!({
        "t": "evt",
        "s": "mhc-session:rk-1",
        "seq": 5,
        "e": { "type": "agent_settled" },
    }));
    let settled = tokio::time::timeout(Duration::from_secs(5), follow.settled)
        .await
        .expect("follow settled");
    assert_eq!(settled, "settled");
    assert_eq!(*lock(&printed), "pong\n");
}

struct StubLocalHost;

impl SessionCommandPort for StubLocalHost {
    fn send(&self, _command: Value) -> BoxFuture<Result<Value, String>> {
        Box::pin(async { Ok(json!({ "type": "response", "success": true })) })
    }
}

impl ThreadHost for StubLocalHost {
    fn list_view(&self) -> BoxFuture<Result<ThreadHostView, String>> {
        let session = ThreadHostSession {
            session_id: "rpc-caller".to_owned(),
            durable_session_id: Some(CALLER.to_owned()),
            session_path: None,
            cwd: "/work".to_owned(),
            name: Some("caller".to_owned()),
            status: Some(HostSessionStatus::Open),
            kind: None,
            archived: false,
            activity: None,
            error_code: None,
            turn_id: None,
            model: None,
            waiting: None,
            engine_version: None,
            created_at: Some("2026-11-01T00:00:00.000Z".to_owned()),
            updated_at: Some("2026-11-01T00:00:00.000Z".to_owned()),
            endpoint_kind: GatewayEndpointKind::RpcHost,
            controls: Vec::new(),
            socket: "/run/local.sock".to_owned(),
        };
        let host = AddressBookHost {
            socket: "/run/local.sock".to_owned(),
            list_sessions: Some(HostListSessions::ok(vec![session.host_session()])),
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive: Some(true),
            reason: None,
            legacy: false,
        };
        Box::pin(async move {
            Ok(ThreadHostView {
                sessions: vec![session],
                hosts: vec![host],
                disk: Vec::new(),
            })
        })
    }

    fn session_port(&self, _session: &ThreadHostSession) -> Arc<dyn SessionCommandPort> {
        Arc::new(StubLocalHost)
    }

    fn open_host_session(
        &self,
        _params: Value,
        _name: Option<String>,
    ) -> BoxFuture<Result<ThreadHostSession, String>> {
        Box::pin(async { Err("local open not used in remote tests".to_owned()) })
    }
}

fn build_component(dir: &Path, client: Arc<AgentdClient>) -> GatewayThreadComponent {
    let ports = Arc::new(AgentdPorts::new(Arc::clone(&client)));
    let surface = Arc::new(RemoteThreadSurface::new(ports.clone(), ports, None));
    let locator = Arc::new(RelayControlLocator::new(surface).with_agentd(client));
    let store = create_gateway_store(GatewayStoreOptions::new(dir.to_string_lossy().into_owned()))
        .expect("gateway store");
    create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
        store,
        runtime: SessionRuntimePort {
            phase: Arc::new(|| RuntimePhase::Idle),
            admission_gate: Arc::new(|| SessionAdmissionGate {
                can_admit: true,
                hold_reason: None,
                editor_revision: 1,
                turn_epoch: 1,
            }),
            admit_external_message: Arc::new(|_| Err("unused".to_owned())),
            list_admitted_deliveries: Arc::new(AdmissionLedger::default),
        },
        durable_id: CALLER.to_owned(),
        session_path: Arc::new(|| None),
        caller_session_id: Arc::new(|| CALLER.to_owned()),
        caller_name: None,
        caller_provenance: Arc::new(|| Provenance {
            workspace: "unclassified".to_owned(),
            project: "test".to_owned(),
            lane: None,
            session: None,
        }),
        resolve: Arc::new(|_, _| {
            Box::pin(async {
                GatewayResolution::Error(thread_tool_failure("not_found", "unused", "none", None))
            })
        }),
        endpoints: GatewayEndpointPort::inert(),
        notify: None,
        log: None,
        host: Some(Arc::new(StubLocalHost)),
        caller_workspace_root: Arc::new(|| "/work".to_owned()),
        sessions_directory: None,
        remote: Some(locator),
        caller_reply_targets: Arc::new(Vec::new),
        caller_reply_senders: Arc::new(Vec::new),
    }))
}

async fn run_tool(component: &GatewayThreadComponent, name: &str, args: Value) -> Value {
    let call_id = format!("call-{}", super::NEXT_CALL.fetch_add(1, Ordering::SeqCst));
    component
        .execute(name, &call_id, args)
        .expect("tool exists")
        .await
}

fn init_git_repo(dir: &Path) -> String {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    let canonical = std::fs::canonicalize(&repo).unwrap_or(repo);
    let repo_str = canonical.to_string_lossy().into_owned();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(&canonical)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(canonical.join("README.md"), "x\n").expect("write readme");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "init"]);
    repo_str
}

fn git_branch_list(repo: &str, branch: &str) -> String {
    let out = Command::new("git")
        .args(["branch", "--list", branch])
        .current_dir(repo)
        .output()
        .expect("git branch --list");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn open_commands(daemon: &FakeAgentd) -> Vec<Value> {
    daemon
        .commands()
        .into_iter()
        .filter(|c| c.get("op").and_then(Value::as_str) == Some("mhc.session.open"))
        .collect()
}

fn create_opens(daemon: &FakeAgentd) -> Vec<Value> {
    open_commands(daemon)
}

fn rpc_commands(daemon: &FakeAgentd, rpc_type: &str) -> Vec<Value> {
    daemon
        .commands()
        .into_iter()
        .filter(|c| {
            c.get("op").and_then(Value::as_str) == Some("mhc.session.command")
                && c["args"]["rpc"].get("type").and_then(Value::as_str) == Some(rpc_type)
        })
        .collect()
}

// ============================================================================
// Port of upstream thread-create-remote-unreachable.test.ts (#14394, #14397, #14623 review M5)
// ============================================================================

#[tokio::test]
async fn thread_create_on_unrouted_device_unloads_session_sends_no_prompt_and_replays_refusal() {
    for args in [
        json!({ "device": "studio", "name": "remote-lane", "prompt": "go", "idempotency_key": "unrouted-1" }),
        json!({ "device": "studio", "prompt": "go", "idempotency_key": "unrouted-2" }),
        json!({ "device": "studio", "alias": "remote-alias-only", "idempotency_key": "unrouted-3" }),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let daemon = FakeAgentd::start(|op, cmd_args| match op {
            "mhc.sessions.list" => Some(Reply::Data(json!({ "sessions": [] }))),
            "mhc.session.open" => Some(Reply::Data(json!({
                "session_id": "rk-new",
                "routing_id": "rpc-9",
                "durable_id": "dur-new-on-b",
                "started": true,
            }))),
            "mhc.session.locate" => Some(Reply::Data(json!({
                "found": true,
                "row": { "session_id": "rk-new" },
            }))),
            "mhc.session.command" => {
                let rpc_type = cmd_args["rpc"]
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if rpc_type == "unload_session" {
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": { "outcome": "unloaded" },
                    })))
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "route_unlisted",
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio"),
        );
        let component = build_component(dir.path(), client);

        let first = run_tool(&component, "thread_create", args.clone()).await;
        assert_eq!(first["kind"], "error", "{first}");
        assert_eq!(first["error"]["code"], "host_unavailable");
        assert_eq!(first["error"]["details"]["device"], "b");
        assert_eq!(first["error"]["details"]["reason"], "route_unlisted");
        assert_eq!(first["error"]["details"]["created"], false);

        let unloads = rpc_commands(&daemon, "unload_session");
        assert_eq!(unloads.len(), 1);
        assert_eq!(unloads[0]["args"]["rpc"]["sessionId"], "rpc-9");
        assert!(rpc_commands(&daemon, "prompt").is_empty());

        // Same-key retry replays the refusal with still one open.
        let second = run_tool(&component, "thread_create", args).await;
        assert_eq!(second, first);
        assert_eq!(open_commands(&daemon).len(), 1);
    }
}

#[tokio::test]
async fn thread_create_on_unrouted_device_with_no_routing_id_reports_no_routing_id_and_route_lag_succeeds()
 {
    // 1. Open reply names no routing id (#14397 review L2)
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let daemon = FakeAgentd::start(|op, _args| match op {
            "mhc.sessions.list" => Some(Reply::Data(json!({ "sessions": [] }))),
            "mhc.session.open" => Some(Reply::Data(json!({
                "state": { "durableSessionId": "dur-new-on-b" }
            }))),
            "mhc.session.locate" => Some(Reply::Data(json!({
                "found": true,
                "row": { "session_id": "rk-new" },
            }))),
            "mhc.session.command" => Some(Reply::Data(json!({
                "state": "engine_refused",
                "reason": "route_unlisted",
            }))),
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio"),
        );
        let component = build_component(dir.path(), client);
        let result = run_tool(
            &component,
            "thread_create",
            json!({ "device": "studio", "name": "remote-lane" }),
        )
        .await;
        assert_eq!(result["kind"], "error", "{result}");
        assert_eq!(result["error"]["code"], "host_unavailable");
        assert_eq!(result["error"]["details"]["created"], true);
        assert_eq!(result["error"]["details"]["reason"], "route_unlisted");
        assert_eq!(result["error"]["details"]["unload"], "no_routing_id");
        assert!(rpc_commands(&daemon, "unload_session").is_empty());
    }

    // 2. Route lag (#14397 review L3): b refuses the new session once with route_unlisted, then lists/routes it.
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let step_calls = Arc::new(AtomicUsize::new(0));
        let step_calls_ans = Arc::clone(&step_calls);
        let daemon = FakeAgentd::start(move |op, _args| match op {
            "mhc.sessions.list" => Some(Reply::Data(json!({ "sessions": [] }))),
            "mhc.session.open" => Some(Reply::Data(json!({
                "session_id": "rk-new",
                "routing_id": "rpc-9",
                "durable_id": "dur-new-on-b",
                "started": true,
            }))),
            "mhc.session.locate" => Some(Reply::Data(json!({
                "found": true,
                "row": { "session_id": "rk-new" },
            }))),
            "mhc.session.command" => {
                if step_calls_ans.fetch_add(1, Ordering::Relaxed) == 0 {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "route_unlisted",
                    })))
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": { "isStreaming": false },
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio"),
        );
        let component = build_component(dir.path(), client);
        let result = run_tool(
            &component,
            "thread_create",
            json!({ "device": "studio", "name": "late-lane" }),
        )
        .await;
        assert_eq!(result["kind"], "ok", "{result}");
        assert_eq!(result["device"], "b");
        assert!(rpc_commands(&daemon, "unload_session").is_empty());
    }
}

#[tokio::test]
async fn thread_create_unrouted_with_worktree_removes_worktree_only_after_confirmed_unload_m5() {
    // 1. Confirmed unload -> removes far worktree and branch from b
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_git_repo(dir.path());
        let far_root = dir.path().join("far-wt");
        std::fs::create_dir_all(&far_root).expect("mkdir far-wt");
        let far_root_str = std::fs::canonicalize(&far_root)
            .unwrap_or(far_root)
            .to_string_lossy()
            .into_owned();
        let daemon = FakeAgentd::start(move |op, cmd_args| match op {
            "mhc.sessions.list" => Some(Reply::Data(json!({ "sessions": [] }))),
            "mhc.session.open" => Some(Reply::Data(json!({
                "session_id": "rk-new",
                "routing_id": "rpc-9",
                "durable_id": "dur-new-on-b",
                "started": true,
            }))),
            "mhc.session.locate" => Some(Reply::Data(json!({
                "found": true,
                "row": { "session_id": "rk-new" },
            }))),
            "mhc.session.command" => {
                let rpc = &cmd_args["rpc"];
                let rpc_type = rpc.get("type").and_then(Value::as_str).unwrap_or_default();
                if rpc_type == "extension_request"
                    && rpc.get("name").and_then(Value::as_str) == Some(THREAD_WORKTREE_RPC)
                {
                    let data = answer_worktree_request(&rpc["data"], &far_root_str, None);
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": data,
                    })))
                } else if rpc_type == "unload_session" {
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": { "outcome": "unloaded" },
                    })))
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "route_unlisted",
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio"),
        );
        let component = build_component(dir.path(), client);
        let result = run_tool(
            &component,
            "thread_create",
            json!({
                "device": "studio",
                "name": "remote-lane",
                "cwd": repo,
                "worktree": { "branch": "far-unrouted" },
            }),
        )
        .await;
        assert_eq!(result["kind"], "error", "{result}");
        assert_eq!(result["error"]["code"], "host_unavailable");
        assert_eq!(result["error"]["details"]["created"], false);
        assert!(result["error"]["details"].get("worktree").is_none());
        assert_eq!(git_branch_list(&repo, "far-unrouted"), "");
    }

    // 2. #14623 review M5: refused, timed-out or unknown unload keeps the worktree on b and names it
    for (idx, unload_reply) in [
        json!({ "state": "engine_applied", "receipt": { "outcome": "refused", "reason": "attached" } }),
        json!({ "state": "engine_refused", "reason": "request_timeout" }),
        json!({ "state": "uncertain", "reason": "reply_lost" }),
    ]
    .into_iter()
    .enumerate()
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_git_repo(dir.path());
        let far_root = dir.path().join("far-wt");
        std::fs::create_dir_all(&far_root).expect("mkdir far-wt");
        let far_root_str = std::fs::canonicalize(&far_root)
            .unwrap_or(far_root)
            .to_string_lossy()
            .into_owned();
        let branch = format!("far-kept-{idx}");
        let daemon = FakeAgentd::start(move |op, cmd_args| match op {
            "mhc.sessions.list" => Some(Reply::Data(json!({ "sessions": [] }))),
            "mhc.session.open" => Some(Reply::Data(json!({
                "session_id": "rk-new",
                "routing_id": "rpc-9",
                "durable_id": "dur-new-on-b",
                "started": true,
            }))),
            "mhc.session.locate" => Some(Reply::Data(json!({
                "found": true,
                "row": { "session_id": "rk-new" },
            }))),
            "mhc.session.command" => {
                let rpc = &cmd_args["rpc"];
                let rpc_type = rpc.get("type").and_then(Value::as_str).unwrap_or_default();
                if rpc_type == "extension_request"
                    && rpc.get("name").and_then(Value::as_str) == Some(THREAD_WORKTREE_RPC)
                {
                    let data = answer_worktree_request(&rpc["data"], &far_root_str, None);
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": data,
                    })))
                } else if rpc_type == "unload_session" {
                    if rpc.get("effectId").is_some() {
                        // Setup session close_ladder unload
                        Some(Reply::Data(json!({
                            "state": "engine_applied",
                            "receipt": { "outcome": "unloaded" },
                        })))
                    } else {
                        Some(Reply::Data(unload_reply.clone()))
                    }
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "route_unlisted",
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio"),
        );
        let component = build_component(dir.path(), client);
        let result = run_tool(
            &component,
            "thread_create",
            json!({
                "device": "studio",
                "name": "remote-lane",
                "cwd": repo,
                "worktree": { "branch": branch },
            }),
        )
        .await;
        assert_eq!(result["kind"], "error", "{result}");
        assert_eq!(result["error"]["code"], "host_unavailable");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("worktree")
        );
        assert_eq!(result["error"]["details"]["created"], true);
        assert_eq!(result["error"]["details"]["worktree"]["branch"], branch);
        assert!(!git_branch_list(&repo, &branch).is_empty());
    }

    // 3. Confirmed unload, but b fails to remove the worktree -> names worktree and cleanup failure
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_git_repo(dir.path());
        let far_root = dir.path().join("far-wt");
        std::fs::create_dir_all(&far_root).expect("mkdir far-wt");
        let far_root_str = std::fs::canonicalize(&far_root)
            .unwrap_or(far_root)
            .to_string_lossy()
            .into_owned();
        let daemon = FakeAgentd::start(move |op, cmd_args| match op {
            "mhc.sessions.list" => Some(Reply::Data(json!({ "sessions": [] }))),
            "mhc.session.open" => Some(Reply::Data(json!({
                "session_id": "rk-new",
                "routing_id": "rpc-9",
                "durable_id": "dur-new-on-b",
                "started": true,
            }))),
            "mhc.session.locate" => Some(Reply::Data(json!({
                "found": true,
                "row": { "session_id": "rk-new" },
            }))),
            "mhc.session.command" => {
                let rpc = &cmd_args["rpc"];
                let rpc_type = rpc.get("type").and_then(Value::as_str).unwrap_or_default();
                if rpc_type == "extension_request"
                    && rpc.get("name").and_then(Value::as_str) == Some(THREAD_WORKTREE_RPC)
                {
                    if rpc["data"].get("remove").is_some() {
                        return Some(Reply::Data(json!({
                            "state": "engine_refused",
                            "reason": "request_timeout",
                        })));
                    }
                    let data = answer_worktree_request(&rpc["data"], &far_root_str, None);
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": data,
                    })))
                } else if rpc_type == "unload_session" {
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": { "outcome": "unloaded" },
                    })))
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "route_unlisted",
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio"),
        );
        let component = build_component(dir.path(), client);
        let result = run_tool(
            &component,
            "thread_create",
            json!({
                "device": "studio",
                "name": "remote-lane",
                "cwd": repo,
                "worktree": { "branch": "far-left" },
            }),
        )
        .await;
        assert_eq!(result["kind"], "error", "{result}");
        assert_eq!(result["error"]["details"]["created"], false);
        assert_eq!(result["error"]["details"]["worktree"]["branch"], "far-left");
        assert_eq!(
            result["error"]["details"]["cleanup"]["details"]["reason"],
            "worktree_left_behind"
        );
    }
}

// ============================================================================
// Port of upstream thread-create-remote-lost-reply.test.ts (#14560, #14569, #14623 review H1 & M4)
// ============================================================================

#[tokio::test]
async fn thread_create_survives_lost_open_reply_on_the_spot_and_on_same_key_retry_and_replay() {
    // 1. Adopted on the spot when b lists the derived id immediately after the open reply is lost
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = created_thread_id(CALLER, "remote-lost-1");
        let id_for_open = id.clone();
        let opened_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let opened_id_ans = Arc::clone(&opened_id);
        let daemon = FakeAgentd::start(move |op, _cmd_args| match op {
            "mhc.sessions.list" => {
                let sessions = match lock(&opened_id_ans).clone() {
                    Some(id) => vec![json!({
                        "session_id": format!("rk-{id}"),
                        "durable_id": id,
                        "title": "",
                        "project": "/work/b",
                        "path": "/work/b",
                        "state": "idle",
                        "updated_ms": 1_000,
                    })],
                    None => Vec::new(),
                };
                Some(Reply::Data(json!({ "sessions": sessions })))
            }
            "mhc.session.open" => {
                *lock(&opened_id_ans) = Some(id_for_open.clone());
                Some(Reply::Silence)
            }
            "mhc.session.command" => Some(Reply::Data(json!({
                "state": "engine_applied",
                "receipt": { "isStreaming": false },
            }))),
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio")
                .with_call_timeout(Duration::from_millis(60)),
        );
        let component = build_component(dir.path(), client);
        let result = run_tool(
            &component,
            "thread_create",
            json!({ "device": "studio", "name": "lost-on-b", "idempotency_key": "remote-lost-1" }),
        )
        .await;
        assert_eq!(result["kind"], "ok", "{result}");
        assert_eq!(result["thread"], id);
        assert_eq!(result["device"], "b");
        assert_eq!(result["name"], "lost-on-b");
        assert_eq!(create_opens(&daemon).len(), 1);
        assert_eq!(rpc_commands(&daemon, "set_session_name").len(), 1);
    }

    // 2. First call fails listing too -> idempotency_uncertain; second call under same key adopts, prompts once;
    // third call under same key replays with deduplicated: true and sends nothing new.
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = created_thread_id(CALLER, "remote-lost-2");
        let id_for_open = id.clone();
        let opened_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let opened_id_ans = Arc::clone(&opened_id);
        let fail_next_list = Arc::new(AtomicBool::new(false));
        let fail_next_list_ans = Arc::clone(&fail_next_list);
        let daemon = FakeAgentd::start(move |op, _cmd_args| match op {
            "mhc.sessions.list" => {
                if fail_next_list_ans.swap(false, Ordering::Relaxed) {
                    return Some(Reply::Error("list_failed".to_owned()));
                }
                let sessions = match lock(&opened_id_ans).clone() {
                    Some(id) => vec![json!({
                        "session_id": format!("rk-{id}"),
                        "durable_id": id,
                        "title": "",
                        "project": "/work/b",
                        "path": "/work/b",
                        "state": "idle",
                        "updated_ms": 1_000,
                    })],
                    None => Vec::new(),
                };
                Some(Reply::Data(json!({ "sessions": sessions })))
            }
            "mhc.session.open" => {
                *lock(&opened_id_ans) = Some(id_for_open.clone());
                fail_next_list_ans.store(true, Ordering::Relaxed);
                Some(Reply::Silence)
            }
            "mhc.session.command" => Some(Reply::Data(json!({
                "state": "engine_applied",
                "receipt": { "isStreaming": false, "disposition": "started", "turnId": "1" },
            }))),
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio")
                .with_call_timeout(Duration::from_millis(60)),
        );
        let component = build_component(dir.path(), client);
        let args = json!({
            "device": "studio",
            "name": "lost-twice",
            "prompt": "build it",
            "idempotency_key": "remote-lost-2",
        });

        let first = run_tool(&component, "thread_create", args.clone()).await;
        assert_eq!(first["kind"], "error", "{first}");
        assert_eq!(first["error"]["code"], "idempotency_uncertain");

        let retry = run_tool(&component, "thread_create", args.clone()).await;
        assert_eq!(retry["kind"], "ok", "{retry}");
        assert_eq!(retry["thread"], id);
        assert_eq!(retry["device"], "b");
        assert_eq!(retry["name"], "lost-twice");
        assert_eq!(create_opens(&daemon).len(), 1);
        assert_eq!(rpc_commands(&daemon, "prompt").len(), 1);

        let commands_before = daemon.commands().len();
        let replay = run_tool(&component, "thread_create", args).await;
        let mut expected_replay = retry.clone();
        expected_replay["deduplicated"] = json!(true);
        assert_eq!(replay, expected_replay);
        assert_eq!(daemon.commands().len(), commands_before);
    }
}

#[tokio::test]
async fn thread_create_lost_reply_m4_refuses_cached_or_unreachable_and_round2_m4_requires_observed_route()
 {
    // #14623 review M4: cached snapshot or unreachable session stays idempotency_uncertain
    for (cached, unreachable) in [(true, false), (false, true), (true, true)] {
        let dir = tempfile::tempdir().expect("tempdir");
        let idempotency_key = format!("remote-lost-m4-{cached}-{unreachable}");
        let id_for_open = created_thread_id(CALLER, &idempotency_key);
        let opened_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let opened_id_ans = Arc::clone(&opened_id);
        let fail_next_list = Arc::new(AtomicBool::new(false));
        let fail_next_list_ans = Arc::clone(&fail_next_list);
        let daemon = FakeAgentd::start(move |op, _cmd_args| match op {
            "mhc.sessions.list" => {
                if fail_next_list_ans.swap(false, Ordering::Relaxed) {
                    return Some(Reply::Error("list_failed".to_owned()));
                }
                let sessions = match lock(&opened_id_ans).clone() {
                    Some(id) => vec![json!({
                        "session_id": format!("rk-{id}"),
                        "durable_id": id,
                        "title": "",
                        "project": "/work/b",
                        "path": "/work/b",
                        "state": "idle",
                        "updated_ms": 1_000,
                        "source": if cached { "cache" } else { "live" },
                        "as_of": 0,
                    })],
                    None => Vec::new(),
                };
                Some(Reply::Data(json!({ "sessions": sessions })))
            }
            "mhc.session.open" => {
                *lock(&opened_id_ans) = Some(id_for_open.clone());
                fail_next_list_ans.store(true, Ordering::Relaxed);
                Some(Reply::Silence)
            }
            "mhc.session.command" => {
                if unreachable {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "peer_not_found",
                    })))
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": { "isStreaming": false },
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio")
                .with_call_timeout(Duration::from_millis(60)),
        );
        let component = build_component(dir.path(), client);
        let args = json!({
            "device": "studio",
            "name": "lost-m4",
            "idempotency_key": idempotency_key,
        });
        let first = run_tool(&component, "thread_create", args.clone()).await;
        assert_eq!(first["error"]["code"], "idempotency_uncertain", "{first}");
        let second = run_tool(&component, "thread_create", args).await;
        assert_eq!(second["error"]["code"], "idempotency_uncertain", "{second}");
        assert_eq!(create_opens(&daemon).len(), 1);
    }

    // #14623 review round 2 M4: first sees route_unlisted, then request_timeout -> stays idempotency_uncertain
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let id_for_open = created_thread_id(CALLER, "remote-lost-mixed-2");
        let opened_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let opened_id_ans = Arc::clone(&opened_id);
        let fail_next_list = Arc::new(AtomicBool::new(false));
        let fail_next_list_ans = Arc::clone(&fail_next_list);
        let cmd_count = Arc::new(AtomicUsize::new(0));
        let cmd_count_ans = Arc::clone(&cmd_count);
        let daemon = FakeAgentd::start(move |op, _cmd_args| match op {
            "mhc.sessions.list" => {
                if fail_next_list_ans.swap(false, Ordering::Relaxed) {
                    return Some(Reply::Error("list_failed".to_owned()));
                }
                let sessions = match lock(&opened_id_ans).clone() {
                    Some(id) => vec![json!({
                        "session_id": format!("rk-{id}"),
                        "durable_id": id,
                        "title": "",
                        "project": "/work/b",
                        "path": "/work/b",
                        "state": "idle",
                        "updated_ms": 1_000,
                    })],
                    None => Vec::new(),
                };
                Some(Reply::Data(json!({ "sessions": sessions })))
            }
            "mhc.session.open" => {
                *lock(&opened_id_ans) = Some(id_for_open.clone());
                fail_next_list_ans.store(true, Ordering::Relaxed);
                Some(Reply::Silence)
            }
            "mhc.session.locate" => {
                let id = lock(&opened_id_ans).clone().unwrap_or_default();
                Some(Reply::Data(json!({
                    "found": true,
                    "row": { "session_id": format!("rk-{id}") },
                })))
            }
            "mhc.session.command" => {
                let n = cmd_count_ans.fetch_add(1, Ordering::Relaxed) + 1;
                if n <= 2 {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "route_unlisted",
                    })))
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "request_timeout",
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio")
                .with_call_timeout(Duration::from_millis(60)),
        );
        let component = build_component(dir.path(), client);
        let args = json!({
            "device": "studio",
            "name": "lost-mixed",
            "prompt": "build it",
            "idempotency_key": "remote-lost-mixed-2",
        });
        for _ in 0..3 {
            let res = run_tool(&component, "thread_create", args.clone()).await;
            assert_eq!(res["error"]["code"], "idempotency_uncertain", "{res}");
        }
        assert_eq!(create_opens(&daemon).len(), 1);
    }
}

#[tokio::test]
async fn thread_create_same_key_recovery_14569_keeps_worktree_first_cause_and_unloads_adopted_unrouted_session()
 {
    // 1. Same-key recovery keeps the far worktree the first run made (#14569)
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_git_repo(dir.path());
        let repo_for_open = repo.clone();
        let expected_id = created_thread_id(CALLER, "remote-lost-wt");
        let expected_id_open = expected_id.clone();
        let far_root = dir.path().join("far-wt");
        std::fs::create_dir_all(&far_root).expect("mkdir far-wt");
        let far_root_str = std::fs::canonicalize(&far_root)
            .unwrap_or(far_root)
            .to_string_lossy()
            .into_owned();
        let opened_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let opened_id_ans = Arc::clone(&opened_id);
        let fail_next_list = Arc::new(AtomicBool::new(false));
        let fail_next_list_ans = Arc::clone(&fail_next_list);
        let daemon = FakeAgentd::start(move |op, cmd_args| match op {
            "mhc.sessions.list" => {
                if fail_next_list_ans.swap(false, Ordering::Relaxed) {
                    return Some(Reply::Error("list_failed".to_owned()));
                }
                let sessions = match lock(&opened_id_ans).clone() {
                    Some(id) => vec![json!({
                        "session_id": format!("rk-{id}"),
                        "durable_id": id,
                        "title": "",
                        "project": "/work/b",
                        "path": "/work/b",
                        "state": "idle",
                        "updated_ms": 1_000,
                    })],
                    None => Vec::new(),
                };
                Some(Reply::Data(json!({ "sessions": sessions })))
            }
            "mhc.session.open" => {
                if cmd_args["launch"].get("cwd").and_then(Value::as_str)
                    != Some(repo_for_open.as_str())
                {
                    *lock(&opened_id_ans) = Some(expected_id_open.clone());
                    fail_next_list_ans.store(true, Ordering::Relaxed);
                    return Some(Reply::Silence);
                }
                Some(Reply::Data(json!({
                    "session_id": "rk-wt-setup",
                    "durable_id": "dur-wt-setup",
                    "started": true,
                })))
            }
            "mhc.session.command" => {
                let rpc = &cmd_args["rpc"];
                let rpc_type = rpc.get("type").and_then(Value::as_str).unwrap_or_default();
                if rpc_type == "extension_request"
                    && rpc.get("name").and_then(Value::as_str) == Some(THREAD_WORKTREE_RPC)
                {
                    let data = answer_worktree_request(&rpc["data"], &far_root_str, None);
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": data,
                    })))
                } else if rpc_type == "unload_session" {
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": { "outcome": "unloaded" },
                    })))
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": { "isStreaming": false },
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio")
                .with_call_timeout(Duration::from_millis(80)),
        );
        let component = build_component(dir.path(), client);
        let args = json!({
            "device": "studio",
            "name": "lost-wt",
            "cwd": repo,
            "worktree": { "branch": "far-lost" },
            "idempotency_key": "remote-lost-wt",
        });

        let first = run_tool(&component, "thread_create", args.clone()).await;
        assert_eq!(first["kind"], "error", "{first}");
        assert_eq!(first["error"]["code"], "idempotency_uncertain");
        assert_eq!(first["error"]["details"]["worktree"]["branch"], "far-lost");

        let retry = run_tool(&component, "thread_create", args).await;
        assert_eq!(retry["kind"], "ok", "{retry}");
        assert_eq!(retry["thread"], expected_id);
        assert_eq!(retry["worktree"]["branch"], "far-lost");
        let thread_opens: Vec<Value> = create_opens(&daemon)
            .into_iter()
            .filter(|c| {
                c["args"]["launch"].get("cwd").and_then(Value::as_str) != Some(repo.as_str())
            })
            .collect();
        assert_eq!(thread_opens.len(), 1);
    }

    // 2. When b ignores the derived id and reply is lost, retries name first_cause (#14569)
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let daemon = FakeAgentd::start(|op, _args| match op {
            "mhc.sessions.list" => Some(Reply::Data(json!({ "sessions": [] }))),
            "mhc.session.open" => Some(Reply::Silence),
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio")
                .with_call_timeout(Duration::from_millis(60)),
        );
        let component = build_component(dir.path(), client);
        let args = json!({
            "device": "studio",
            "name": "first-cause",
            "idempotency_key": "remote-lost-cause",
        });
        let first = run_tool(&component, "thread_create", args.clone()).await;
        assert_eq!(first["error"]["code"], "idempotency_uncertain", "{first}");
        let first_msg = first["error"]["message"].clone();
        let first_details = first["error"]["details"].clone();
        for _ in 0..2 {
            let retry = run_tool(&component, "thread_create", args.clone()).await;
            assert_eq!(retry["error"]["code"], "idempotency_uncertain", "{retry}");
            assert_eq!(retry["error"]["details"]["error_note"], first_msg);
            assert_eq!(retry["error"]["details"]["first_cause"], first_details);
        }
        assert_eq!(create_opens(&daemon).len(), 1);
    }

    // 3. Adopted session that b's route never lists is unloaded by the routing id b's engine names, and receipt stays uncertain (#14569)
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = created_thread_id(CALLER, "remote-lost-unrouted");
        let id_for_open = id.clone();
        let opened_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let opened_id_ans = Arc::clone(&opened_id);
        let fail_next_list = Arc::new(AtomicBool::new(false));
        let fail_next_list_ans = Arc::clone(&fail_next_list);
        let unloaded = Arc::new(AtomicBool::new(false));
        let unloaded_ans = Arc::clone(&unloaded);
        let daemon = FakeAgentd::start(move |op, cmd_args| match op {
            "mhc.sessions.list" => {
                if fail_next_list_ans.swap(false, Ordering::Relaxed) {
                    return Some(Reply::Error("list_failed".to_owned()));
                }
                if unloaded_ans.load(Ordering::Relaxed) {
                    return Some(Reply::Data(json!({ "sessions": [] })));
                }
                let sessions = match lock(&opened_id_ans).clone() {
                    Some(id) => vec![json!({
                        "session_id": format!("rk-{id}"),
                        "routing_id": format!("rpc-lost-{id}"),
                        "durable_id": id,
                        "title": "",
                        "project": "/work/b",
                        "path": "/work/b",
                        "state": "idle",
                        "updated_ms": 1_000,
                    })],
                    None => Vec::new(),
                };
                Some(Reply::Data(json!({ "sessions": sessions })))
            }
            "mhc.session.open" => {
                *lock(&opened_id_ans) = Some(id_for_open.clone());
                fail_next_list_ans.store(true, Ordering::Relaxed);
                Some(Reply::Silence)
            }
            "mhc.session.locate" => {
                let id = lock(&opened_id_ans).clone().unwrap_or_default();
                Some(Reply::Data(json!({
                    "found": true,
                    "row": { "session_id": format!("rk-{id}") },
                })))
            }
            "mhc.session.command" => {
                let rpc_type = cmd_args["rpc"]
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if rpc_type == "unload_session" {
                    unloaded_ans.store(true, Ordering::Relaxed);
                    Some(Reply::Data(json!({
                        "state": "engine_applied",
                        "receipt": { "outcome": "unloaded" },
                    })))
                } else {
                    Some(Reply::Data(json!({
                        "state": "engine_refused",
                        "reason": "route_unlisted",
                    })))
                }
            }
            _ => None,
        });
        let client = Arc::new(
            AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
                .with_peer_label("b", "studio")
                .with_call_timeout(Duration::from_millis(60)),
        );
        let component = build_component(dir.path(), client);
        let args = json!({
            "device": "studio",
            "name": "lost-unrouted",
            "idempotency_key": "remote-lost-unrouted",
        });

        let first = run_tool(&component, "thread_create", args.clone()).await;
        assert_eq!(first["error"]["code"], "idempotency_uncertain", "{first}");

        let second = run_tool(&component, "thread_create", args.clone()).await;
        assert_eq!(second["error"]["code"], "host_unavailable", "{second}");
        assert_eq!(second["error"]["details"]["reason"], "route_unlisted");
        assert_eq!(second["error"]["details"]["created"], false);
        let unloads = rpc_commands(&daemon, "unload_session");
        assert_eq!(unloads.len(), 1);
        assert_eq!(
            unloads[0]["args"]["rpc"]["sessionId"],
            format!("rpc-lost-{id}")
        );

        // Receipt stays uncertain: third call under the same key runs again and answers idempotency_uncertain.
        let third = run_tool(&component, "thread_create", args).await;
        assert_eq!(third["error"]["code"], "idempotency_uncertain", "{third}");
        assert_eq!(create_opens(&daemon).len(), 1);
    }
}

#[tokio::test]
async fn thread_create_fork_from_through_agentd_sends_strict_contract_launch_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, cmd_args| match op {
        "mhc.sessions.list" => Some(Reply::Data(json!({
            "sessions": [{
                "session_id": "rk-src-1",
                "durable_id": "dur-src-1",
                "title": "source",
                "project": "app",
                "path": "/work/src",
                "state": "idle",
                "updated_ms": 1_000,
            }],
            "capabilities": ["engine", "open_session_fork"],
        }))),
        "mhc.session.locate" => {
            let asked = cmd_args
                .get("session")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let sid = if asked == "dur-fork-2" {
                "rk-fork-2"
            } else {
                "rk-src-1"
            };
            Some(Reply::Data(json!({
                "found": true,
                "row": { "session_id": sid },
            })))
        }
        "mhc.session.open" => Some(Reply::Data(json!({
            "session_id": "rk-fork-2",
            "durable_id": "dur-fork-2",
            "stream": "mhc-session:rk-fork-2",
            "started": true,
            "forked": true,
        }))),
        "mhc.session.command" => Some(Reply::Data(json!({
            "state": "engine_applied",
            "receipt": { "isStreaming": false },
        }))),
        _ => None,
    });
    let client = Arc::new(
        AgentdClient::with_peers("test-node", vec![make_peer("b", &daemon, dir.path())])
            .with_peer_label("b", "studio"),
    );
    let component = build_component(dir.path(), client);

    let created = run_tool(
        &component,
        "thread_create",
        json!({
            "fork_from": "dur-src-1",
            "name": "forked-child",
            "cwd": "/work/fork",
            "model": "openai/gpt-6",
            "reasoning": "high",
            "permission_preset": "ask",
            "skills": ["programming"],
        }),
    )
    .await;
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(created["device"], "b");

    let opens = open_commands(&daemon);
    assert_eq!(opens.len(), 1);
    let args = &opens[0]["args"];
    assert_eq!(args["fork_from"], json!("dur-src-1"));
    assert!(args.get("session").is_none());
    assert_eq!(args["launch"]["cwd"], json!("/work/fork"));
    assert_eq!(args["launch"]["model"], json!("openai/gpt-6"));
    assert_eq!(args["launch"]["thinking_level"], json!("high"));
    assert_eq!(args["launch"]["permission_preset"], json!("ask"));
    assert_eq!(args["launch"]["name"], json!("forked-child"));
    assert_eq!(
        args["launch"]["context"]["initial_skills"],
        json!("[\"programming\"]")
    );
    for forbidden in [
        "forkFrom",
        "sessionPath",
        "durableSessionId",
        "modelId",
        "thinkingLevel",
        "permissionPreset",
        "provider",
        "initialSkills",
        "session",
    ] {
        assert!(
            args["launch"].get(forbidden).is_none(),
            "unexpected launch key {forbidden}: {args}"
        );
    }
}
