//! The local API v1 server (`docs/local-api.md` upstream) on `<state dir>/service.sock`.
//!
//! The access contract is the device service's: a private state dir, a per-boot token, a `welcome`
//! that proves the token, and nothing honored before the client proves it back. Served requests:
//! status, the enable/control/wake switches, device info and forget, devices, endpoints, session
//! listing rounds, attach/detach and commands with the command_id ledger. Requests this service
//! defines but does not serve (machine operations, uploads, delegated commands, history reads,
//! upgrade hand-off, device adoption) answer `unsupported`, pointing to the agentd path the
//! account-wide sessions and deliveries ops use instead.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use device_service_core::local_api::LocalApiError;
use device_service_core::local_api::access::{
    EntryKind, SOCKET_FILE, UNIX_ENDPOINT, insecure, posix_secure_file,
};
use device_service_core::local_api::limits::{
    COMMAND_TEXT_MAX_CHARS, LOCAL_INLINE_MAX, command_text_fits, line_bytes,
};
use device_service_core::local_api::proof::{
    client_proof, decoded_bytes, mint_id, mint_nonce, proof_matches, server_proof,
};
use device_service_core::local_api::protocol::{
    ClientInfo, Welcome, error_response, event_message, ok_response, refused_message,
};
use device_service_core::local_api::vocabulary::{CLIENT_NAMES, LOCAL_API_VERSION};
use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::config::PeerConfig;
use crate::ledger::{Admission, valid_command_id};
use crate::peer::{PeerError, command_state, peer_request};
use crate::session_host::HostSession;
use crate::state::{RelaydState, platform};
use crate::wire::{LineReader, now_ms, now_rfc3339, write_value};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(25);

/// Engine methods a command may carry to another device (or this one) through the service.
pub const SESSION_METHODS: [&str; 7] = [
    "prompt",
    "steer",
    "follow_up",
    "abort",
    "set_model",
    "get_state",
    "close_session",
];
/// Methods that configure a local connection and never travel (`method_local_only`).
pub const LOCAL_ONLY_METHODS: [&str; 5] = [
    "open_session",
    "attach_session",
    "set_client_info",
    "subscribe_sessions",
    "detach_connection",
];

const UNSUPPORTED_REQUESTS: [&str; 10] = [
    "refresher.register",
    "device.adopt",
    "enforcement.report",
    "owner.shutdown_for_upgrade",
    "session.endpoints",
    "session.locate",
    "session.read",
    "session.command_part",
    "sessions.events.subscribe",
    "deliveries.serve",
];

/// Why the 10 names above are refused here, and how a caller reaches them instead. The code stays
/// `unsupported`; this is the reason and the next action, not a new request the relay serves.
const AGENTD_ONLY_REASON: &str =
    "mhc-relayd does not serve this request; it is served over the agentd path";
const AGENTD_ONLY_ACTION: &str = "Add an `agentd` object (address, port, tokenFile) to this peer in relayd.json, then retry over agentd.";

/// Binds the private socket. A socket a live service still answers on is refused (another device
/// service runs here); a stale one is replaced; anything else at that path is refused.
pub fn bind_local(state_dir: &Path) -> Result<UnixListener, LocalApiError> {
    use std::os::unix::fs::FileTypeExt;
    let path = state_dir.join(SOCKET_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                return Err(LocalApiError::new(
                    "owner_transfer",
                    "another device service already answers on this state dir",
                ));
            }
            std::fs::remove_file(&path)
                .map_err(|error| LocalApiError::new("internal", error.to_string()))?;
        }
        Ok(_) => return Err(insecure("the socket path holds something else")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(LocalApiError::new("internal", error.to_string())),
    }
    let listener = UnixListener::bind(&path)
        .map_err(|error| LocalApiError::new("internal", error.to_string()))?;
    posix_secure_file(&path, EntryKind::Socket)?;
    Ok(listener)
}

pub async fn accept_loop(
    state: Arc<RelaydState>,
    listener: UnixListener,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(serve_connection(Arc::clone(&state), stream));
                }
                Err(_) => break,
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

type Outbox = mpsc::UnboundedSender<Value>;

