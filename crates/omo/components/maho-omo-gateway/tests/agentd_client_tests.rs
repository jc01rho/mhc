use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use maho_omo_gateway::thread::control::port::{SessionHost, SessionOpenRequest};
use maho_omo_gateway::thread::remote::agentd::AGENTD_ENTRY_REQUIRED;
use maho_omo_gateway::thread::remote::agentd::{
    AgentdClient, AgentdDeliveryPort, AgentdError, AgentdFollowPort, AgentdPeer, AgentdPorts,
    AgentdSessionHost, DeliveryOutcome, DeliveryPushOutcome,
};
use maho_omo_gateway::thread::remote::outcome::relay_outcome_failure;
use maho_omo_gateway::thread::remote::ports::{
    RemoteCommandOutcome, RemoteLocateOutcome, RemoteLookupOutcome, RemoteSessionsPort,
};
use serde_json::{Value, json};

/// The token the fake daemon admits; every peer's token file carries it.
const TOKEN: &str = "shared-agentd-token";

/// How long a test waits for a frame the daemon should have read.
const WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
struct Received {
    kind: String,
    frame: Value,
}

/// What the daemon answers one `cmd` with.
enum Reply {
    Data(Value),
    Error(String),
    /// Ends the connection without answering, as a daemon losing its link does.
    Drop,
}

/// What the daemon answers one `cmd` with, given the op and its args; `None` is `unsupported:<op>`.
type AnswerFn = dyn Fn(&str, &Value) -> Option<Reply> + Send + Sync;

/// A fake agentd on loopback: one thread accepts one connection at a time and answers the ops of the contract from
/// `answer`, so a test drives the real client over a real socket and a real frame format.
struct FakeAgentd {
    listener: TcpListener,
    port: u16,
    received: Mutex<Vec<Received>>,
    arrived: Condvar,
    /// The live connection's write side, shared so a test can push an `evt` or end the connection.
    writer: Mutex<Option<TcpStream>>,
    connections: AtomicUsize,
    stop: AtomicBool,
    answer: Box<AnswerFn>,
}

impl FakeAgentd {
    fn start<F>(answer: F) -> Arc<Self>
    where
        F: Fn(&str, &Value) -> Option<Reply> + Send + Sync + 'static,
    {
        let answer: Box<AnswerFn> = Box::new(answer);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let daemon = Arc::new(Self {
            listener,
            port,
            received: Mutex::new(Vec::new()),
            arrived: Condvar::new(),
            writer: Mutex::new(None),
            connections: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            answer,
        });
        let serving = Arc::clone(&daemon);
        std::thread::Builder::new()
            .name("fake-agentd".to_owned())
            .spawn(move || serving.serve())
            .expect("fake agentd thread");
        daemon
    }

