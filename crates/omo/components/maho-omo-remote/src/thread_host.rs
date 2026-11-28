//! The engine side of the thread component in an mhc session (omo `thread/live-surface.ts` `requestFrame`, and senpi's
//! `pi.session`): the wire every thread tool request travels to this device's session hosts, and the session's own
//! control surface, bound into the gateway's `NativeThreadHost`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use maho_core::agent_session::AgentSession;
use maho_core::extensions::session_control_types::{self as engine, SessionControlActions};
use maho_core::session_control_actions::GenerationSessionControlActions;
use maho_omo_gateway::thread::adapter::BoxFuture;
use maho_omo_gateway::thread::attachment_fence::{ConversationFollowups, RpcTransport};
use maho_omo_gateway::thread::native::{NativeThreadHost, ThreadHostWire};
use maho_omo_gateway::thread::registration::{
    RegisterControlEndpointOptions, SessionControlActionsPort, SessionControlRegistration,
};
use serde_json::Value;

/// The thread host of the session `session` reads (`None` until the host binds it): the shared host of `agent_dir`
/// (`RPC_SOCKET`, then `<agentDir>/rpc/rpc.sock`, the socket `mhc host` binds) and the session's `pi.session`.
pub fn native_thread_host(
    session: Arc<dyn Fn() -> Option<AgentSession> + Send + Sync>,
    agent_dir: &Path,
) -> NativeThreadHost {
    NativeThreadHost {
        wire: Some(ThreadHostWire {
            socket: crate::sync_status::operator_socket(agent_dir)
                .to_string_lossy()
                .into_owned(),
            transport: Arc::new(HostTransport::new(agent_dir.to_path_buf())),
        }),
        session_control: Some(Arc::new(move || {
            let actions = session()?.session_control()?;
            let control: Arc<dyn SessionControlActionsPort> = Arc::new(SessionControl(actions));
            Some(control)
        })),
    }
}

/// The session's `pi.session` (the core's current extension generation) as the gateway reads it.
struct SessionControl(Arc<GenerationSessionControlActions>);

impl SessionControlActionsPort for SessionControl {
    fn admission_gate(&self) -> Value {
        self.0.admission_gate()
    }

    fn admit_external_message(&self, input: Value) -> Result<Value, String> {
        self.0.admit_external_message(input)
    }

    fn list_admitted_deliveries(&self) -> Value {
        self.0.list_admitted_deliveries()
    }

    fn persist_header_now(&self) -> BoxFuture<Result<(), String>> {
        let actions = self.0.clone();
        Box::pin(async move { actions.persist_header_now().await })
    }

    fn register_control_endpoint(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> BoxFuture<SessionControlRegistration> {
        let actions = self.0.clone();
        Box::pin(async move {
            let referenced = options.is_session_referenced;
            let registration = actions
                .register_control_endpoint(engine::RegisterControlEndpointOptions {
                    inbox_dir: options.inbox_dir,
                    drain: options.drain,
                    is_session_referenced: Some(Arc::new(move || -> BoxFuture<bool> {
                        let referenced = referenced.clone();
                        Box::pin(async move { referenced() })
                    })),
                })
                .await;
            match registration {
                engine::SessionControlRegistration::Registered { socket, dispose } => {
                    SessionControlRegistration::Registered { socket, dispose }
                }
                engine::SessionControlRegistration::Unsupported { reason } => {
                    SessionControlRegistration::Unsupported {
                        reason: reason.as_str().to_owned(),
                    }
                }
                engine::SessionControlRegistration::Failed { reason } => {
                    SessionControlRegistration::Failed { reason }
                }
            }
        })
    }
}

#[cfg(unix)]
type Held = maho_rpc::socket_client::RpcSocket;
#[cfg(not(unix))]
type Held = ();

/// The wire to this device's session hosts (`live-surface.ts` `requestFrame` on an `rpc_host` endpoint): each request
/// is one authenticated connection, opened with an observing `get_protocol_info`, except that a fenced command's
/// `attach_session`, the command and its `release_attachment` share the connection the attach was made on, as
/// upstream runs the three in one `withRpcSocket`: an attachment lasts as long as its connection.
pub struct HostTransport {
    agent_dir: PathBuf,
    attached: Arc<Mutex<HashMap<(String, String), Held>>>,
}

impl HostTransport {
    pub fn new(agent_dir: PathBuf) -> Self {
        Self {
            agent_dir,
            attached: Arc::default(),
        }
    }
}

impl RpcTransport for HostTransport {
    fn request(
        &self,
        socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let agent_dir = self.agent_dir.clone();
        let attached = self.attached.clone();
        Box::pin(async move { wire::converse(&socket, command, &agent_dir, &attached).await })
    }