async fn serve_connection(state: Arc<RelaydState>, stream: UnixStream) {
    let (read, write) = stream.into_split();
    let (out, mut outgoing) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let mut write = write;
        while let Some(message) = outgoing.recv().await {
            if write_value(&mut write, &message).await.is_err() {
                break;
            }
        }
        let _ = write.shutdown().await;
    });
    let mut reader = LineReader::new(read);
    let Some(mut connection) = greet(state, &mut reader, out).await else {
        return;
    };
    while let Ok(Some(message)) = reader.next_value().await {
        if !connection.handle(message).await {
            break;
        }
    }
    connection.end();
}

fn valid_request_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|byte| (0x20..0x7f).contains(&byte))
}

async fn greet(
    state: Arc<RelaydState>,
    reader: &mut LineReader<OwnedReadHalf>,
    out: Outbox,
) -> Option<Connection> {
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, reader.next_value()).await {
        Ok(Ok(Some(hello))) => hello,
        _ => {
            let _ = out.send(refused_message("malformed"));
            return None;
        }
    };
    let refuse = |reason: &str| -> Option<Connection> {
        let _ = out.send(refused_message(reason));
        None
    };
    if hello.get("type").and_then(Value::as_str) != Some("hello") {
        return refuse("malformed");
    }
    if hello.get("v").and_then(Value::as_u64) != Some(LOCAL_API_VERSION) {
        return refuse("version");
    }
    let Some(client) = hello
        .get("client")
        .and_then(|client| serde_json::from_value::<ClientInfo>(client.clone()).ok())
    else {
        return refuse("malformed");
    };
    if !CLIENT_NAMES.contains(&client.name.as_str()) {
        return refuse("malformed");
    }
    if client.name == "desktop" && client.data_home.is_none() {
        return refuse("data_home_required");
    }
    let Some(client_nonce) = hello
        .get("client_nonce")
        .and_then(Value::as_str)
        .filter(|nonce| decoded_bytes(nonce, 32, "a nonce").is_ok())
    else {
        return refuse("malformed");
    };
    let (Ok(server_nonce), Ok(proof)) = (
        mint_nonce(),
        server_proof(&state.token, client_nonce, UNIX_ENDPOINT),
    ) else {
        return refuse("malformed");
    };
    let welcome = Welcome {
        v: LOCAL_API_VERSION,
        service_version: state.service_version.clone(),
        owner: "service".into(),
        server_proof: proof,
        server_nonce: server_nonce.clone(),
        capabilities: vec!["sessions".into(), "devices".into(), "control".into()],
        state: state.status(),
    };
    let _ = out.send(welcome.to_message());
    Some(Connection {
        state,
        out,
        server_nonce,
        authenticated: false,
        attaches: HashMap::new(),
        tasks: Vec::new(),
    })
}

struct Connection {
    state: Arc<RelaydState>,
    out: Outbox,
    server_nonce: String,
    authenticated: bool,
    /// This connection's attach handles: handle to (device id, session id).
    attaches: HashMap<String, (String, String)>,
    /// Event tasks this connection started; they end with it.
    tasks: Vec<JoinHandle<()>>,
}

fn error(code: &str, reason: &str) -> LocalApiError {
    LocalApiError::new(code, reason)
}

impl Connection {
    fn end(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }

    fn reply(&self, id: &str, answer: Result<Value, LocalApiError>) {
        let message = match answer {
            Ok(data) => ok_response(id, data),
            Err(error) => error_response(id, &error),
        };
        let _ = self.out.send(message);
    }

