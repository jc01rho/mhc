//! The thread tools' client for every endpoint an agent directory holds.
//!
//! Upstream counterpart: `thread/live-surface.ts`. The wire client is the `RpcTransport` seam: this
//! module owns the surface logic (which endpoint is dialed, which session a command names, which
//! refusal a reply means), and the transport owns the socket.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use super::adapter::{
    BoxFuture, EndpointLiveness, GatewayEndpointPort, GatewayEndpointRef, ReleaseSessionReply,
};
use super::address_book::{AddressBookHost, DiskSession, HostListSessions, read_disk_session};
use super::attachment_fence::{FenceOutcome, RpcTransport, fenced_command};
use super::endpoint_controls::{
    LEGACY_TUI_COMMANDS as TUI_READ_COMMANDS, accepted_controls, control_commands,
    tui_commands_from,
};
use super::endpoint_registry::{EndpointKind as RegistryEndpointKind, RegistryEndpoint};
use super::gateway_replies::{release_reply, wake_reply};
use super::host_view::{SessionCommandPort, ThreadHost, ThreadHostSession, ThreadHostView};
use super::thread_state::holder_version;
use super::types::GatewayEndpointKind;

pub const REQUEST_TIMEOUT_MS: u64 = 60_000;
/// Budget for one endpoint's `list_sessions` while enumerating: one hung shard must not stall every call.
pub const ENDPOINT_LIST_TIMEOUT_MS: u64 = 10_000;
/// A terminal answers from the user's own process: a suspended or busy one costs a listing at most this long.
pub const TUI_REQUEST_TIMEOUT_MS: u64 = 1_500;
/// How long one `host status --all` enumeration is reused before the engine is asked again.
pub const HOST_ENDPOINTS_CACHE_TTL_MS: i64 = 5_000;
pub const HOST_STATUS_ALL_ARGS: [&str; 5] =
    ["host", "status", "--all", "--include-workers", "--json"];

/// The commands any terminal control endpoint answers, an older one included.
pub const LEGACY_TUI_COMMANDS: [&str; 5] = [
    "list_sessions",
    "get_messages",
    "get_state",
    "get_protocol_info",
    "release_session",
];

/// One endpoint row of `host status --all --include-workers --json`, reduced to what addressing reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEndpointReport {
    pub socket: Option<String>,
    pub reachable: bool,
    pub session_paths: Vec<String>,
    pub endpoint_kind: Option<GatewayEndpointKind>,
    pub alive: Option<bool>,
    pub reason: Option<String>,
}