    fn serve(self: Arc<Self>) {
        while !self.stop.load(Ordering::Relaxed) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    self.connections.fetch_add(1, Ordering::Relaxed);
                    stream.set_nodelay(true).expect("nodelay");
                    if let Err(error) = self.converse(stream) {
                        eprintln!("fake agentd connection ended: {error}");
                    }
                    *self.writer.lock().expect("writer") = None;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::yield_now();
                }
                Err(error) => {
                    eprintln!("fake agentd accept failed: {error}");
                    return;
                }
            }
        }
    }

    fn converse(&self, stream: TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_millis(25)))?;
        let mut write_half = stream.try_clone()?;
        *self.writer.lock().expect("writer") = Some(stream.try_clone()?);
        let mut reader = BufReader::new(stream);
        let mut pending = String::new();
        let Some(hello) = self.read_frame(&mut reader, &mut pending)? else {
            return Ok(());
        };
        self.remember(&hello);
        if hello.get("t").and_then(Value::as_str) != Some("hello") {
            let refused = json!({ "t": "refused", "reason": "hello_expected" });
            self.send(&mut write_half, &refused)?;
            self.remember(&refused);
            return Ok(());
        }
        if hello.get("token").and_then(Value::as_str) != Some(TOKEN) {
            let refused = json!({ "t": "refused", "reason": "bad_token" });
            self.send(&mut write_half, &refused)?;
            self.remember(&refused);
            return Ok(());
        }
        self.send(
            &mut write_half,
            &json!({ "t": "welcome", "v": 1, "node": "fake-agentd", "streams": {}, "floors": {} }),
        )?;
        loop {
            let Some(frame) = self.read_frame(&mut reader, &mut pending)? else {
                return Ok(());
            };
            self.remember(&frame);
            match frame.get("t").and_then(Value::as_str).unwrap_or_default() {
                "cmd" => {
                    let id = frame.get("id").and_then(Value::as_str).unwrap_or_default();
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
                    let answer = match reply {
                        Reply::Data(data) => {
                            json!({ "t": "res", "id": id, "ok": true, "data": data })
                        }
                        Reply::Error(error) => {
                            json!({ "t": "res", "id": id, "ok": false, "error": error })
                        }
                        Reply::Drop => return Ok(()),
                    };
                    self.send(&mut write_half, &answer)?;
                }
                "ping" => {
                    let n = frame.get("n").cloned().unwrap_or(json!(0));
                    let at = frame.get("at").cloned().unwrap_or(json!(0));
                    self.send(&mut write_half, &json!({ "t": "pong", "n": n, "at": at }))?;
                }
                _ => {}
            }
        }
    }

    /// One frame line, waiting out a read timeout so the daemon notices a closed connection instead of blocking on it.
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

    fn send(&self, writer: &mut TcpStream, frame: &Value) -> std::io::Result<()> {
        let mut line = serde_json::to_string(frame).unwrap_or_default();
        line.push('\n');
        writer.write_all(line.as_bytes())?;
        writer.flush()
    }

    fn remember(&self, frame: &Value) {
        let kind = frame
            .get("t")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.received.lock().expect("received").push(Received {
            kind,
            frame: frame.clone(),
        });
        self.arrived.notify_all();
    }

    fn frames(&self) -> Vec<Received> {
        self.received.lock().expect("received").clone()
    }

    fn ops(&self) -> Vec<String> {
        self.frames()
            .iter()
            .filter_map(|entry| entry.frame.get("op").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    fn of_kind(&self, kind: &str) -> Vec<Value> {
        self.frames()
            .iter()
            .filter(|entry| entry.kind == kind)
            .map(|entry| entry.frame.clone())
            .collect()
    }

    fn commands(&self) -> Vec<Value> {
        self.frames()
            .iter()
            .filter(|entry| entry.kind == "cmd")
            .map(|entry| entry.frame.clone())
            .collect()
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    /// Pushes one server frame down the live connection, as a daemon publishing an event does.
    fn emit(&self, frame: &Value) -> bool {
        let mut guard = self.writer.lock().expect("writer");
        let Some(writer) = guard.as_mut() else {
            return false;
        };
        let mut line = serde_json::to_string(frame).unwrap_or_default();
        line.push('\n');
        writer.write_all(line.as_bytes()).is_ok() && writer.flush().is_ok()
    }

    fn drop_connection(&self) {
        let mut guard = self.writer.lock().expect("writer");
        if let Some(writer) = guard.as_mut() {
            let _ = writer.shutdown(Shutdown::Both);
        }
    }

    fn wait_for(&self, predicate: impl Fn(&[Received]) -> bool) -> bool {
        let deadline = Instant::now() + WAIT;
        let mut frames = self.received.lock().expect("received");
        while !predicate(&frames) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (next, _) = self.arrived.wait_timeout(frames, left).expect("arrived");
            frames = next;
        }
        true
    }
}

impl Drop for FakeAgentd {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn peer(daemon: &FakeAgentd, dir: &std::path::Path, token: &str) -> AgentdPeer {
    let token_file = dir.join("token");
    std::fs::write(&token_file, token).expect("token file");
    AgentdPeer {
        device: "b".to_owned(),
        address: "127.0.0.1".to_owned(),
        port: daemon.port,
        token_file,
    }
}

fn client(daemon: &FakeAgentd, dir: &std::path::Path) -> Arc<AgentdClient> {
    Arc::new(AgentdClient::with_peers(
        "test-node",
        vec![peer(daemon, dir, TOKEN)],
    ))
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

fn listing_answer() -> Value {
    json!({
        "sessions": [{
            "session_id": "rk-1",
            "durable_id": "01932bf3-0280-7abc-8def-0123456789ab",
            "title": "on b",
            "project": "/work/app",
            "path": "/work/app",
            "state": "running",
            "updated_ms": 1_700_000_000_000i64,
            "model": { "provider": "openai", "id": "gpt-6" },
            "waiting": { "kind": "rate_limit", "until": "2026-11-02T00:01:00.000Z", "attempt": 2, "maxAttempts": 5 },
            "engine_version": "2026.11.1",
        }],
        "capabilities": ["open_session_fork"],
    })
}

#[test]
fn a_listing_reaches_the_daemon_over_the_wire_and_keeps_the_retry_wait_and_engine_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, _args| {
        (op == "mhc.sessions.list").then(|| Reply::Data(listing_answer()))
    });
    let ports = AgentdPorts::new(client(&daemon, dir.path()));

    let listing = block_on(ports.listing(None)).expect("listing");
    assert_eq!(listing.rows.len(), 1);
    let row = &listing.rows[0];
    assert_eq!(row.device, "b");
    assert_eq!(row.state, "running");
    assert_eq!(row.engine_version.as_deref(), Some("2026.11.1"));
    assert_eq!(
        row.waiting
            .as_ref()
            .and_then(|waiting| waiting.get("attempt"))
            .and_then(Value::as_i64),
        Some(2)
    );
    assert_eq!(
        row.model.as_ref().map(|model| model.id.as_str()),
        Some("gpt-6")
    );
    assert!(listing.incomplete.is_empty());
    assert_eq!(daemon.ops(), vec!["mhc.sessions.list".to_owned()]);
}

#[test]
fn a_hello_carrying_the_wrong_token_is_refused_and_no_op_is_ever_served() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, _args| {
        (op == "mhc.sessions.list").then(|| Reply::Data(listing_answer()))
    });
    let client = Arc::new(AgentdClient::with_peers(
        "test-node",
        vec![peer(&daemon, dir.path(), "not-the-token")],
    ));

    let error = block_on(client.call(
        "b",
        "mhc.sessions.list",
        json!({}),
        Duration::from_millis(500),
    ))
    .expect_err("a refused handshake answers no data");
    assert!(matches!(error, AgentdError::Timeout), "{error:?}");
    assert!(daemon.ops().is_empty(), "no op is served after a refusal");
    assert!(
        daemon.wait_for(|frames| frames.iter().any(|entry| entry.kind == "refused")),
        "the daemon answers the hello it refused"
    );
    assert_eq!(
        daemon.of_kind("refused")[0]["reason"],
        json!("bad_token"),
        "the daemon refused the hello itself"
    );
}

