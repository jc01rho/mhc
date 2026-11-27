//! The account surface over agentd: the sessions port, the device client, the session host and the follow stream.
//!
//! Every method here is one op of the agentd contract (`mhc.sessions.list`, `mhc.session.locate`, `mhc.session.read`,
//! `mhc.session.open`, `mhc.session.command`, `mhc.session.follow`), so a device with an agentd entry answers the
//! same questions mhc-relayd answered, and a device without one keeps the relayd path untouched.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::thread::adapter::BoxFuture;
use crate::thread::address_book::HostSessionStatus;
use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::contracts_results::ThreadResultSummary;
use crate::thread::control::port::{
    ControlError, ControlResult, SessionHost, SessionOpenOptions, SessionOpenRequest, refuse,
};
use crate::thread::control::session_host::{OPEN_SESSION_FORK, opened_thread};
use crate::thread::host_view::ThreadHostSession;
use crate::thread::remote::address_book::relay_address_entry;
use crate::thread::remote::attach_pool::{AttachTransport, AttachedClient};
use crate::thread::remote::ports::{
    ClientDeviceRow, RemoteClient, RemoteCommandOutcome, RemoteDevice, RemoteFailure,
    RemoteListing, RemoteLocateOutcome, RemoteLookupOutcome, RemoteSessionRow, RemoteSessionsPort,
    RowSource, ServiceStatus, SessionCommandOptions, iso_from_ms,
};
use crate::thread::thread_state::thread_model;
use crate::thread::types::GatewayEndpointKind;

use super::frame::EventFrame;
use super::link::{AgentdError, AgentdLink, DEFAULT_CALL_TIMEOUT, now_ms};
use super::peer_config::{AgentdPeer, peer_map};

/// A record at or under this size travels as one `cmd`; a bigger one is split into parts.
const ONE_FRAME_BYTES: usize = 48 * 1024;

/// The listing key a row is reached by: its durable id when the device publishes one, else its session id.
pub fn row_key_of(row: &RemoteSessionRow) -> &str {
    row.durable_id.as_deref().unwrap_or(&row.session_id)
}

/// One `mhc.sessions.list` answer's rows, as this device's listing carries them.
pub fn rows_of(device: &str, data: &Value) -> Vec<RemoteSessionRow> {
    let Some(sessions) = data.get("sessions").and_then(Value::as_array) else {
        return Vec::new();
    };
    sessions
        .iter()
        .filter_map(|session| row_of(device, session))
        .collect()
}

fn row_of(device: &str, session: &Value) -> Option<RemoteSessionRow> {
    let text = |key: &str| session.get(key).and_then(Value::as_str).map(str::to_owned);
    let session_id = text("session_id")?;
    Some(RemoteSessionRow {
        device: device.to_owned(),
        session_id,
        durable_id: text("durable_id"),
        title: text("title").unwrap_or_default(),
        project: text("project").unwrap_or_default(),
        path: text("path").unwrap_or_default(),
        state: text("state").unwrap_or_else(|| "idle".to_owned()),
        updated_ms: session
            .get("updated_ms")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        source: Some(RowSource::Live),
        model: thread_model(session.get("model")),
        archived: session
            .get("archived")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        waiting: session
            .get("waiting")
            .filter(|waiting| !waiting.is_null())
            .cloned(),
        engine_version: text("engine_version"),
        ..RemoteSessionRow::default()
    })
}

/// A daemon's error as the command outcome it names, the way the relay answered the same three states.
pub fn outcome_of(error: &AgentdError) -> RemoteCommandOutcome {
    match error {
        AgentdError::Refused { reason, detail } => RemoteCommandOutcome::Refused {
            reason: reason.clone(),
            detail: detail.clone(),
        },
        AgentdError::Unsupported(op) => RemoteCommandOutcome::Refused {
            reason: "unsupported".to_owned(),
            detail: Some(op.clone()),
        },
        AgentdError::Expired => RemoteCommandOutcome::Uncertain {
            reason: "expired".to_owned(),
        },
        AgentdError::Timeout => RemoteCommandOutcome::Uncertain {
            reason: "timeout".to_owned(),
        },
        other => RemoteCommandOutcome::Refused {
            reason: "daemon_unavailable".to_owned(),
            detail: Some(other.describe()),
        },
    }
}