fn strings_at(rows: Option<&Value>, field: &str) -> Vec<String> {
    rows.and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get(field)?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn owner_session_path(owner: Option<&Value>) -> Vec<String> {
    owner
        .and_then(|owner| owner.get("session"))
        .and_then(|session| session.get("path"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(|path| vec![path.to_owned()])
        .unwrap_or_default()
}

/// Reads the one JSON line `host status --all` prints; anything without an `endpoints` array cannot enumerate.
pub fn parse_host_status_all(stdout: &str) -> Option<Vec<HostEndpointReport>> {
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())?;
    let parsed: Value = serde_json::from_str(line).ok()?;
    let endpoints = parsed.get("endpoints")?.as_array()?;
    Some(
        endpoints
            .iter()
            .filter_map(|endpoint| {
                endpoint.as_object()?;
                let mut session_paths = strings_at(endpoint.get("session_rows"), "session_path");
                session_paths.extend(strings_at(endpoint.get("claims"), "session_path"));
                session_paths.extend(owner_session_path(endpoint.get("owner")));
                session_paths.sort();
                session_paths.dedup();
                Some(HostEndpointReport {
                    socket: endpoint
                        .get("socket")
                        .and_then(Value::as_str)
                        .filter(|socket| !socket.is_empty())
                        .map(str::to_owned),
                    reachable: endpoint
                        .get("reachable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    session_paths,
                    endpoint_kind: match endpoint.get("endpoint_kind").and_then(Value::as_str) {
                        Some("tui") => Some(GatewayEndpointKind::Tui),
                        Some("rpc_host") => Some(GatewayEndpointKind::RpcHost),
                        _ => None,
                    },
                    alive: endpoint.get("alive").and_then(Value::as_bool),
                    reason: endpoint
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect(),
    )
}

/// The frame a command's `data`, or the failure its reply names.
pub fn data_record(frame: &Value, command: &str) -> Result<Map<String, Value>, String> {
    let success = frame.get("success").and_then(Value::as_bool);
    let error = frame.get("error").and_then(Value::as_str);
    if success == Some(false)
        && command == "set_thinking_level"
        && error.is_some_and(|error| {
            error.starts_with("Thinking level ")
                && error.ends_with(" is not supported by the active model.")
        })
    {
        return Err(format!(
            "thinking_level_unsupported:{}",
            error.unwrap_or_default()
        ));
    }
    if success == Some(false) && error == Some("unsupported") {
        return Err(format!("unsupported:{command}"));
    }
    // An engine from before senpi's archived flag answers set_session_archived as a command it does not know.
    if success == Some(false)
        && command == "set_session_archived"
        && (frame.get("errorCode").and_then(Value::as_str) == Some("unknown_command")
            || error.is_some_and(|error| {
                error == "unknown_command" || error.starts_with("Unknown command:")
            }))
    {
        return Err(format!("unsupported:{command}"));
    }
    if success == Some(true)
        && frame.get("data").is_none()
        && matches!(
            command,
            "set_session_name" | "set_thinking_level" | "set_session_archived"
        )
    {
        return Ok(Map::new());
    }
    match frame.get("data").and_then(Value::as_object) {
        Some(data) if success == Some(true) => Ok(data.clone()),
        _ => Err(format!(
            "thread RPC request failed: {}",
            frame
                .get("error")
                .map_or_else(|| frame.to_string(), Value::to_string)
        )),
    }
}

/// The verdict of one endpoint's liveness.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointVerdict {
    pub alive: Option<bool>,
    pub reason: Option<String>,
}

/// What the surface knows about one endpoint, as the last enumeration said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownEndpoint {
    pub socket: String,
    pub paths: Vec<String>,
    /// Registry directories whose `reservations/` claims are read when the endpoint does not answer.
    pub claim_dirs: Vec<String>,
    pub kind: Option<GatewayEndpointKind>,
    pub verdict: EndpointVerdict,
}

/// One answered enumeration: the endpoints it found and the instant its cache window closes.
/// Shared by every surface over one agent dir through the injected map, so a session that a
/// delivery just created reuses the listing another session took inside the window (#12989).
pub struct SharedEnumeration {
    expires_at: i64,
    result: Vec<KnownEndpoint>,
}

/// The surface's view over one agent dir: it enumerates every endpoint once per cache window and
/// shares that enumeration with every surface over the same agent dir.
pub struct LiveThreadSurface {
    legacy: String,
    transport: Arc<dyn RpcTransport>,
    status_all: Arc<dyn Fn() -> Option<Vec<HostEndpointReport>> + Send + Sync>,
    registry: Arc<dyn Fn() -> Vec<RegistryEndpoint> + Send + Sync>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    enumerations: Arc<Mutex<BTreeMap<String, SharedEnumeration>>>,
    kinds: Mutex<BTreeMap<String, GatewayEndpointKind>>,
}

pub struct LiveThreadSurfaceOptions {
    pub socket: String,
    pub transport: Arc<dyn RpcTransport>,
    /// Replaces the engine CLI enumeration; a test answers it without spawning anything.
    pub status_all: Option<Arc<dyn Fn() -> Option<Vec<HostEndpointReport>> + Send + Sync>>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Shared enumeration map; surfaces agreeing on agent dir + legacy socket share one entry.
    pub enumerations: Option<Arc<Mutex<BTreeMap<String, SharedEnumeration>>>>,
    pub registry: Option<Arc<dyn Fn() -> Vec<RegistryEndpoint> + Send + Sync>>,
}

/// The thread tools' client: it never starts or replaces a host.
impl LiveThreadSurface {
    pub fn new(options: LiveThreadSurfaceOptions) -> Self {
        let now = options.now.unwrap_or_else(|| {
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis() as i64)
                    .unwrap_or(0)
            })
        });
        let enumerations = options
            .enumerations
            .unwrap_or_else(|| Arc::new(Mutex::new(BTreeMap::new())));
        Self {
            legacy: options.socket,
            transport: options.transport,
            status_all: options.status_all.unwrap_or_else(|| Arc::new(|| None)),
            // The registry is read only when a caller supplies its reader (the engine side reads
            // this device's agent dir); a surface built without one lists the report alone.
            registry: options.registry.unwrap_or_else(|| Arc::new(Vec::new)),
            now,
            enumerations,
            kinds: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn socket(&self) -> &str {
        &self.legacy
    }

    fn kind_of(&self, socket: &str) -> GatewayEndpointKind {
        self.kinds
            .lock()
            .ok()
            .and_then(|kinds| kinds.get(socket).copied())
            .unwrap_or(GatewayEndpointKind::RpcHost)
    }

    fn set_kind(&self, socket: &str, kind: GatewayEndpointKind) {
        if let Ok(mut kinds) = self.kinds.lock() {
            kinds.insert(socket.to_owned(), kind);
        }
    }

    /// Build the list of known endpoints from the status-all report and the on-disk registry: the
    /// report says what the engine sees, and the registry adds every endpoint that bound a socket
    /// even when the engine does not report it — the host shards and terminals `listView` must list.
    /// A registry directory also holds the session claims read from disk when its endpoint does not
    /// answer (#13452).
    fn enumerate_endpoints(
        &self,
        reports: Option<Vec<HostEndpointReport>>,
        recorded: Vec<RegistryEndpoint>,
    ) -> Vec<KnownEndpoint> {
        let mut found: Vec<KnownEndpoint> = reports
            .unwrap_or_default()
            .into_iter()
            .filter_map(|report| {
                Some(KnownEndpoint {
                    socket: report.socket?,
                    paths: report.session_paths,
                    claim_dirs: Vec::new(),
                    kind: report.endpoint_kind,
                    verdict: EndpointVerdict {
                        alive: report.alive,
                        reason: report.reason,
                    },
                })
            })
            .collect();

        for entry in recorded {
            let Some(socket) = entry.socket else {
                continue;
            };
            match found.iter().position(|known| known.socket == socket) {
                Some(index) => found[index].claim_dirs.push(entry.dir),
                None => found.push(KnownEndpoint {
                    socket,
                    paths: Vec::new(),
                    claim_dirs: vec![entry.dir],
                    kind: Some(match entry.endpoint_kind {
                        RegistryEndpointKind::Tui => GatewayEndpointKind::Tui,
                        RegistryEndpointKind::RpcHost => GatewayEndpointKind::RpcHost,
                    }),
                    verdict: EndpointVerdict::default(),
                }),
            }
        }
        for endpoint in &mut found {
            endpoint.claim_dirs.sort();
            endpoint.claim_dirs.dedup();
        }

        // The legacy endpoint is always listed first; a report that already names it keeps its
        // session files, kind and liveness verdict.
        let mut rest: Vec<KnownEndpoint> = Vec::with_capacity(found.len());
        let mut legacy: Option<KnownEndpoint> = None;
        for endpoint in found {
            if legacy.is_none() && endpoint.socket == self.legacy {
                legacy = Some(endpoint);
            } else {
                rest.push(endpoint);
            }
        }
        let mut result = Vec::with_capacity(rest.len() + 1);
        result.push(legacy.unwrap_or_else(|| KnownEndpoint {
            socket: self.legacy.clone(),
            ..KnownEndpoint::default()
        }));
        result.extend(rest);
        result
    }

    /// The shared enumeration: an entry still inside its cache window is returned as it was
    /// recorded, so surfaces over one agent dir ask the engine and the registry once per window
    /// (#12989). A stale or absent entry is enumerated here; the enumeration is synchronous
    /// (the registry read and the `host status --all` callback), so no caller ever observes a
    /// half-finished one.
    pub fn known_endpoints(&self) -> Vec<KnownEndpoint> {
        // The legacy socket names the agent dir a surface enumerates (one process serves one agent
        // dir), so surfaces over the same dir share one entry in the injected map (#12989).
        let key = self.legacy.clone();
        let current = (self.now)();
        if let Some(cached) = self
            .enumerations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .filter(|entry| entry.expires_at > current)
        {
            return cached.result.clone();
        }
        let reports = (self.status_all)();
        let recorded = (self.registry)();
        let found = self.enumerate_endpoints(reports, recorded);
        {
            let mut kinds = self
                .kinds
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for endpoint in &found {
                if let Some(kind) = &endpoint.kind {
                    kinds.insert(endpoint.socket.clone(), *kind);
                }
            }
        }
        let _ = self
            .enumerations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                key,
                SharedEnumeration {
                    expires_at: current + HOST_ENDPOINTS_CACHE_TTL_MS,
                    result: found.clone(),
                },
            );
        found
    }

    pub fn call(
        &self,
        socket: &str,
        command: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        self.transport
            .request(socket, command, durable_session_id.map(str::to_owned))
    }

    /// One request under the attachment fence, with a terminal's secret and command set honored.
    fn fenced(
        &self,
        socket: &str,
        command: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let kind = self.kind_of(&socket);
        let transport = self.transport.clone();
        let durable = durable_session_id.map(str::to_owned);
        let capabilities: std::collections::BTreeSet<String> = [
            "attach_session",
            "release_attachment",
            "release_caller_aware",
        ]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
        Box::pin(async move {
            if kind == GatewayEndpointKind::Tui {
                let command_type = command
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !LEGACY_TUI_COMMANDS.contains(&command_type.as_str()) {
                    return Err(format!("unsupported:{command_type}"));
                }
                return transport.request(&socket, command, None).await;
            }
            let send_command = command.clone();
            let send_transport = transport.clone();
            let send_socket = socket.clone();
            let send = move || send_transport.request(&send_socket, send_command.clone(), None);
            match fenced_command(
                transport.as_ref(),
                &socket,
                command,
                &capabilities,
                durable.as_deref(),
                &send,
            )
            .await?
            {
                FenceOutcome::Ran(frame) | FenceOutcome::Refused(frame) => Ok(frame),
                FenceOutcome::Failed(error) => Err(error),
            }
        })
    }

    fn call_on(
        &self,
        socket: &str,
        command: &str,
        data: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let mut frame = data;
        if let Some(object) = frame.as_object_mut() {
            object.insert("type".into(), Value::String(command.to_owned()));
        }
        let command_name = command.to_owned();
        let pending = self.fenced(socket, frame, durable_session_id);
        Box::pin(async move {
            let frame = pending.await?;
            let data = data_record(&frame, &command_name)?;
            Ok(Value::Object(data))
        })
    }

    /// The routing id of a host session, looked up in that host's own listing when the endpoint has none.
    pub fn routing_id_of(
        &self,
        endpoint: &GatewayEndpointRef,
    ) -> BoxFuture<Result<Option<String>, String>> {
        if let Some(routing_id) = &endpoint.routing_id {
            let routing_id = routing_id.clone();
            return Box::pin(async move { Ok(Some(routing_id)) });
        }
        let socket = endpoint.socket.clone();
        let durable_id = endpoint.durable_session_id.clone();
        let listing = self.call_on(
            &socket,
            "list_sessions",
            serde_json::json!({ "observe": true }),
            None,
        );
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(sessions.into_iter().find_map(|session| {
                let durable = session.get("durableSessionId").and_then(Value::as_str);
                let status = session.get("status").and_then(Value::as_str);
                (durable == Some(durable_id.as_str()) && status != Some("closed"))
                    .then(|| {
                        session
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten()
            }))
        })
    }

    /// The gateway's sender port. A delivery is announced with `wake` only, never `prompt`.
    pub fn gateway(&self) -> GatewayEndpointPort {
        let surface = Arc::new(SurfaceHandle {
            transport: self.transport.clone(),
        });
        let wake_surface = surface.clone();
        let release_surface = surface.clone();
        let liveness_surface = surface;
        GatewayEndpointPort {
            wake: Arc::new(move |endpoint, delivery_ids| {
                let surface = wake_surface.clone();
                Box::pin(async move {
                    let session_id = if endpoint.kind == GatewayEndpointKind::RpcHost {
                        surface.routing_id_of(&endpoint).await.ok().flatten()
                    } else {
                        None
                    };
                    if endpoint.kind == GatewayEndpointKind::RpcHost && session_id.is_none() {
                        return super::adapter::GatewayWakeReply::unroutable_no_live_session();
                    }
                    let mut command =
                        serde_json::json!({ "type": "wake", "delivery_ids": delivery_ids });
                    if let Some(session_id) = session_id
                        && let Some(object) = command.as_object_mut()
                    {
                        object.insert("sessionId".into(), Value::String(session_id));
                    }
                    match surface
                        .fenced(
                            &endpoint.socket,
                            command,
                            Some(&endpoint.durable_session_id),
                        )
                        .await
                    {
                        Ok(frame) => wake_reply(&frame),
                        Err(_) => super::adapter::GatewayWakeReply::unroutable_no_live_session(),
                    }
                })
            }),
            release_session: Some(Arc::new(move |endpoint, request| {
                let surface = release_surface.clone();
                Box::pin(async move {
                    if endpoint.kind != GatewayEndpointKind::RpcHost {
                        return ReleaseSessionReply::Failure {
                            error: "unsupported".to_owned(),
                            error_data: None,
                        };
                    }
                    let session_id = match surface.routing_id_of(&endpoint).await {
                        Ok(Some(session_id)) => session_id,
                        _ => {
                            return ReleaseSessionReply::Failure {
                                error: "unknown_session".to_owned(),
                                error_data: None,
                            };
                        }
                    };
                    let command = serde_json::json!({ "type": "release_session", "sessionId": session_id, "reason": request.reason, "interrupt": request.interrupt, "force": request.force });
                    match surface
                        .fenced(
                            &endpoint.socket,
                            command,
                            Some(&endpoint.durable_session_id),
                        )
                        .await
                    {
                        Ok(frame) => release_reply(&frame),
                        Err(error) => ReleaseSessionReply::Failure {
                            error,
                            error_data: None,
                        },
                    }
                })
            })),
            classify_liveness: Some(Arc::new(move |endpoint| {
                let surface = liveness_surface.clone();
                Box::pin(async move {
                    match surface
                        .call_on(
                            &endpoint.socket,
                            "get_protocol_info",
                            serde_json::json!({ "observe": true }),
                            None,
                        )
                        .await
                    {
                        Ok(_) => EndpointLiveness::Routable,
                        Err(error) if error.starts_with("host_unavailable:") => {
                            EndpointLiveness::Dead
                        }
                        Err(_) => EndpointLiveness::LiveUnresponsive,
                    }
                })
            })),
        }
    }

    /// The owner of one durable id, listed exactly as a listing lists it.
    pub fn list_target(
        &self,
        durable_id: &str,
        endpoint: &GatewayEndpointRef,
    ) -> BoxFuture<Result<Vec<Value>, String>> {
        self.set_kind(&endpoint.socket, endpoint.kind);
        let socket = endpoint.socket.clone();
        let durable_id = durable_id.to_owned();
        let listing = self.call_on(
            &socket,
            "list_sessions",
            serde_json::json!({ "observe": true }),
            None,
        );
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(sessions
                .into_iter()
                .filter(|session| {
                    let durable = session
                        .get("durableSessionId")
                        .and_then(Value::as_str)
                        .or_else(|| session.get("sessionId").and_then(Value::as_str));
                    durable == Some(durable_id.as_str())
                        && session.get("status").and_then(Value::as_str) != Some("closed")
                })
                .collect())
        })
    }

    /// Opens a thread on the legacy endpoint and reads the session list back, so the reply carries the
    /// durable id and the name the address book keys entries by.
    pub fn open_session(
        &self,
        params: Value,
        name: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = self.legacy.clone();
        let name = name.map(str::to_owned);
        let transport = self.transport.clone();
        Box::pin(async move {
            let mut command = params;
            if let Some(object) = command.as_object_mut() {
                object.insert("type".into(), Value::String("open_session".into()));
                object.insert("retain_on_disconnect".into(), Value::Bool(true));
            }
            // Upstream `requestFrame` sends `set_session_name` on the connection `open_session`
            // opened, so the host has attached the caller by the time the naming command arrives.
            let name = name.filter(|name| !name.trim().is_empty());
            let name_for_closure = name.clone();
            let transport_for_closure = transport.clone();
            let socket_for_closure = socket.clone();
            let frame = transport
                .clone()
                .conversation(
                    &socket,
                    command,
                    None,
                    Arc::new(move |reply, request| {
                        let name = name_for_closure.clone();
                        let transport_for_naming = transport_for_closure.clone();
                        let socket_for_naming = socket_for_closure.clone();
                        Box::pin(async move {
                            let Some(name) = name else {
                                return Ok(());
                            };
                            let opened = match data_record(&reply, "open_session") {
                                Ok(opened) => opened,
                                Err(_) => return Ok(()),
                            };
                            let routing_id = opened
                                .get("sessionId")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let naming = request(serde_json::json!({
                                "type": "set_session_name",
                                "sessionId": routing_id,
                                "name": name,
                            }))
                            .await;
                            if naming.is_err() {
                                let _ = transport_for_naming
                                    .request(
                                        &socket_for_naming,
                                        serde_json::json!({
                                            "type": "set_session_name",
                                            "sessionId": routing_id,
                                            "name": name,
                                        }),
                                        None,
                                    )
                                    .await;
                            }
                            Ok(())
                        })
                    }),
                )
                .await?;
            let mut opened = data_record(&frame, "open_session")?;
            let routing_id = opened
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let listing = transport
                .clone()
                .request(
                    &socket,
                    serde_json::json!({ "type": "list_sessions", "observe": true }),
                    None,
                )
                .await?;
            let listed = data_record(&listing, "list_sessions")?;
            let sessions = listed
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(session) = sessions.into_iter().find(|session| {
                session.get("sessionId").and_then(Value::as_str) == Some(routing_id.as_str())
            }) && let Some(object) = session.as_object()
            {
                for (key, value) in object {
                    opened.insert(key.clone(), value.clone());
                }
            }
            opened.insert("sessionId".into(), Value::String(routing_id));
            opened.insert("socket".into(), Value::String(socket));
            opened.insert("endpoint_kind".into(), Value::String("rpc_host".into()));
            Ok(Value::Object(opened))
        })
    }
}

/// One endpoint's part of a host view.
struct ListedEndpoint {
    socket: String,
    failed: bool,
    sessions: Vec<ThreadHostSession>,
    host: AddressBookHost,
    disk: Vec<DiskSession>,
}

/// One endpoint's observing `list_sessions`; a terminal also says which session controls it takes. A dead endpoint is
/// an address-book host carrying its failure, plus the sessions its report still claims, read from their files.
async fn list_endpoint(
    transport: Arc<dyn RpcTransport>,
    endpoint: KnownEndpoint,
    kind: GatewayEndpointKind,
    legacy: bool,
) -> ListedEndpoint {
    let host = |list_sessions: HostListSessions| AddressBookHost {
        socket: endpoint.socket.clone(),
        list_sessions: Some(list_sessions),
        endpoint_kind: Some(kind),
        alive: endpoint.verdict.alive,
        reason: endpoint.verdict.reason.clone(),
        legacy,
    };
    // A session held by a terminal or a task-host shard can be absent from `list_sessions`: its
    // file is the record of its threads when the endpoint does not answer. Complete the disk list
    // with the claims its registry directory still holds (#13452).
    let mut disk_paths = endpoint.paths.clone();
    for dir in &endpoint.claim_dirs {
        disk_paths.extend(crate::thread::endpoint_registry::claimed_session_paths(dir));
    }
    disk_paths.sort();
    disk_paths.dedup();
    let listing = transport
        .request(
            &endpoint.socket,
            serde_json::json!({ "type": "list_sessions", "observe": true }),
            None,
        )
        .await
        .and_then(|frame| data_record(&frame, "list_sessions"));
    let data = match listing {
        Ok(data) => data,
        Err(error) => {
            return ListedEndpoint {
                socket: endpoint.socket.clone(),
                failed: true,
                sessions: Vec::new(),
                host: host(HostListSessions::failed(error)),
                disk: disk_paths
                    .iter()
                    .filter_map(|path| {
                        read_disk_session(
                            std::path::Path::new(path),
                            Some(endpoint.socket.as_str()),
                        )
                    })
                    .collect(),
            };
        }
    };
    let protocol = transport
        .request(
            &endpoint.socket,
            serde_json::json!({ "type": "get_protocol_info", "observe": true }),
            None,
        )
        .await
        .and_then(|frame| data_record(&frame, "get_protocol_info"))
        .ok()
        .map(Value::Object);
    // omo-relay-spec #314: the engine's own version when the key is present, the legacy serverVersion only when it is
    // absent; either one only when it is a holder version.
    let engine_version = protocol.as_ref().and_then(|info| {
        holder_version(
            info.get("engineVersion")
                .or_else(|| info.get("serverVersion")),
        )
    });
    let controls = if kind == GatewayEndpointKind::Tui {
        accepted_controls(kind, Some(&tui_commands_from(protocol.as_ref())))
    } else {
        accepted_controls(kind, None)
    };
    let sessions: Vec<ThreadHostSession> = data
        .get("sessions")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    ThreadHostSession::from_listing(row, &endpoint.socket, kind, &controls)
                })
                .map(|mut session| {
                    if engine_version.is_some() {
                        session.engine_version = engine_version.clone();
                    }
                    session
                })
                .collect()
        })
        .unwrap_or_default();
    ListedEndpoint {
        socket: endpoint.socket.clone(),
        failed: false,
        host: host(HostListSessions::ok(
            sessions
                .iter()
                .map(ThreadHostSession::host_session)
                .collect(),
        )),
        sessions,
        disk: Vec::new(),
    }
}