#[test]
fn one_locate_probe_answers_the_key_the_device_holds_the_session_under() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, args| {
        if op != "mhc.session.locate" {
            return None;
        }
        let asked = args
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Some(Reply::Data(if asked == "dur-1" {
            json!({ "found": true, "row": { "session_id": "rk-9" } })
        } else {
            json!({ "found": false })
        }))
    });
    let ports = AgentdPorts::new(client(&daemon, dir.path()));

    assert_eq!(
        block_on(ports.lookup("b".to_owned(), "dur-1".to_owned())),
        RemoteLookupOutcome::Found {
            key: "rk-9".to_owned()
        }
    );
    assert_eq!(
        block_on(ports.lookup("b".to_owned(), "dur-2".to_owned())),
        RemoteLookupOutcome::NotHeld
    );
    assert_eq!(
        block_on(ports.locate("b".to_owned(), "dur-1".to_owned())),
        RemoteLocateOutcome::Located {
            session_id: "rk-9".to_owned()
        }
    );
}

#[test]
fn an_op_the_daemon_does_not_serve_answers_unsupported_and_an_unconfigured_device_is_silent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|_op, _args| None);
    let ports = AgentdPorts::new(client(&daemon, dir.path()));

    assert_eq!(
        block_on(ports.session_command(
            "b".to_owned(),
            "rk-1".to_owned(),
            json!({ "type": "prompt" }),
            Default::default(),
        )),
        RemoteCommandOutcome::Refused {
            reason: "unsupported".to_owned(),
            detail: Some("mhc.session.command".to_owned()),
        }
    );
    let stranger = AgentdPorts::new(Arc::new(AgentdClient::with_peers("test-node", Vec::new())));
    assert_eq!(
        block_on(stranger.lookup("c".to_owned(), "dur-1".to_owned())),
        RemoteLookupOutcome::Silent {
            reason: AGENTD_ENTRY_REQUIRED.to_owned()
        }
    );
    let error = block_on(stranger.client().call(
        "c",
        "mhc.session.read",
        json!({}),
        Duration::from_secs(1),
    ))
    .expect_err("a peer without agentd is not reached");
    let AgentdError::Refused { reason, detail } = &error else {
        panic!("expected a missing-agentd refusal: {error:?}");
    };
    assert_eq!(reason, AGENTD_ENTRY_REQUIRED);
    assert!(
        detail
            .as_deref()
            .is_some_and(|detail| detail.contains("relayd.json")),
        "{detail:?}"
    );
    let failure = relay_outcome_failure(
        &maho_omo_gateway::thread::remote::agentd::outcome_of(&error),
        "c",
    );
    assert_eq!(failure.code.as_str(), "host_unavailable");
    let details = failure.details.as_ref().and_then(|value| value.as_object());
    assert_eq!(
        details
            .and_then(|map| map.get("reason"))
            .and_then(Value::as_str),
        Some(AGENTD_ENTRY_REQUIRED)
    );
    let fix = details
        .and_then(|map| map.get("fix"))
        .and_then(Value::as_str);
    assert!(
        fix.is_some_and(|fix| fix.contains("relayd.json")),
        "{fix:?}"
    );
    assert_eq!(
        details
            .and_then(|map| map.get("device"))
            .and_then(Value::as_str),
        Some("c")
    );
    assert!(
        failure.next_action.contains("`agentd` object"),
        "{}",
        failure.next_action
    );
}