/// A `mhc.session.command` answer as the relay's command outcome.
pub fn command_outcome(data: &Value) -> RemoteCommandOutcome {
    let state = data
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reason = data
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match state {
        "engine_applied" => RemoteCommandOutcome::Ok {
            response: data.clone(),
            local: false,
            located: None,
        },
        "engine_refused" => RemoteCommandOutcome::Refused {
            reason: reason.unwrap_or_else(|| "engine_refused".to_owned()),
            detail: None,
        },
        _ => RemoteCommandOutcome::Uncertain {
            reason: reason.unwrap_or_else(|| "uncertain".to_owned()),
        },
    }
}

/// A `command_id` of 22 base64url characters, unique to this process and this call: the daemon keys its dedup record
/// by (sender node, command id), so a resend of the same call finds the result instead of running it twice.
pub fn command_id(counter: u64) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or_default();
    let mut seed = nanos ^ (u64::from(std::process::id()) << 32) ^ counter.rotate_left(17);
    let mut id = String::with_capacity(22);
    for _ in 0..22 {
        id.push(ALPHABET[(seed & 0x3f) as usize] as char);
        seed = seed.rotate_right(7) ^ (seed >> 11).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    id
}

/// This device's view of the account's agentd peers, and one connection per peer opened on first use.
pub struct AgentdClient {
    node_name: String,
    peers: BTreeMap<String, AgentdPeer>,
    links: Mutex<BTreeMap<String, Arc<AgentdLink>>>,
    /// The solo locate gate (#14748): one at a time, and every other lookup waits for it.
    solo: AsyncMutex<()>,
    counter: std::sync::atomic::AtomicU64,
}

impl AgentdClient {
    /// The peers named in the state dir's `relayd.json`; a device without an `agentd` object is not one of them.
    pub fn new(state_dir: &std::path::Path) -> Self {
        let node_name = format!(
            "mhc-{}-{}",
            std::env::var("HOSTNAME").unwrap_or_else(|_| "host".to_owned()),
            std::process::id()
        );
        Self::with_peers(node_name, peer_map(state_dir).into_values().collect())
    }