/// The command channel to the endpoint that listed one session. A host session's records go under the attachment
/// fence; a terminal takes only the commands its session controls name, and any other is `unsupported:<command>`
/// before anything is sent.
struct LiveSessionPort {
    transport: Arc<dyn RpcTransport>,
    socket: String,
    kind: GatewayEndpointKind,
    durable_session_id: String,
    terminal_commands: std::collections::BTreeSet<String>,
}

impl SessionCommandPort for LiveSessionPort {
    fn send(&self, command: Value) -> BoxFuture<Result<Value, String>> {
        if self.kind == GatewayEndpointKind::Tui {
            let command_type = command
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if !self.terminal_commands.contains(&command_type) {
                return Box::pin(async move { Err(format!("unsupported:{command_type}")) });
            }
            return self.transport.request(&self.socket, command, None);
        }
        SurfaceHandle {
            transport: self.transport.clone(),
        }
        .fenced(
            &self.socket,
            command,
            Some(self.durable_session_id.as_str()),
        )
    }
}

impl ThreadHost for LiveThreadSurface {
    /// Every known endpoint listed at once. When no endpoint answers, the first failure (the legacy endpoint first) is
    /// the view's failure, `host_unavailable:<socket>`.
    fn list_view(&self) -> BoxFuture<Result<ThreadHostView, String>> {
        let endpoints = self.known_endpoints();
        let kinds: Vec<GatewayEndpointKind> = endpoints
            .iter()
            .map(|endpoint| {
                if let Some(kind) = endpoint.kind {
                    self.set_kind(&endpoint.socket, kind);
                }
                endpoint
                    .kind
                    .unwrap_or_else(|| self.kind_of(&endpoint.socket))
            })
            .collect();
        let transport = self.transport.clone();
        let legacy = self.legacy.clone();
        Box::pin(async move {
            let listed = futures::future::join_all(endpoints.into_iter().zip(kinds).map(
                |(endpoint, kind)| {
                    let is_legacy = endpoint.socket == legacy;
                    list_endpoint(transport.clone(), endpoint, kind, is_legacy)
                },
            ))
            .await;
            if let Some(first) = listed.first()
                && listed.iter().all(|endpoint| endpoint.failed)
            {
                return Err(format!("host_unavailable:{}", first.socket));
            }
            let mut view = ThreadHostView::default();
            for endpoint in listed {
                view.sessions.extend(endpoint.sessions);
                view.hosts.push(endpoint.host);
                view.disk.extend(endpoint.disk);
            }
            Ok(view)
        })
    }