#[test]
fn a_command_reaches_the_engine_and_a_record_bigger_than_one_frame_is_sent_in_parts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, args| {
        if op != "mhc.session.command" {
            return None;
        }
        Some(Reply::Data(json!({
            "state": "engine_applied",
            "receipt": { "turn": 1, "part": args.get("part").cloned().unwrap_or(Value::Null) },
        })))
    });
    let client = client(&daemon, dir.path());

    let small = block_on(client.send_command(
        "b",
        "rk-1",
        &json!({ "type": "prompt", "text": "go" }),
        Duration::from_secs(5),
    ))
    .expect("command");
    assert_eq!(small["state"], json!("engine_applied"));

    let big = json!({ "type": "prompt", "text": "x".repeat(200 * 1024) });
    let answer = block_on(client.send_command("b", "rk-1", &big, Duration::from_secs(20)))
        .expect("chunked command");
    assert_eq!(answer["state"], json!("engine_applied"));
    let parts: Vec<Value> = daemon
        .commands()
        .iter()
        .filter(|frame| frame.get("op").and_then(Value::as_str) == Some("mhc.session.command"))
        .filter_map(|frame| frame["args"].get("part").cloned())
        .collect();
    assert!(parts.len() >= 2, "a 200 KiB record is several parts");
    let total = parts[0]["total"].as_u64().unwrap_or_default();
    assert_eq!(parts.len() as u64, total);
    assert_eq!(parts[0]["index"].as_u64(), Some(0));
    let ids: Vec<&str> = parts
        .iter()
        .filter_map(|part| part.get("id").and_then(Value::as_str))
        .collect();
    assert!(
        ids.windows(2).all(|pair| pair[0] == pair[1]),
        "one id per record"
    );
}