    /// A client with the peers a caller names, for a test or a caller that reads its config elsewhere.
    pub fn with_peers(node_name: impl Into<String>, peers: Vec<AgentdPeer>) -> Self {
        Self {
            node_name: node_name.into(),
            peers: peers
                .into_iter()
                .map(|peer| (peer.device.clone(), peer))
                .collect(),
            links: Mutex::new(BTreeMap::new()),
            solo: AsyncMutex::new(()),
            counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Whether any device is configured to be reached over agentd at all.
    pub fn configured(&self) -> bool {
        !self.peers.is_empty()
    }

    /// The devices this client may reach, by name.
    pub fn devices_configured(&self) -> Vec<String> {
        self.peers.keys().cloned().collect()
    }

    /// Whether this device has an agentd entry for `device`.
    pub fn reaches(&self, device: &str) -> bool {
        self.peers.contains_key(device)
    }

    fn links(&self) -> MutexGuard<'_, BTreeMap<String, Arc<AgentdLink>>> {
        self.links.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The link to one device, opened on first use; `None` when this device has no agentd entry.
    pub fn link(&self, device: &str) -> Option<Arc<AgentdLink>> {
        if let Some(link) = self.links().get(device) {
            return Some(Arc::clone(link));
        }
        let peer = self.peers.get(device)?.clone();
        let opened = AgentdLink::new(peer, self.node_name.clone());
        let mut links = self.links();
        // Another caller may have opened it while this one waited for the lock.
        Some(Arc::clone(
            links
                .entry(device.to_owned())
                .or_insert_with(|| Arc::clone(&opened)),
        ))
    }

    /// Whether a device is reachable right now, read from its link (no round trip).
    pub fn connected(&self, device: &str) -> bool {
        self.link(device).is_some_and(|link| link.connected())
    }

    fn next_command_id(&self) -> String {
        command_id(
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// One op on one device's daemon.
    pub async fn call(
        &self,
        device: &str,
        op: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, AgentdError> {
        let Some(link) = self.link(device) else {
            return Err(AgentdError::Unavailable(format!(
                "device {device} has no agentd entry"
            )));
        };
        link.call(op, args, timeout).await
    }

    /// One session command, split into parts when the record is bigger than one frame.
    pub async fn send_command(
        &self,
        device: &str,
        session_id: &str,
        rpc: &Value,
        timeout: Duration,
    ) -> Result<Value, AgentdError> {
        let id = self.next_command_id();
        let record = serde_json::to_string(rpc).unwrap_or_default();
        if record.len() <= ONE_FRAME_BYTES {
            let args = json!({
                "session": session_id,
                "command_id": id,
                "rpc": rpc,
            });
            return self
                .call(device, "mhc.session.command", args, timeout)
                .await;
        }
        // The daemon assembles the parts of one id and runs the record once, on the last part.
        let chunks: Vec<&[u8]> = record.as_bytes().chunks(ONE_FRAME_BYTES).collect();
        let total = chunks.len();
        let mut answer = Value::Null;
        for (index, chunk) in chunks.iter().enumerate() {
            let text = String::from_utf8_lossy(chunk).into_owned();
            let args = json!({
                "session": session_id,
                "command_id": id,
                "rpc": Value::String(text),
                "part": { "index": index, "total": total, "id": id },
            });
            answer = self
                .call(device, "mhc.session.command", args, timeout)
                .await?;
        }
        Ok(answer)
    }

    /// The `capabilities` a device's daemon advertised in its last `mhc.sessions.list` answer.
    pub async fn capabilities(&self, device: &str) -> Vec<String> {
        match self
            .call(device, "mhc.sessions.list", json!({}), DEFAULT_CALL_TIMEOUT)
            .await
        {
            Ok(data) => data
                .get("capabilities")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// The account's session list: every configured device, or the one named. A device that did not answer is listed as
    /// incomplete, never as empty.
    pub async fn listing(&self, device: Option<String>) -> Result<RemoteListing, String> {
        let devices = match device {
            Some(device) => vec![device],
            None => self.devices_configured(),
        };
        let mut rows = Vec::new();
        let mut incomplete = Vec::new();
        let mut states = BTreeMap::new();
        for device in devices {
            if !self.peers.contains_key(&device) {
                incomplete.push(device.clone());
                states.insert(device, "unconfigured".to_owned());
                continue;
            }
            match self
                .call(
                    &device,
                    "mhc.sessions.list",
                    json!({}),
                    DEFAULT_CALL_TIMEOUT,
                )
                .await
            {
                Ok(data) => {
                    rows.extend(rows_of(&device, &data));
                    states.insert(device, "answered".to_owned());
                }
                Err(_) => {
                    states.insert(device.clone(), "silent".to_owned());
                    incomplete.push(device);
                }
            }
        }
        Ok(RemoteListing {
            rows,
            incomplete,
            devices: Some(states),
        })
    }

    async fn lookup_on(
        &self,
        device: &str,
        durable_id: &str,
        timeout: Duration,
    ) -> RemoteLookupOutcome {
        let args = json!({ "session": durable_id });
        match self.call(device, "mhc.session.locate", args, timeout).await {
            Ok(data) if data.get("found").and_then(Value::as_bool) == Some(true) => {
                let key = data
                    .get("row")
                    .and_then(|row| row.get("session_id"))
                    .and_then(Value::as_str)
                    .unwrap_or(durable_id)
                    .to_owned();
                RemoteLookupOutcome::Found { key }
            }
            // A device that matched more than one session named none: that is not proof it holds nothing.
            Ok(data) if data.get("ambiguous").and_then(Value::as_bool) == Some(true) => {
                RemoteLookupOutcome::Silent {
                    reason: "ambiguous".to_owned(),
                }
            }
            Ok(_) => RemoteLookupOutcome::NotHeld,
            Err(AgentdError::Refused { reason, .. }) if reason == "not_found" => {
                RemoteLookupOutcome::NotHeld
            }
            Err(error) => RemoteLookupOutcome::Silent {
                reason: reason_of(&error),
            },
        }
    }
}

/// A daemon's error as the reason a refusal names.
pub fn reason_of(error: &AgentdError) -> String {
    match error {
        AgentdError::Refused { reason, .. } => reason.clone(),
        AgentdError::Unsupported(_) => "unsupported".to_owned(),
        AgentdError::Expired => "expired".to_owned(),
        _ => "daemon_unavailable".to_owned(),
    }
}

/// The account surface over agentd, ready to be the gateway's `RemoteThreadSurface`.
#[derive(Clone)]
pub struct AgentdPorts {
    client: Arc<AgentdClient>,
}

impl AgentdPorts {
    pub fn new(client: Arc<AgentdClient>) -> Self {
        Self { client }
    }

    pub fn client(&self) -> Arc<AgentdClient> {
        Arc::clone(&self.client)
    }
}

impl RemoteSessionsPort for AgentdPorts {
    fn devices(&self) -> BoxFuture<Result<Vec<RemoteDevice>, String>> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            Ok(client
                .devices_configured()
                .into_iter()
                .map(|device| RemoteDevice {
                    online: client.connected(&device),
                    label: device.clone(),
                    device,
                    ..RemoteDevice::default()
                })
                .collect())
        })
    }

    fn sessions(&self, device: Option<String>) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
        let client = Arc::clone(&self.client);
        Box::pin(async move { Ok(client.listing(device).await?.rows) })
    }

    fn listing(&self, device: Option<String>) -> BoxFuture<Result<RemoteListing, String>> {
        let client = Arc::clone(&self.client);
        Box::pin(async move { client.listing(device).await })
    }

    fn locate(&self, device: String, engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let args = json!({ "session": engine_id });
            match client
                .call(&device, "mhc.session.locate", args, DEFAULT_CALL_TIMEOUT)
                .await
            {
                Ok(data) if data.get("found").and_then(Value::as_bool) == Some(true) => {
                    let session_id = data
                        .get("row")
                        .and_then(|row| row.get("session_id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or(engine_id);
                    RemoteLocateOutcome::Located { session_id }
                }
                Ok(_) => RemoteLocateOutcome::Refused {
                    reason: "not_found".to_owned(),
                },
                Err(error) => RemoteLocateOutcome::Refused {
                    reason: reason_of(&error),
                },
            }
        })
    }

    fn lookup(&self, device: String, durable_id: String) -> BoxFuture<RemoteLookupOutcome> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            // Every other lookup waits for a solo locate to end (#14748).
            let _solo = client.solo.lock().await;
            client
                .lookup_on(&device, &durable_id, DEFAULT_CALL_TIMEOUT)
                .await
        })
    }