    fn conversation(
        self: Arc<Self>,
        socket: &str,
        first: Value,
        _durable_session_id: Option<String>,
        followups: ConversationFollowups,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let agent_dir = self.agent_dir.clone();
        let attached = self.attached.clone();
        Box::pin(async move {
            wire::conversation(&socket, first, &agent_dir, &attached, followups).await
        })
    }
}

#[cfg(unix)]
mod wire {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use maho_omo_gateway::thread::attachment_fence::{ConversationFollowups, ConversationRequest};

    use maho_omo_gateway::thread::live_surface::{ENDPOINT_LIST_TIMEOUT_MS, REQUEST_TIMEOUT_MS};
    use maho_rpc::socket_client::RpcSocket;
    use serde_json::Value;
    use tokio::sync::Mutex as AsyncMutex;

    type Attached = Mutex<HashMap<(String, String), RpcSocket>>;

    fn attached_connections(
        attached: &Attached,
    ) -> MutexGuard<'_, HashMap<(String, String), RpcSocket>> {
        attached.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) async fn converse(
        socket: &str,
        command: Value,
        agent_dir: &Path,
        attached: &Attached,
    ) -> Result<Value, String> {
        let kind = command
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let key = command
            .get("sessionId")
            .and_then(Value::as_str)
            .map(|session| (socket.to_owned(), session.to_owned()));
        let attach = kind == "attach_session";
        let held = match &key {
            Some(key) if !attach => attached_connections(attached).remove(key),
            _ => None,
        };
        let reused = held.is_some();
        let mut connection = match held {
            Some(connection) => connection,
            None => open(socket, &kind, agent_dir).await?,
        };
        let answer = connection
            .request(command)
            .await
            .map_err(|error| io_failure(socket, &error))?;
        if refused_for_auth(&answer) {
            return Err(format!("auth_required:{socket}"));
        }
        let keep = if attach {
            answer.get("success").and_then(Value::as_bool) == Some(true)
        } else {
            reused && kind != "release_attachment"
        };
        if keep && let Some(key) = key {
            attached_connections(attached).insert(key, connection);
        }
        Ok(answer)
    }

    pub(super) async fn conversation(
        socket: &str,
        first: Value,
        agent_dir: &Path,
        attached: &Attached,
        followups: ConversationFollowups,
    ) -> Result<Value, String> {
        // Upstream's `requestFrame` runs `open_session` and its `set_session_name` inside one
        // `withRpcSocket`. Keep one authenticated connection and pass it to each follow-up, so the
        // host sees the naming request on the very connection its open attached.
        let kind = first
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let key = first
            .get("sessionId")
            .and_then(Value::as_str)
            .map(|session| (socket.to_owned(), session.to_owned()));
        let held = match &key {
            Some(key) => attached_connections(attached).remove(key),
            None => None,
        };
        let reused = held.is_some();
        let connection = match held {
            Some(connection) => connection,
            None => open(socket, &kind, agent_dir).await?,
        };
        // `RpcSocket::request` needs `&mut self`, but the per-conversation request is shared through
        // a `Fn` closure, so hold the connection behind a tokio mutex for the whole conversation.
        let shared = Arc::new(AsyncMutex::new(connection));
        let answer = shared
            .lock()
            .await
            .request(first)
            .await
            .map_err(|error| io_failure(socket, &error))?;
        if refused_for_auth(&answer) {
            return Err(format!("auth_required:{socket}"));
        }
        {
            let socket_for_request = socket.to_owned();
            let shared_for_request = shared.clone();
            let request: ConversationRequest = Arc::new(move |command| {
                let socket = socket_for_request.clone();
                let shared = shared_for_request.clone();
                Box::pin(async move {
                    shared
                        .lock()
                        .await
                        .request(command)
                        .await
                        .map_err(|error| io_failure(&socket, &error))
                })
            });
            followups(answer.clone(), request).await?;
        }
        // `request` (and its clone of `shared`) is dropped here; reclaim the held connection.
        // A follow-up that kept the request alive leaves the connection shared: it is dropped, not reused.
        let connection = match Arc::try_unwrap(shared) {
            Ok(connection) => connection.into_inner(),
            Err(_) => return Ok(answer),
        };
        // An `open_session` attach remains on the connection it opened, like the fence's attach.
        let keep = reused && kind != "release_attachment";
        if keep && let Some(key) = key {
            attached_connections(attached).insert(key, connection);
        }
        Ok(answer)
    }