#[test]
fn an_open_asks_the_device_for_the_engines_own_field_names_and_answers_the_thread_it_made() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, _args| {
        (op == "mhc.session.open").then(|| {
            Reply::Data(json!({
                "session_id": "rk-2",
                "durable_id": "dur-2",
                "stream": "mhc-session:rk-2",
                "started": true,
            }))
        })
    });
    let host = AgentdSessionHost::new(client(&daemon, dir.path()), "b");

    let opened = block_on(host.open(
        SessionOpenRequest {
            cwd: Some("/work/app".to_owned()),
            name: Some("child".to_owned()),
            provider: Some("openai".to_owned()),
            model_id: Some("gpt-6".to_owned()),
            ..Default::default()
        },
        Default::default(),
    ))
    .expect("opened");
    assert_eq!(opened.thread, "dur-2");
    assert_eq!(opened.device.as_deref(), Some("b"));
    assert_eq!(opened.cwd, "/work/app");

    let opens = daemon.commands();
    let open = opens
        .iter()
        .find(|frame| frame.get("op").and_then(Value::as_str) == Some("mhc.session.open"))
        .expect("the open frame");
    assert_eq!(open["args"]["launch"]["cwd"], json!("/work/app"));
    assert_eq!(open["args"]["launch"]["model"], json!("openai/gpt-6"));
    assert_eq!(open["args"]["launch"]["name"], json!("child"));
    assert!(open["args"]["launch"].get("modelId").is_none());
    assert!(open["args"]["launch"].get("provider").is_none());
    assert!(open["args"].get("session").is_none());
}

#[test]
fn a_reopen_names_the_session_that_device_holds_and_a_fork_names_its_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, _args| {
        (op == "mhc.session.open").then(|| {
            Reply::Data(json!({ "session_id": "rk-2", "durable_id": "dur-2", "started": false }))
        })
    });
    let host = AgentdSessionHost::new(client(&daemon, dir.path()), "b");

    block_on(host.open(
        SessionOpenRequest {
            remote_session: Some("dur-1".to_owned()),
            cwd: Some("/work/app".to_owned()),
            ..Default::default()
        },
        Default::default(),
    ))
    .expect("reopened");
    block_on(host.open(
        SessionOpenRequest {
            fork_from: Some("dur-1".to_owned()),
            cwd: Some("/work/app".to_owned()),
            ..Default::default()
        },
        Default::default(),
    ))
    .expect("forked");

    let opens: Vec<Value> = daemon
        .commands()
        .into_iter()
        .filter(|frame| frame.get("op").and_then(Value::as_str) == Some("mhc.session.open"))
        .collect();
    assert_eq!(opens.len(), 2);
    assert_eq!(opens[0]["args"]["session"], json!("dur-1"));
    assert!(opens[0]["args"].get("fork_from").is_none());
    assert!(opens[0]["args"]["launch"].get("forkFrom").is_none());
    assert!(opens[0]["args"]["launch"].get("session").is_none());
    assert_eq!(opens[1]["args"]["fork_from"], json!("dur-1"));
    assert!(opens[1]["args"]["launch"].get("forkFrom").is_none());
    assert!(opens[1]["args"].get("session").is_none());
}