    fn lookup_alone(
        &self,
        device: String,
        durable_id: String,
        timeout_ms: i64,
    ) -> BoxFuture<Option<RemoteLookupOutcome>> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let Ok(_solo) = client.solo.try_lock() else {
                return None;
            };
            let timeout = Duration::from_millis(u64::try_from(timeout_ms.max(1)).unwrap_or(1));
            Some(client.lookup_on(&device, &durable_id, timeout).await)
        })
    }

    fn can_lookup(&self) -> BoxFuture<bool> {
        let client = Arc::clone(&self.client);
        Box::pin(async move { client.configured() })
    }

    fn can_wake(&self) -> BoxFuture<bool> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            // A wake reaches a device that answers now: a daemon that is not connected cannot take one.
            client
                .devices_configured()
                .iter()
                .any(|device| client.connected(device))
        })
    }

    fn session_command(
        &self,
        device: String,
        session_id: String,
        rpc: Value,
        _options: SessionCommandOptions,
    ) -> BoxFuture<RemoteCommandOutcome> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            match client
                .send_command(&device, &session_id, &rpc, DEFAULT_CALL_TIMEOUT)
                .await
            {
                Ok(data) => command_outcome(&data),
                Err(error) => outcome_of(&error),
            }
        })
    }
}

impl RemoteClient for AgentdPorts {
    fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            Ok(client
                .devices_configured()
                .into_iter()
                .map(|device| ClientDeviceRow {
                    online: client.connected(&device),
                    name: device.clone(),
                    id: device,
                    is_self: false,
                    last_seen_ms: None,
                })
                .collect())
        })
    }

    fn service_status(&self) -> BoxFuture<Result<ServiceStatus, RemoteFailure>> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let connected = client
                .devices_configured()
                .iter()
                .any(|device| client.connected(device));
            Ok(ServiceStatus {
                state: if connected {
                    "connected"
                } else {
                    "disconnected"
                }
                .to_owned(),
                device_id: None,
            })
        })
    }
}