    /// `false` closes the connection.
    async fn handle(&mut self, message: Value) -> bool {
        let Some(id) = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_request_id(id))
            .map(str::to_owned)
        else {
            return false;
        };
        let kind = message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if !self.authenticated {
            if kind != "authenticate" {
                self.reply(&id, Err(error("unauthenticated", "authenticate first")));
                return true;
            }
            let offered = message
                .get("client_proof")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let proven = client_proof(&self.state.token, &self.server_nonce, UNIX_ENDPOINT)
                .is_ok_and(|expected| proof_matches(&expected, offered));
            if proven {
                self.authenticated = true;
                self.reply(&id, Ok(json!({})));
                return true;
            }
            self.reply(
                &id,
                Err(error(
                    "client_proof_invalid",
                    "the client proof does not match this connection",
                )),
            );
            return false;
        }
        let answer = self.dispatch(&id, &kind, &message).await;
        if let Some(answer) = answer {
            self.reply(&id, answer);
        }
        true
    }

    /// `None` when the handler already replied (it must answer before the events that follow).
    async fn dispatch(
        &mut self,
        id: &str,
        kind: &str,
        message: &Value,
    ) -> Option<Result<Value, LocalApiError>> {
        let state = Arc::clone(&self.state);
        let answer = match kind {
            "authenticate" => Ok(json!({})),
            "status.get" => Ok(state.status()),
            "status.subscribe" => {
                self.reply(id, Ok(json!({})));
                self.follow_status();
                return None;
            }
            "service.set_enabled" => match message.get("enabled").and_then(Value::as_bool) {
                Some(enabled) => state
                    .update_settings(|settings| {
                        settings.enabled = enabled;
                        if enabled {
                            settings.forgotten = false;
                        }
                    })
                    .map(|settings| json!({ "enabled": settings.enabled }))
                    .map_err(|reason| error("internal", &reason)),
                None => Err(error("invalid_request", "enabled")),
            },
            "device.info" => {
                let device = if state.settings().forgotten {
                    Value::Null
                } else {
                    state.this_device()
                };
                Ok(json!({ "device": device }))
            }
            "device.forget" => state
                .update_settings(|settings| settings.forgotten = true)
                .map(|_| json!({}))
                .map_err(|reason| error("internal", &reason)),
            "devices.list" => Ok(json!({ "devices": device_rows(&state).await })),
            "endpoints.register" | "endpoints.unregister" => {
                match message
                    .get("agent_dir")
                    .and_then(Value::as_str)
                    .filter(|dir| Path::new(dir).is_absolute())
                {
                    Some(dir) => {
                        let register = kind == "endpoints.register";
                        state
                            .update_settings(|settings| {
                                if register {
                                    settings.endpoints.insert(dir.to_owned());
                                } else {
                                    settings.endpoints.remove(dir);
                                }
                            })
                            .map(|_| json!({}))
                            .map_err(|reason| error("internal", &reason))
                    }
                    None => Err(error("invalid_request", "agent_dir")),
                }
            }
            "endpoints.list" => Ok(json!({ "agent_dirs": state.settings().endpoints })),
            "control.attach.get" => Ok(state.settings().control_view()),
            "control.attach.set" => match message.get("attached").and_then(Value::as_bool) {
                Some(attached) => state
                    .update_settings(|settings| {
                        settings.control = Some(crate::settings::Switch {
                            value: attached,
                            changed_at: now_rfc3339(),
                        });
                    })
                    .map(|settings| settings.control_view())
                    .map_err(|reason| error("internal", &reason)),
                None => Err(error("invalid_request", "attached")),
            },
            "control.session_wake.get" => Ok(state.settings().wake_view()),
            "control.session_wake.set" => match message.get("enabled") {
                Some(Value::Bool(_) | Value::Null) => {
                    let enabled = message.get("enabled").and_then(Value::as_bool);
                    state
                        .update_settings(|settings| {
                            settings.wake = Some(crate::settings::WakeSwitch {
                                value: enabled,
                                changed_at: now_rfc3339(),
                            });
                        })
                        .map(|settings| settings.wake_view())
                        .map_err(|reason| error("internal", &reason))
                }
                _ => Err(error("invalid_request", "enabled")),
            },
            "sessions.list" => return self.start_listing(id, message),
            "session.attach" => self.attach(message).await,
            "session.detach" => match message.get("attach").and_then(Value::as_str) {
                Some(attach) if self.attaches.remove(attach).is_some() => Ok(json!({})),
                Some(_) => Err(error("not_attached", "no such attach on this connection")),
                None => Err(error("invalid_request", "attach")),
            },
            "session.command" => return self.command(id, message),
            other if UNSUPPORTED_REQUESTS.contains(&other) => Err(error(
                "unsupported",
                &format!("{AGENTD_ONLY_REASON}: {other}"),
            )
            .with_action(Some(AGENTD_ONLY_ACTION.to_owned()))),
            _ => Err(error(
                "unknown_type",
                "this service does not define that request",
            )),
        };
        Some(answer)
    }

    fn follow_status(&mut self) {
        let mut statuses = self.state.status_receiver();
        let out = self.out.clone();
        let current = self.state.status();
        let _ = out.send(event_message(
            "status.changed",
            json!({ "status": current }),
        ));
        self.tasks.push(tokio::spawn(async move {
            while statuses.changed().await.is_ok() {
                let status = statuses.borrow_and_update().clone();
                if out
                    .send(event_message("status.changed", json!({ "status": status })))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn start_listing(&mut self, id: &str, message: &Value) -> Option<Result<Value, LocalApiError>> {
        let filter = match message.get("filter") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(filter))
                if filter.iter().all(|(key, value)| {
                    ["device", "project", "state"].contains(&key.as_str()) && value.is_string()
                }) =>
            {
                filter.clone()
            }
            Some(_) => return Some(Err(error("invalid_request", "filter"))),
        };
        let rows = match message.get("rows") {
            None | Some(Value::Null) => 1,
            Some(value) => match value.as_u64() {
                Some(rows @ 2..=4) => rows,
                _ => return Some(Err(error("invalid_request", "rows"))),
            },
        };
        let Ok(query_id) = mint_id() else {
            return Some(Err(error("internal", "no query id")));
        };
        self.reply(id, Ok(json!({ "query_id": query_id })));
        let state = Arc::clone(&self.state);
        let out = self.out.clone();
        self.tasks.push(tokio::spawn(run_listing(
            state, out, query_id, filter, rows,
        )));
        None
    }

    async fn attach(&mut self, message: &Value) -> Result<Value, LocalApiError> {
        let (Some(device), Some(session_id)) = (
            message.get("device").and_then(Value::as_str),
            message.get("session_id").and_then(Value::as_str),
        ) else {
            return Err(error("invalid_request", "device and session_id"));
        };
        let state = Arc::clone(&self.state);
        let this = state.identity.id();
        let sessions = if device == this {
            state
                .host
                .list_sessions(&state.agent_dirs())
                .await
                .map_err(|reason| error("host_not_answering", &reason))?
        } else {
            let peer = state
                .find_peer(device)
                .ok_or_else(|| error("peer_not_found", "the account has no such device"))?;
            remote_sessions(&state, &peer).await?
        };
        let device = if device == this {
            this
        } else {
            state
                .find_peer(device)
                .and_then(|peer| peer.device_id())
                .unwrap_or_else(|| device.to_owned())
        };
        let session = sessions
            .iter()
            .find(|session| session.is_named(session_id))
            .ok_or_else(|| error("session_not_found", "that device lists no such session"))?;
        let handle = mint_id()?;
        self.attaches
            .insert(handle.clone(), (device, session.session_id.clone()));
        Ok(json!({ "attach": handle }))
    }

    fn command(&mut self, id: &str, message: &Value) -> Option<Result<Value, LocalApiError>> {
        let Some(attach) = message.get("attach").and_then(Value::as_str) else {
            return Some(Err(error("invalid_request", "attach")));
        };
        let Some((device, session_id)) = self.attaches.get(attach).cloned() else {
            return Some(Err(error(
                "not_attached",
                "no such attach on this connection",
            )));
        };
        if message.get("upload_id").is_some()
            || message.get("endpoint").is_some()
            || message.get("delegated").and_then(Value::as_bool) == Some(true)
        {
            return Some(Err(error(
                "unsupported",
                "uploads and delegated commands are not served by mhc-relayd",
            )));
        }
        let Some(rpc) = message.get("rpc").filter(|rpc| rpc.is_object()).cloned() else {
            return Some(Err(error("invalid_request", "rpc")));
        };
        let Some(method) = rpc.get("type").and_then(Value::as_str) else {
            return Some(Err(error("malformed", "the record names no method")));
        };
        if LOCAL_ONLY_METHODS.contains(&method) {
            return Some(Err(error(
                "method_local_only",
                "that method configures a local connection",
            )));
        }
        if !SESSION_METHODS.contains(&method) {
            return Some(Err(error(
                "method_unclassified",
                "that method is not one a command may carry",
            )));
        }
        if !command_text_fits(&rpc.to_string()) {
            let mut refusal = error("message_too_long", "the record is over the engine's limit");
            refusal.limit_chars = Some(COMMAND_TEXT_MAX_CHARS as u64);
            return Some(Err(refusal));
        }
        let command_id = match message.get("command_id").and_then(Value::as_str) {
            Some(command_id) if valid_command_id(command_id) => command_id.to_owned(),
            Some(_) => return Some(Err(error("invalid_request", "command_id"))),
            None => match mint_id() {
                Ok(command_id) => command_id,
                Err(refusal) => return Some(Err(refusal)),
            },
        };
        let target = format!("{device}/{session_id}");
        let admission = self
            .state
            .ledger()
            .admit(&format!("local/{command_id}"), &target, &rpc);
        match admission {
            Admission::Conflict => {
                return Some(Err(error(
                    "command_id_conflict",
                    "this command_id is bound to another target or payload",
                )));
            }
            Admission::Repeat(outcome) => {
                self.reply(id, Ok(json!({ "command_id": command_id })));
                if let Some(outcome) = outcome {
                    let _ = self.out.send(command_event(&command_id, outcome));
                }
                return None;
            }
            Admission::New => {}
        }
        self.reply(id, Ok(json!({ "command_id": command_id })));
        let state = Arc::clone(&self.state);
        let out = self.out.clone();
        self.tasks.push(tokio::spawn(async move {
            let outcome = run_command(&state, &out, &command_id, &device, &session_id, &rpc).await;
            state
                .ledger()
                .settle(&format!("local/{command_id}"), outcome.clone());
            let _ = out.send(command_event(&command_id, outcome));
        }));
        None
    }
}

fn command_event(command_id: &str, outcome: Value) -> Value {
    let mut fields = match outcome {
        Value::Object(fields) => fields,
        _ => Map::new(),
    };
    fields.insert("command_id".into(), Value::String(command_id.to_owned()));
    if !fields.contains_key("retryable") {
        fields.insert("retryable".into(), Value::Bool(false));
    }
    event_message("session.command_state", Value::Object(fields))
}

async fn run_command(
    state: &RelaydState,
    out: &Outbox,
    command_id: &str,
    device: &str,
    session_id: &str,
    rpc: &Value,
) -> Value {
    if device == state.identity.id() {
        let _ = out.send(command_event(
            command_id,
            json!({ "state": "engine_dispatched" }),
        ));
        return match tokio::time::timeout(
            COMMAND_TIMEOUT,
            state.host.command(&state.agent_dirs(), session_id, rpc),
        )
        .await
        {
            Ok(outcome) => command_state(outcome),
            Err(_) => json!({ "state": "uncertain", "reason": "no_receipt" }),
        };
    }
    if !state.serving() {
        return json!({ "state": "not delivered: never_sent", "retryable": true });
    }
    let Some(peer) = state.find_peer(device) else {
        return json!({ "state": "not delivered: never_sent", "retryable": true });
    };
    let params = json!({ "session_id": session_id, "command_id": command_id, "rpc": rpc });
    match peer_request(state, &peer, "command", params, COMMAND_TIMEOUT).await {
        Ok(outcome) => {
            let _ = out.send(command_event(
                command_id,
                json!({ "state": "transport_confirmed" }),
            ));
            outcome
        }
        Err(PeerError::Unreachable(_) | PeerError::Refused(_)) => {
            json!({ "state": "not delivered: never_sent", "retryable": true })
        }
        Err(PeerError::Failed { code, .. }) => {
            let reason = match code.as_str() {
                "forbidden" | "service_disabled" => "peer_unauthorized",
                "command_id_conflict" => "parts_conflict",
                _ => "engine_capability_missing",
            };
            json!({ "state": "engine_refused", "reason": reason })
        }
        Err(PeerError::Lost(_)) => json!({ "state": "uncertain", "reason": "no_receipt" }),
    }
}

async fn remote_sessions(
    state: &RelaydState,
    peer: &PeerConfig,
) -> Result<Vec<HostSession>, LocalApiError> {
    if !state.serving() {
        return Err(error(
            "service_disabled",
            "remote sessions are off on this device",
        ));
    }
    match peer_request(state, peer, "sessions", json!({}), LIST_TIMEOUT).await {
        Ok(data) => Ok(parse_sessions(&data)),
        Err(PeerError::Failed { code, reason }) if code == "forbidden" => {
            Err(error("forbidden", &reason))
        }
        Err(other) => Err(error("host_not_answering", &other.reason())),
    }
}

fn parse_sessions(data: &Value) -> Vec<HostSession> {
    data.get("sessions")
        .and_then(Value::as_array)
        .map(|sessions| {
            sessions
                .iter()
                .filter_map(|session| serde_json::from_value(session.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

async fn device_rows(state: &Arc<RelaydState>) -> Vec<Value> {
    let serving = state.serving();
    let mut rows = vec![json!({
        "device": state.identity.id(),
        "label": state.identity.name(),
        "kind": "cli",
        "platform": platform(),
        "last_seen_ms": now_ms(),
        "online": serving,
        "this_device": true,
    })];
    let mut probes = JoinSet::new();
    if serving {
        for peer in state.config.peers.clone() {
            let state = Arc::clone(state);
            probes.spawn(async move {
                let answer = tokio::time::timeout(
                    PROBE_TIMEOUT,
                    peer_request(&state, &peer, "describe", json!({}), PROBE_TIMEOUT),
                )
                .await;
                (peer.name.clone(), answer.ok().and_then(Result::ok))
            });
        }
    }
    let mut described: HashMap<String, Value> = HashMap::new();
    while let Some(joined) = probes.join_next().await {
        if let Ok((name, Some(answer))) = joined {
            described.insert(name, answer);
        }
    }
    for peer in &state.config.peers {
        let Some(device_id) = peer.device_id() else {
            continue;
        };
        let answer = described.get(&peer.name);
        rows.push(json!({
            "device": device_id,
            "label": answer
                .and_then(|answer| answer["device"]["name"].as_str())
                .unwrap_or(peer.name.as_str()),
            "kind": "cli",
            "platform": answer
                .and_then(|answer| answer["device"]["platform"].as_str())
                .unwrap_or("unknown"),
            "last_seen_ms": state.peer_last_seen(&device_id),
            "online": answer.is_some(),
            "this_device": false,
        }));
    }
    rows
}

fn listing_row(device: &str, session: &HostSession, rows: u64, as_of: i64) -> Value {
    let mut row = json!({
        "device": device,
        "session_id": session.session_id,
        "title": session.title.clone().unwrap_or_default(),
        "project": session.project.clone().unwrap_or_default(),
        "path": session.path.clone().unwrap_or_default(),
        "state": session.state,
        "updated_ms": session.updated_ms.unwrap_or(as_of),
        "source": "live",
        "as_of": as_of,
    });
    if rows >= 2
        && let Some(kind) = &session.kind
    {
        row["kind"] = Value::String(kind.clone());
    }
    if rows >= 3
        && let Some(durable_id) = &session.durable_id
    {
        row["durable_id"] = Value::String(durable_id.clone());
    }
    if rows >= 4
        && let Some(model) = &session.model
    {
        row["model"] = model.clone();
    }
    row
}

fn matches_filter(row: &Value, filter: &Map<String, Value>) -> bool {
    filter
        .iter()
        .all(|(key, value)| row.get(key) == Some(value))
}

/// `listing_begin`, pages of rows each under `LOCAL_INLINE_MAX`, and `listing_end`.
fn emit_revision(
    out: &Outbox,
    query_id: &str,
    revision: u64,
    rows: &[Value],
    incomplete: &[String],
) {
    let _ = out.send(event_message(
        "sessions.listing_begin",
        json!({ "query_id": query_id, "revision": revision }),
    ));
    let mut page: Vec<Value> = Vec::new();
    for row in rows {
        page.push(row.clone());
        let candidate = event_message(
            "sessions.listing_rows",
            json!({ "query_id": query_id, "revision": revision, "rows": page }),
        );
        if line_bytes(&candidate) > LOCAL_INLINE_MAX && page.len() > 1 {
            let last = page.pop();
            let _ = out.send(event_message(
                "sessions.listing_rows",
                json!({ "query_id": query_id, "revision": revision, "rows": page }),
            ));
            page = last.into_iter().collect();
        }
    }
    if !page.is_empty() {
        let _ = out.send(event_message(
            "sessions.listing_rows",
            json!({ "query_id": query_id, "revision": revision, "rows": page }),
        ));
    }
    let _ = out.send(event_message(
        "sessions.listing_end",
        json!({ "query_id": query_id, "revision": revision, "incomplete": incomplete }),
    ));
}

/// One unfiltered round across this device and its peers, published to this query as revisions:
/// this device's rows at once, then every answer.
async fn run_listing(
    state: Arc<RelaydState>,
    out: Outbox,
    query_id: String,
    filter: Map<String, Value>,
    rows: u64,
) {
    let this = state.identity.id();
    let peers: Vec<(String, PeerConfig)> = state
        .config
        .peers
        .iter()
        .filter_map(|peer| peer.device_id().map(|id| (id, peer.clone())))
        .collect();
    let mut devices = Map::new();
    devices.insert(this.clone(), Value::from("pending"));
    for (id, _) in &peers {
        devices.insert(id.clone(), Value::from("pending"));
    }
    let _ = out.send(event_message(
        "sessions.round",
        json!({ "query_id": query_id, "state": "open", "devices": devices }),
    ));
    let as_of = now_ms();
    let mut all = Vec::new();
    match tokio::time::timeout(LIST_TIMEOUT, state.host.list_sessions(&state.agent_dirs())).await {
        Ok(Ok(sessions)) => {
            all.extend(
                sessions
                    .iter()
                    .map(|session| listing_row(&this, session, rows, as_of)),
            );
            devices.insert(this.clone(), Value::from("answered"));
        }
        _ => {
            devices.insert(this.clone(), Value::from("timed_out"));
        }
    }
    let filtered = |all: &[Value]| -> Vec<Value> {
        all.iter()
            .filter(|row| matches_filter(row, &filter))
            .cloned()
            .collect()
    };
    let pending: Vec<String> = peers.iter().map(|(id, _)| id.clone()).collect();
    emit_revision(&out, &query_id, 1, &filtered(&all), &pending);
    if !peers.is_empty() {
        let mut incomplete = Vec::new();
        if state.serving() {
            let mut answers = JoinSet::new();
            for (id, peer) in peers {
                let state = Arc::clone(&state);
                answers.spawn(async move {
                    let answer = tokio::time::timeout(
                        LIST_TIMEOUT,
                        peer_request(&state, &peer, "sessions", json!({}), LIST_TIMEOUT),
                    )
                    .await;
                    (id, answer)
                });
            }
            while let Some(joined) = answers.join_next().await {
                let Ok((id, answer)) = joined else {
                    continue;
                };
                let device_state = match answer {
                    Ok(Ok(data)) => {
                        let as_of = now_ms();
                        all.extend(
                            parse_sessions(&data)
                                .iter()
                                .map(|session| listing_row(&id, session, rows, as_of)),
                        );
                        "answered"
                    }
                    Ok(Err(PeerError::Lost(_))) | Err(_) => "timed_out",
                    Ok(Err(_)) => "undelivered",
                };
                if device_state != "answered" {
                    incomplete.push(id.clone());
                }
                devices.insert(id, Value::from(device_state));
            }
        } else {
            for (id, _) in &peers {
                devices.insert(id.clone(), Value::from("skipped"));
                incomplete.push(id.clone());
            }
        }
        incomplete.sort();
        emit_revision(&out, &query_id, 2, &filtered(&all), &incomplete);
    }
    let _ = out.send(event_message(
        "sessions.round",
        json!({ "query_id": query_id, "state": "done", "devices": devices }),
    ));
}