    fn open_host_session(
        &self,
        params: Value,
        name: Option<String>,
    ) -> BoxFuture<Result<ThreadHostSession, String>> {
        let opening = self.open_session(params, name.as_deref());
        let socket = self.legacy.clone();
        Box::pin(async move {
            let opened = opening.await?;
            let controls = accepted_controls(GatewayEndpointKind::RpcHost, None);
            ThreadHostSession::from_listing(
                &opened,
                &socket,
                GatewayEndpointKind::RpcHost,
                &controls,
            )
            .ok_or_else(|| "open_session answered no session".to_owned())
        })
    }

    fn capabilities(&self) -> BoxFuture<Result<Vec<String>, String>> {
        let transport = self.transport.clone();
        let socket = self.legacy.clone();
        Box::pin(async move {
            let frame = transport
                .request(
                    &socket,
                    serde_json::json!({ "type": "get_protocol_info", "observe": true }),
                    None,
                )
                .await?;
            let info = data_record(&frame, "get_protocol_info")?;
            Ok(info
                .get("capabilities")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default())
        })
    }

    fn session_port(&self, session: &ThreadHostSession) -> Arc<dyn SessionCommandPort> {
        let mut terminal_commands: std::collections::BTreeSet<String> = TUI_READ_COMMANDS
            .iter()
            .map(|command| (*command).to_owned())
            .collect();
        for control in &session.controls {
            terminal_commands.extend(
                control_commands(control)
                    .iter()
                    .map(|command| (*command).to_owned()),
            );
        }
        Arc::new(LiveSessionPort {
            transport: self.transport.clone(),
            socket: session.socket.clone(),
            kind: session.endpoint_kind,
            durable_session_id: session.thread_id().to_owned(),
            terminal_commands,
        })
    }
}