/// The device's own engine, asked to open, reopen or fork a session (the `RelaySessionHost` leg of the contract).
pub struct AgentdSessionHost {
    client: Arc<AgentdClient>,
    device: String,
}

impl AgentdSessionHost {
    pub fn new(client: Arc<AgentdClient>, device: impl Into<String>) -> Self {
        Self {
            client,
            device: device.into(),
        }
    }

    pub fn device_name(&self) -> &str {
        &self.device
    }

    /// Whether the daemon advertises `open_session_fork`, read from its listing answer.
    pub async fn can_fork(&self) -> bool {
        self.client
            .capabilities(&self.device)
            .await
            .iter()
            .any(|capability| capability == "open_session_fork")
    }

    fn refusal(&self, error: &AgentdError) -> ControlError {
        let device = self.device.clone();
        match error {
            AgentdError::Refused { reason, detail } if reason == "fork_source_not_found" => refuse(
                "not_found",
                format!("Device {device} holds no session to fork from; nothing was opened."),
                "Fork from a session that device lists, or create the thread without fork_from.",
                Some(json!({ "reason": reason, "device": device, "detail": detail })),
            ),
            AgentdError::Refused { reason, .. } if reason == "fork_source_not_local" => refuse(
                "not_found",
                format!("The session to fork is not on device {device}; nothing was opened."),
                "Fork it on the device that holds it.",
                Some(json!({ "reason": reason, "device": device })),
            ),
            AgentdError::Refused { reason, .. } if reason == "session_busy" => refuse(
                "turn_active",
                format!("Device {device} is running a turn on that session, so it was not opened."),
                "Retry when the session is idle, or create a new session instead.",
                Some(json!({ "reason": reason, "device": device })),
            ),
            AgentdError::Refused { reason, .. } if reason == "session_cap" => refuse(
                "overloaded",
                format!("Device {device} holds as many sessions as it can, so nothing was opened."),
                "Close a session on that device, then retry.",
                Some(json!({ "reason": reason, "device": device })),
            ),
            AgentdError::Unsupported(op) => refuse(
                "unsupported",
                format!("Device {device}'s daemon does not serve {op}, so nothing was opened."),
                "Update the daemon on that device, or open the thread there.",
                Some(json!({ "device": device, "reason": "unsupported", "op": op })),
            ),
            other => ControlError::Transport(other.describe()),
        }
    }
}

/// The engine's `open_session` fields an open carries.
pub fn launch_of(request: &SessionOpenRequest) -> Value {
    let mut launch = Map::new();
    if let Some(cwd) = &request.cwd {
        launch.insert("cwd".to_owned(), json!(cwd));
    }
    if let Some(path) = &request.session_path {
        launch.insert("sessionPath".to_owned(), json!(path));
    }
    if let Some(fork_from) = &request.fork_from {
        launch.insert("forkFrom".to_owned(), json!(fork_from));
    }
    if let Some(name) = &request.name {
        launch.insert("name".to_owned(), json!(name));
    }
    if let Some(preset) = &request.permission_preset {
        launch.insert("permissionPreset".to_owned(), json!(preset));
    }
    if let Some(provider) = &request.provider {
        launch.insert("provider".to_owned(), json!(provider));
    }
    if let Some(model) = &request.model_id {
        launch.insert("modelId".to_owned(), json!(model));
    }
    if let Some(level) = &request.thinking_level {
        launch.insert("thinkingLevel".to_owned(), json!(level));
    }
    if let Some(durable) = &request.durable_session_id {
        launch.insert("durableSessionId".to_owned(), json!(durable));
    }
    if !request.initial_skills.is_empty() {
        launch.insert("initialSkills".to_owned(), json!(request.initial_skills));
    }
    Value::Object(launch)
}