#[test]
fn a_non_contract_launch_key_is_refused_invalid_request_by_the_strict_daemon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, _args| {
        (op == "mhc.session.open").then(|| Reply::Data(json!({ "session_id": "rk-2" })))
    });
    let client = client(&daemon, dir.path());

    for bad_launch in [
        json!({ "forkFrom": "dur-1" }),
        json!({ "sessionPath": "/tmp/s.jsonl" }),
        json!({ "modelId": "gpt-6" }),
        json!({ "durableSessionId": "dur-1" }),
        json!({ "cwd": "relative" }),
    ] {
        let err = block_on(client.call(
            "b",
            "mhc.session.open",
            json!({ "launch": bad_launch }),
            Duration::from_secs(5),
        ))
        .expect_err("non-contract launch key must be refused");
        assert!(
            matches!(err, AgentdError::Refused { ref reason, .. } if reason == "invalid_request"),
            "for {bad_launch}: {err:?}"
        );
    }
}

#[test]
fn every_refusal_the_contract_names_reaches_the_caller_as_its_own_typed_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let answer: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let served = Arc::clone(&answer);
    let daemon = FakeAgentd::start(move |op, _args| {
        if op != "mhc.session.open" {
            return None;
        }
        let error = served.lock().expect("answer").clone()?;
        Some(Reply::Error(error))
    });
    let host = AgentdSessionHost::new(client(&daemon, dir.path()), "b");

    let cases = [
        ("fork_source_not_found", "not_found"),
        ("fork_source_not_local", "not_found"),
        ("session_busy", "turn_active"),
        ("session_cap", "overloaded"),
        ("unsupported:mhc.session.open", "unsupported"),
    ];
    for (daemon_error, expected) in cases {
        *answer.lock().expect("answer") = Some(daemon_error.to_owned());
        let error = block_on(host.open(
            SessionOpenRequest {
                fork_from: Some("dur-9".to_owned()),
                ..Default::default()
            },
            Default::default(),
        ))
        .expect_err("a refused open opens nothing");
        let refusal = error.refusal().expect("a typed refusal");
        assert_eq!(refusal.code.as_str(), expected, "for {daemon_error}");
        assert!(
            !refusal.next_action.is_empty(),
            "every refusal names the next step: {daemon_error}"
        );
        assert_eq!(
            refusal
                .details
                .as_ref()
                .and_then(|details| details.get("reason")),
            Some(&json!(
                daemon_error.split(':').next().unwrap_or(daemon_error)
            )),
            "the daemon's own reason is kept for {daemon_error}"
        );
    }
}

#[test]
fn a_follow_stream_keeps_its_cursor_across_a_reconnect_and_releases_with_its_handle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, _args| {
        (op == "mhc.session.follow")
            .then(|| Reply::Data(json!({ "stream": "mhc-session:rk-1", "seq": 0 })))
    });
    let client = client(&daemon, dir.path());
    let follow_port = AgentdFollowPort::new(Arc::clone(&client));

    let mut follow = block_on(follow_port.follow("b", "rk-1", None)).expect("follow");
    assert_eq!(follow.stream(), "mhc-session:rk-1");
    assert!(daemon.emit(&json!({
        "t": "evt",
        "s": "mhc-session:rk-1",
        "seq": 1,
        "e": { "kind": "started" },
    })));
    let first = block_on(follow.next_event()).expect("the first event");
    assert_eq!(first.seq, 1);
    assert_eq!(first.event["kind"], json!("started"));

    let link = client.link("b").expect("link");
    assert_eq!(link.cursor_of("mhc-session:rk-1"), Some(1));
    assert_eq!(link.subscriber_count("mhc-session:rk-1"), 1);

    daemon.drop_connection();
    assert!(
        daemon.wait_for(|frames| frames.iter().filter(|frame| frame.kind == "hello").count() >= 2),
        "the link reconnected"
    );
    let hellos = daemon.of_kind("hello");
    assert_eq!(hellos[1]["resume"]["mhc-session:rk-1"], json!(1));
    assert_eq!(hellos[1]["subscribe"], json!(["mhc-session:rk-1"]));

    follow.release();
    assert_eq!(link.subscriber_count("mhc-session:rk-1"), 0);
    assert_eq!(link.cursor_of("mhc-session:rk-1"), None);
}