/// The parts a gateway port needs after the surface is boxed.
struct SurfaceHandle {
    transport: Arc<dyn RpcTransport>,
}

impl SurfaceHandle {
    fn fenced(
        &self,
        socket: &str,
        command: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let transport = self.transport.clone();
        let durable = durable_session_id.map(str::to_owned);
        let capabilities: std::collections::BTreeSet<String> = [
            "attach_session",
            "release_attachment",
            "release_caller_aware",
        ]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
        Box::pin(async move {
            let send_command = command.clone();
            let send_transport = transport.clone();
            let send_socket = socket.clone();
            let send = move || send_transport.request(&send_socket, send_command.clone(), None);
            match fenced_command(
                transport.as_ref(),
                &socket,
                command,
                &capabilities,
                durable.as_deref(),
                &send,
            )
            .await?
            {
                FenceOutcome::Ran(frame) | FenceOutcome::Refused(frame) => Ok(frame),
                FenceOutcome::Failed(error) => Err(error),
            }
        })
    }

    fn call_on(
        &self,
        socket: &str,
        command: &str,
        data: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let mut frame = data;
        if let Some(object) = frame.as_object_mut() {
            object.insert("type".into(), Value::String(command.to_owned()));
        }
        let command_name = command.to_owned();
        let pending = self.fenced(socket, frame, durable_session_id);
        Box::pin(async move {
            let frame = pending.await?;
            let data = data_record(&frame, &command_name)?;
            Ok(Value::Object(data))
        })
    }