/// The session an open answer names, as this device reports a thread.
pub fn opened_summary(
    device: &str,
    data: &Value,
    request: &SessionOpenRequest,
) -> Option<ThreadResultSummary> {
    let session_id = data.get("session_id").and_then(Value::as_str)?.to_owned();
    let session = ThreadHostSession {
        session_id,
        durable_session_id: data
            .get("durable_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        session_path: request.session_path.clone(),
        cwd: request.cwd.clone().unwrap_or_default(),
        name: request.name.clone(),
        status: Some(HostSessionStatus::Open),
        kind: None,
        archived: false,
        activity: None,
        error_code: None,
        turn_id: None,
        model: None,
        waiting: None,
        engine_version: None,
        created_at: None,
        updated_at: None,
        endpoint_kind: GatewayEndpointKind::RpcHost,
        controls: Vec::new(),
        socket: String::new(),
    };
    let now = iso_from_ms(now_ms());
    let mut row = opened_thread(&session, &now);
    row.device = Some(device.to_owned());
    Some(row)
}

impl SessionHost for AgentdSessionHost {
    fn device(&self) -> Option<&str> {
        Some(&self.device)
    }

    fn visible(&self) -> BoxFuture<ControlResult<Vec<ThreadAddressEntry>>> {
        let client = Arc::clone(&self.client);
        let device = self.device.clone();
        Box::pin(async move {
            match client.listing(Some(device.clone())).await {
                Ok(listing) => Ok(listing
                    .rows
                    .iter()
                    .map(|row| relay_address_entry(row, &[]))
                    .collect()),
                Err(error) => Err(refuse(
                    "host_unavailable",
                    format!("Device {device}'s threads could not be listed."),
                    "Retry when that device is reachable.",
                    Some(json!({ "device": device, "reason": error })),
                )),
            }
        })
    }

    fn forks(&self) -> BoxFuture<ControlResult<bool>> {
        let client = Arc::clone(&self.client);
        let device = self.device.clone();
        Box::pin(async move {
            Ok(client
                .capabilities(&device)
                .await
                .iter()
                .any(|name| name == OPEN_SESSION_FORK))
        })
    }

    fn open(
        &self,
        request: SessionOpenRequest,
        _options: SessionOpenOptions,
    ) -> BoxFuture<ControlResult<ThreadResultSummary>> {
        let client = Arc::clone(&self.client);
        let device = self.device.clone();
        let host = AgentdSessionHost {
            client: Arc::clone(&client),
            device: device.clone(),
        };
        Box::pin(async move {
            let mut args = Map::new();
            if let Some(session) = &request.remote_session {
                args.insert("session".to_owned(), json!(session));
            }
            if let Some(fork_from) = &request.fork_from {
                args.insert("fork_from".to_owned(), json!(fork_from));
            }
            args.insert("launch".to_owned(), launch_of(&request));
            let answer = client
                .call(
                    &device,
                    "mhc.session.open",
                    Value::Object(args),
                    DEFAULT_CALL_TIMEOUT,
                )
                .await;
            match answer {
                Ok(data) => opened_summary(&device, &data, &request).map_or_else(
                    || {
                        Err(ControlError::Transport(format!(
                            "device {device} opened a session but named no session id"
                        )))
                    },
                    Ok,
                ),
                Err(error) => Err(host.refusal(&error)),
            }
        })
    }
}

/// One open follow stream: its events, and the release the connection owes when this handle ends (#14091).
pub struct AgentdFollow {
    link: Arc<AgentdLink>,
    stream: String,
    events: Option<UnboundedReceiver<EventFrame>>,
}

impl AgentdFollow {
    pub fn stream(&self) -> &str {
        &self.stream
    }

    /// The next event of the stream, or `None` once the daemon's connection ended.
    pub async fn next_event(&mut self) -> Option<EventFrame> {
        self.events.as_mut()?.recv().await
    }

    /// Releases the stream on the connection and forgets its cursor: a closed thread is not resumed.
    pub fn release(&mut self) {
        self.events = None;
        self.link.release_stream(&self.stream);
    }
}

impl Drop for AgentdFollow {
    fn drop(&mut self) {
        self.events = None;
        self.link.release_stream(&self.stream);
    }
}

/// Opens follow streams on other devices (`mhc.session.follow`).
pub struct AgentdFollowPort {
    client: Arc<AgentdClient>,
}

impl AgentdFollowPort {
    pub fn new(client: Arc<AgentdClient>) -> Self {
        Self { client }
    }

    /// Subscribes to one session's events from `from_seq`, releasing the subscription when the handle drops.
    pub async fn follow(
        &self,
        device: &str,
        session_id: &str,
        from_seq: Option<u64>,
    ) -> Result<AgentdFollow, AgentdError> {
        let Some(link) = self.client.link(device) else {
            return Err(AgentdError::Unavailable(format!(
                "device {device} has no agentd entry"
            )));
        };
        let mut args = Map::new();
        args.insert("session".to_owned(), json!(session_id));
        if let Some(from_seq) = from_seq {
            args.insert("from_seq".to_owned(), json!(from_seq));
        }
        let data = link
            .call(
                "mhc.session.follow",
                Value::Object(args),
                DEFAULT_CALL_TIMEOUT,
            )
            .await?;
        let stream = data
            .get("stream")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("mhc-session:{session_id}"));
        let seq = data.get("seq").and_then(Value::as_u64).unwrap_or_default();
        let events = link.follow(&stream, seq);
        Ok(AgentdFollow {
            link,
            stream,
            events: Some(events),
        })
    }
}