    async fn open(socket: &str, kind: &str, agent_dir: &Path) -> Result<RpcSocket, String> {
        let timeout_ms = if matches!(kind, "list_sessions" | "get_protocol_info") {
            ENDPOINT_LIST_TIMEOUT_MS
        } else {
            REQUEST_TIMEOUT_MS
        };
        // Terminal control endpoints authenticate with 32 bytes from `<socket>.secret`, sent before
        // the first request, instead of the rpc-host identity handshake.
        if maho_omo_gateway::thread::endpoint_registry::is_tui_control_socket(socket) {
            let secret_path = std::path::PathBuf::from(
                maho_omo_gateway::thread::endpoint_registry::control_socket_secret_path(socket),
            );
            let secret = maho_rpc::socket_transport::read_socket_secret(&secret_path)
                .map_err(|error| format!("host_unavailable:{socket}: {error}"))?;
            let mut connection =
                RpcSocket::connect(socket, std::time::Duration::from_millis(timeout_ms))
                    .await
                    .map_err(|error| io_failure(socket, &error))?;
            maho_rpc::socket_transport::send_socket_handshake(
                connection.reader_mut().get_mut(),
                &secret,
            )
            .await
            .map_err(|error| format!("host_unavailable:{socket}: {error}"))?;
            return Ok(connection);
        }
        maho_rpc::host_probe::authenticated_host_socket(
            socket,
            &serde_json::json!({ "type": "get_protocol_info", "observe": true }),
            timeout_ms,
            Some(agent_dir),
        )
        .await
        .map(|(connection, _)| connection)
        .map_err(|error| open_failure(socket, error.as_ref()))
    }

    fn refused_for_auth(answer: &Value) -> bool {
        answer.get("success").and_then(Value::as_bool) == Some(false)
            && (answer.get("errorCode").and_then(Value::as_str) == Some("auth_required")
                || answer.get("error").and_then(Value::as_str) == Some("auth_required"))
    }

    /// A host that is not there is `host_unavailable:<socket>`, the failure the thread tools classify as such; a host
    /// whose identity is not proven keeps its typed code.
    fn open_failure(
        socket: &str,
        error: &(dyn std::error::Error + Send + Sync + 'static),
    ) -> String {
        if let Some(identity) = error.downcast_ref::<maho_rpc::socket_auth::RpcIdentityError>() {
            return format!("{}:{socket}", identity.code);
        }
        match error.downcast_ref::<std::io::Error>() {
            Some(io) => io_failure(socket, io),
            None => error.to_string(),
        }
    }

    fn io_failure(socket: &str, error: &std::io::Error) -> String {
        match error.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                format!("host_unavailable:{socket}")
            }
            std::io::ErrorKind::TimedOut => "thread RPC request timed out".to_owned(),
            _ => error.to_string(),
        }
    }
}

#[cfg(not(unix))]
mod wire {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use maho_omo_gateway::thread::attachment_fence::ConversationFollowups;
    use serde_json::Value;

    /// maho-rpc reaches a session host over a unix socket only, so on this platform no conversation opens.
    pub(super) async fn conversation(
        socket: &str,
        first: Value,
        agent_dir: &Path,
        attached: &Mutex<HashMap<(String, String), ()>>,
        followups: ConversationFollowups,
    ) -> Result<Value, String> {
        let _ = (first, agent_dir, attached, followups);
        Err(format!("host_unavailable:{socket}"))
    }

    /// maho-rpc reaches a session host over a unix socket only, so on this platform no host answers.
    pub(super) async fn converse(
        socket: &str,
        command: Value,
        agent_dir: &Path,
        attached: &Mutex<HashMap<(String, String), ()>>,
    ) -> Result<Value, String> {
        let _ = (command, agent_dir, attached);
        Err(format!("host_unavailable:{socket}"))
    }
}
