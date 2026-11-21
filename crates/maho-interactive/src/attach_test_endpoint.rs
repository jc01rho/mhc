//! Test double for an RPC endpoint an attached TUI connects to (senpi `test/helpers/attach-endpoint.ts`):
//! a terminal control socket with its secret beside it, answering the commands an attach issues.
use maho_rpc::socket_transport::create_socket_secret;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The reply to an `extension_ui_response`: success by default, a refusal's error, or none at all.
#[derive(Default)]
pub enum AnswerBehavior {
    #[default]
    Accept,
    Refuse(String),
    Silent,
}

#[derive(Default)]
pub struct EndpointOptions {
    /// Commands left unanswered, as a stalled endpoint would.
    pub hold: Vec<String>,
    /// Commands refused with the given error.
    pub refuse: HashMap<String, String>,
    /// The listed row's `retain_on_disconnect`; true unless a case needs a non-retained session.
    pub retained: Option<bool>,
    pub answer: AnswerBehavior,
}

type Commands = Arc<(Mutex<Vec<Value>>, Condvar)>;

pub struct Endpoint {
    pub path: String,
    _directory: tempfile::TempDir,
    peer: Arc<Mutex<Option<UnixStream>>>,
    commands: Commands,
}

impl Endpoint {
    pub fn start(options: EndpointOptions) -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory
            .path()
            .join("t-0123456789abcdef.sock")
            .to_string_lossy()
            .into_owned();
        let secret =
            create_socket_secret(Path::new(&format!("{path}.secret"))).expect("socket secret");
        let listener = UnixListener::bind(&path).expect("bind test endpoint");
        let peer: Arc<Mutex<Option<UnixStream>>> = Arc::default();
        let commands: Commands = Arc::default();
        let serving = (peer.clone(), commands.clone());
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let (peer, commands) = serving;
            let mut presented = [0u8; 32];
            if stream.read_exact(&mut presented).is_err() || presented != secret {
                return;
            }
            let Ok(writer) = stream.try_clone() else {
                return;
            };
            *lock(&peer) = Some(writer);
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { return };
                let Ok(command) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let (list, signal) = &*commands;
                lock(list).push(command.clone());
                signal.notify_all();
                serve(&options, &peer, &command);
            }
        });
        Self {
            path,
            _directory: directory,
            peer,
            commands,
        }
    }

    /// One record from the endpoint, tagged with the live session.
    pub fn send(&self, record: &Value) {
        let mut tagged = json!({"sessionId": "live"});
        if let (Some(target), Some(fields)) = (tagged.as_object_mut(), record.as_object()) {
            target.extend(fields.clone());
        }
        write_line(&self.peer, &tagged.to_string());
    }

    /// One raw line, as a misbehaving endpoint would write it.
    pub fn send_raw(&self, line: &str) {
        write_line(&self.peer, line);
    }

    /// The first command of `kind`, already received or still to come.
    pub fn command(&self, kind: &str) -> Value {
        self.wait_for(|command| command["type"] == kind)
    }

    pub fn wait_for(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let (list, signal) = &*self.commands;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut commands = lock(list);
        loop {
            if let Some(found) = commands.iter().find(|command| predicate(command)) {
                return found.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for a command");
            commands = signal
                .wait_timeout(commands, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    pub fn commands(&self) -> Vec<Value> {
        lock(&self.commands.0).clone()
    }

    pub fn disconnect(&self) {
        if let Some(peer) = lock(&self.peer).take() {
            let _ = peer.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn write_line(peer: &Mutex<Option<UnixStream>>, line: &str) {
    if let Some(stream) = lock(peer).as_mut() {
        let _ = stream.write_all(format!("{line}\n").as_bytes());
    }
}

fn reply(peer: &Mutex<Option<UnixStream>>, command: &Value, outcome: Value) {
    let mut frame = json!({"type": "response", "id": command["id"], "command": command["type"]});
    if let (Some(target), Some(fields)) = (frame.as_object_mut(), outcome.as_object()) {
        target.extend(fields.clone());
    }
    write_line(peer, &frame.to_string());
}

fn serve(options: &EndpointOptions, peer: &Mutex<Option<UnixStream>>, command: &Value) {
    let kind = command["type"].as_str().unwrap_or_default();
    let data = match kind {
        "list_sessions" => json!({"sessions": [{
            "sessionId": "live",
            "durableSessionId": "durable",
            "cwd": "/fixture",
            "status": "open",
            "retain_on_disconnect": options.retained.unwrap_or(true),
        }]}),
        "attach_session" => json!({"sessionId": "live", "attached": true}),
        "get_state" => {
            json!({"sessionId": "durable", "cwd": "/fixture", "isStreaming": false, "pendingDialogs": []})
        }
        "get_entries" => {
            let next_before = if command.get("before").is_some() {
                Value::Null
            } else {
                json!("history")
            };
            json!({
                "entries": [{
                    "type": "message",
                    "id": "history",
                    "parentId": null,
                    "timestamp": "2026-01-01",
                    "message": {"role": "user", "content": "Previous question", "timestamp": 1},
                }],
                "leafId": "history",
                "next_before": next_before,
            })
        }
        "subscribe_sessions" => json!({
            "subscriptionId": "sub",
            "snapshot": [],
            "seq": 0,
            "hostGeneration": "fixture",
            "hostInstanceId": "fixture",
        }),
        "release_attachment" => json!({"deferred": false}),
        "extension_ui_response" => {
            match &options.answer {
                AnswerBehavior::Accept => reply(peer, command, json!({"success": true})),
                AnswerBehavior::Refuse(error) => {
                    reply(peer, command, json!({"success": false, "error": error}));
                }
                AnswerBehavior::Silent => {}
            }
            return;
        }
        _ => json!({}),
    };
    if options.hold.iter().any(|held| held == kind) {
        return;
    }
    match options.refuse.get(kind) {
        Some(error) => reply(peer, command, json!({"success": false, "error": error})),
        None => reply(peer, command, json!({"success": true, "data": data})),
    }
}