    fn routing_id_of(
        &self,
        endpoint: &GatewayEndpointRef,
    ) -> BoxFuture<Result<Option<String>, String>> {
        if let Some(routing_id) = &endpoint.routing_id {
            let routing_id = routing_id.clone();
            return Box::pin(async move { Ok(Some(routing_id)) });
        }
        let socket = endpoint.socket.clone();
        let durable_id = endpoint.durable_session_id.clone();
        let listing = self.call_on(
            &socket,
            "list_sessions",
            serde_json::json!({ "observe": true }),
            None,
        );
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(sessions.into_iter().find_map(|session| {
                let durable = session.get("durableSessionId").and_then(Value::as_str);
                (durable == Some(durable_id.as_str())
                    && session.get("status").and_then(Value::as_str) != Some("closed"))
                .then(|| {
                    session
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .flatten()
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_json_line_of_host_status_all_is_read() {
        let stdout = "warning: legacy engine\n{\"endpoints\":[{\"socket\":\"/run/i-1.sock\",\"reachable\":true,\"session_rows\":[{\"session_path\":\"/s/a.jsonl\"}],\"endpoint_kind\":\"rpc_host\",\"alive\":true}]}\n";
        let reports = parse_host_status_all(stdout).expect("reports");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].socket.as_deref(), Some("/run/i-1.sock"));
        assert_eq!(reports[0].session_paths, vec!["/s/a.jsonl".to_owned()]);
        assert!(parse_host_status_all("not json").is_none());
        assert!(parse_host_status_all("{\"endpoints\": 3}").is_none());
    }

    #[test]
    fn an_unsupported_command_and_an_unsupported_thinking_level_keep_their_codes() {
        assert_eq!(
            data_record(
                &serde_json::json!({ "success": false, "error": "unsupported" }),
                "prompt"
            ),
            Err("unsupported:prompt".to_owned())
        );
        assert_eq!(
            data_record(&serde_json::json!({ "success": false, "error": "Thinking level max is not supported by the active model." }), "set_thinking_level"),
            Err("thinking_level_unsupported:Thinking level max is not supported by the active model.".to_owned())
        );
        assert!(data_record(&serde_json::json!({ "success": true }), "set_session_name").is_ok());
    }
}