#[test]
fn a_command_lost_with_its_connection_is_sent_again_under_its_id_and_answered_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let answered = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&answered);
    let daemon = FakeAgentd::start(move |op, _args| {
        if op != "mhc.sessions.list" {
            return None;
        }
        if counted.fetch_add(1, Ordering::Relaxed) == 0 {
            return Some(Reply::Drop);
        }
        Some(Reply::Data(listing_answer()))
    });
    let client = client(&daemon, dir.path());

    let listing =
        block_on(client.listing(Some("b".to_owned()))).expect("answered after the resend");
    assert_eq!(listing.rows.len(), 1);
    assert!(daemon.connections() >= 2, "the link reconnected");
    let ids: Vec<String> = daemon
        .commands()
        .iter()
        .filter(|frame| frame.get("op").and_then(Value::as_str) == Some("mhc.sessions.list"))
        .filter_map(|frame| frame.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    assert_eq!(ids.len(), 2, "the same call reached the daemon twice");
    assert_eq!(ids[0], ids[1], "resent under its own id, not a new one");
}

#[test]
fn a_delivery_is_pushed_to_the_holder_and_its_outcome_read_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, args| match op {
        "mhc.delivery.push" => Some(Reply::Data(json!({
            "outcome": "accepted",
            "row_id": "row-7",
            "queue_position": 2,
            "deduplicated": args.get("delivery_id").and_then(Value::as_str) == Some("dup"),
        }))),
        "mhc.delivery.outcome" => Some(Reply::Data(json!({
            "outcome": "delivered",
            "settled_ms": 1_700_000_001_000i64,
        }))),
        _ => None,
    });
    let port = AgentdDeliveryPort::new(client(&daemon, dir.path()));

    let pushed = block_on(port.push(
        "b",
        "d-1",
        &json!({ "device": "a", "session": "s-1" }),
        "dur-2",
        &json!({ "message": "hi" }),
        "ledger-1",
    ))
    .expect("push");
    assert_eq!(
        pushed,
        DeliveryPushOutcome::Accepted {
            row_id: Some("row-7".to_owned()),
            queue_position: Some(2),
            deduplicated: false,
        }
    );
    assert_eq!(
        block_on(port.outcome("b", "d-1")).expect("outcome"),
        DeliveryOutcome::Settled {
            outcome: "delivered".to_owned(),
            reason: None,
            settled_ms: Some(1_700_000_001_000),
        }
    );
    assert_eq!(
        daemon.ops(),
        vec![
            "mhc.delivery.push".to_owned(),
            "mhc.delivery.outcome".to_owned()
        ]
    );
}

#[test]
fn a_delivery_the_holder_never_took_reads_as_unknown_rather_than_settled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = FakeAgentd::start(|op, _args| {
        (op == "mhc.delivery.outcome").then(|| Reply::Data(json!({ "outcome": "unknown" })))
    });
    let port = AgentdDeliveryPort::new(client(&daemon, dir.path()));
    assert_eq!(
        block_on(port.outcome("b", "d-404")).expect("outcome"),
        DeliveryOutcome::Unknown
    );
}

#[test]
fn a_device_without_an_agentd_entry_is_never_reached() {
    let client = AgentdClient::with_peers("test-node", Vec::new());
    assert!(!client.configured());
    assert!(!client.reaches("b"));
    assert!(client.link("b").is_none());
    let ports = AgentdPorts::new(Arc::new(client));
    let listing = block_on(ports.listing(Some("b".to_owned()))).expect("a listing of nothing");
    assert!(listing.rows.is_empty());
    assert_eq!(listing.incomplete, vec!["b".to_owned()]);
    assert_eq!(
        listing
            .devices
            .as_ref()
            .and_then(|states| states.get("b"))
            .map(String::as_str),
        Some("unconfigured")
    );
}