/// The pooled attach of one remote session over agentd: its follow stream, and commands sent to its engine.
pub struct AgentdAttach {
    follow: Mutex<Option<AgentdFollow>>,
    client: Arc<AgentdClient>,
    device: String,
    session_id: String,
}

impl AgentdAttach {
    /// The stream this attach follows, while it is held.
    pub fn stream(&self) -> Option<String> {
        self.follow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|follow| follow.stream().to_owned())
    }
}

impl AttachedClient for AgentdAttach {
    fn command(
        &self,
        rpc: Value,
        _options: SessionCommandOptions,
    ) -> BoxFuture<RemoteCommandOutcome> {
        let client = Arc::clone(&self.client);
        let device = self.device.clone();
        let session_id = self.session_id.clone();
        Box::pin(async move {
            match client
                .send_command(&device, &session_id, &rpc, DEFAULT_CALL_TIMEOUT)
                .await
            {
                Ok(data) => command_outcome(&data),
                Err(error) => outcome_of(&error),
            }
        })
    }

    fn detach(&self) -> BoxFuture<()> {
        // Dropping the follow releases the subscription on this connection (#14091).
        let follow = self
            .follow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Box::pin(async move {
            drop(follow);
        })
    }
}

impl AttachTransport for AgentdFollowPort {
    fn attach(
        &self,
        device: &str,
        session_id: &str,
    ) -> BoxFuture<Result<Arc<dyn AttachedClient>, RemoteCommandOutcome>> {
        let port = AgentdFollowPort::new(Arc::clone(&self.client));
        let client = Arc::clone(&self.client);
        let device = device.to_owned();
        let session = session_id.to_owned();
        Box::pin(async move {
            match port.follow(&device, &session, None).await {
                Ok(follow) => Ok(Arc::new(AgentdAttach {
                    follow: Mutex::new(Some(follow)),
                    client,
                    device,
                    session_id: session,
                }) as Arc<dyn AttachedClient>),
                Err(error) => Err(outcome_of(&error)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(state: &str) -> Value {
        json!({
            "session_id": "rk-1",
            "durable_id": "01932bf3-0280-7abc-8def-0123456789ab",
            "title": "on b",
            "project": "/work/app",
            "path": "/work/app",
            "state": state,
            "updated_ms": 1_700_000_000_000i64,
            "model": { "provider": "openai", "id": "gpt-6" },
            "waiting": { "kind": "rate_limit", "until": "2026-11-02T00:01:00.000Z", "attempt": 2, "maxAttempts": 5 },
            "engine_version": "2026.11.1",
        })
    }

    #[test]
    fn a_listing_row_carries_the_holders_retry_wait_and_engine_version() {
        let rows = rows_of("b", &json!({ "sessions": [session("running")] }));
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.device, "b");
        assert_eq!(row.state, "running");
        assert_eq!(row.source, Some(RowSource::Live));
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
        assert_eq!(row_key_of(row), "01932bf3-0280-7abc-8def-0123456789ab");
    }

    #[test]
    fn a_row_without_a_durable_id_is_reached_by_its_listing_key() {
        let rows = rows_of(
            "b",
            &json!({ "sessions": [{ "session_id": "rk-9", "state": "idle" }] }),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(row_key_of(&rows[0]), "rk-9");
        assert!(rows[0].waiting.is_none());
        assert!(rows[0].engine_version.is_none());
    }

    #[test]
    fn a_listing_answer_without_rows_names_none() {
        assert!(rows_of("b", &json!({})).is_empty());
        assert!(rows_of("b", &json!({ "sessions": [{}] })).is_empty());
    }

    #[test]
    fn a_command_answer_reads_as_the_three_engine_states() {
        assert!(matches!(
            command_outcome(&json!({ "state": "engine_applied", "receipt": {} })),
            RemoteCommandOutcome::Ok { .. }
        ));
        assert_eq!(
            command_outcome(&json!({ "state": "engine_refused", "reason": "not_steerable" })),
            RemoteCommandOutcome::Refused {
                reason: "not_steerable".to_owned(),
                detail: None,
            }
        );
        assert_eq!(
            command_outcome(&json!({ "state": "uncertain", "reason": "request_gone" })),
            RemoteCommandOutcome::Uncertain {
                reason: "request_gone".to_owned(),
            }
        );
    }

    #[test]
    fn a_command_id_is_22_base64url_characters_and_never_repeats() {
        let first = command_id(1);
        let second = command_id(2);
        assert_eq!(first.len(), 22);
        assert_ne!(first, second);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn an_unsupported_op_and_a_lost_daemon_read_as_the_outcomes_a_caller_retries() {
        assert_eq!(
            outcome_of(&AgentdError::Unsupported("mhc.session.read".to_owned())),
            RemoteCommandOutcome::Refused {
                reason: "unsupported".to_owned(),
                detail: Some("mhc.session.read".to_owned()),
            }
        );
        assert_eq!(
            outcome_of(&AgentdError::Unavailable("gone".to_owned())),
            RemoteCommandOutcome::Refused {
                reason: "daemon_unavailable".to_owned(),
                detail: Some("agentd is unreachable: gone".to_owned()),
            }
        );
    }

    #[test]
    fn an_open_request_reaches_the_daemon_as_the_engines_own_field_names() {
        let launch = launch_of(&SessionOpenRequest {
            cwd: Some("/work/app".to_owned()),
            fork_from: Some("rk-1".to_owned()),
            name: Some("child".to_owned()),
            provider: Some("openai".to_owned()),
            model_id: Some("gpt-6".to_owned()),
            thinking_level: Some("high".to_owned()),
            ..SessionOpenRequest::default()
        });
        assert_eq!(launch["cwd"], json!("/work/app"));
        assert_eq!(launch["forkFrom"], json!("rk-1"));
        assert_eq!(launch["modelId"], json!("gpt-6"));
        assert_eq!(launch["thinkingLevel"], json!("high"));
        assert!(launch.get("sessionPath").is_none());
    }

    #[test]
    fn an_open_answer_becomes_the_thread_the_create_reports_on_that_device() {
        let request = SessionOpenRequest {
            cwd: Some("/work/app".to_owned()),
            name: Some("child".to_owned()),
            ..SessionOpenRequest::default()
        };
        let summary = opened_summary(
            "b",
            &json!({ "session_id": "rk-2", "durable_id": "dur-2", "started": true }),
            &request,
        )
        .expect("summary");
        assert_eq!(summary.thread, "dur-2");
        assert_eq!(summary.device.as_deref(), Some("b"));
        assert_eq!(summary.cwd, "/work/app");
        assert!(opened_summary("b", &json!({}), &request).is_none());
    }
}
