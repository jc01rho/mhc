//! One call's view of every endpoint, and the ports the thread tools reach a host through.
//!
//! Upstream counterpart: `thread/tools/ports.ts` (`ThreadHostSession`, `ThreadHostView`, `ThreadHost`,
//! `ThreadSessionPort`). The port is deliberately narrow: a listing of every endpoint, and one command channel to the
//! endpoint that listed a session. Everything a control does is built on that channel (`control/local_control.rs`).

use std::sync::Arc;

use serde_json::Value;

use super::adapter::BoxFuture;
use super::address_book::{AddressBookHost, DiskSession, HostSession, HostSessionStatus};
use super::contracts_state::ThreadModel;
use super::thread_state::thread_model;
use super::types::GatewayEndpointKind;

/// One session as the endpoint that holds it lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadHostSession {
    /// The routing id on its endpoint (`rpc-N` on a host); a terminal names its one session by its durable id.
    pub session_id: String,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub cwd: String,
    pub name: Option<String>,
    pub status: Option<HostSessionStatus>,
    /// senpi session kind (`interactive` | `worker`).
    pub kind: Option<String>,
    /// The session file's archived flag; an engine from before `set_session_archived` lists none.
    pub archived: bool,
    /// What the session is doing (senpi#6688); an older engine lists only `running` | `idle`, or none.
    pub activity: Option<String>,
    pub error_code: Option<String>,
    pub turn_id: Option<String>,
    pub model: Option<ThreadModel>,
    /// The engine's retry wait of a running thread (senpi#7025): `{kind, until, attempt, maxAttempts}` as it lists it.
    pub waiting: Option<Value>,
    /// The version of the engine that endpoint runs, as its `get_protocol_info` names it (#14278).
    pub engine_version: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub endpoint_kind: GatewayEndpointKind,
    /// The controls the endpoint that listed the session accepts (`endpoint_controls.rs`).
    pub controls: Vec<String>,
    /// The endpoint that listed the session: routing ids are per-endpoint counters, so a session is only addressable as
    /// the pair (socket, session id).
    pub socket: String,
}

impl ThreadHostSession {
    /// The durable id the address book keys the session by.
    pub fn thread_id(&self) -> &str {
        self.durable_session_id
            .as_deref()
            .unwrap_or(&self.session_id)
    }

    /// One `list_sessions` row; a row without a `sessionId` is not a session.
    pub fn from_listing(
        value: &Value,
        socket: &str,
        endpoint_kind: GatewayEndpointKind,
        controls: &[String],
    ) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let either = |camel: &str, snake: &str| text(camel).or_else(|| text(snake));
        Some(Self {
            session_id: text("sessionId")?,
            durable_session_id: either("durableSessionId", "durable_session_id"),
            session_path: either("sessionPath", "session_path"),
            cwd: text("cwd").unwrap_or_default(),
            name: text("name"),
            status: text("status").as_deref().and_then(HostSessionStatus::parse),
            kind: text("kind"),
            archived: value
                .get("archived")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            activity: text("activity"),
            error_code: text("error_code"),
            turn_id: text("turn_id"),
            model: thread_model(value.get("model")),
            waiting: value
                .get("waiting")
                .filter(|waiting| !waiting.is_null())
                .cloned(),
            engine_version: either("engineVersion", "engine_version")
                .or_else(|| either("serverVersion", "server_version")),
            created_at: either("createdAt", "created_at"),
            updated_at: either("updatedAt", "updated_at"),
            endpoint_kind,
            controls: controls.to_vec(),
            socket: socket.to_owned(),
        })
    }

    /// The row the address book assembles entries from.
    pub fn host_session(&self) -> HostSession {
        HostSession {
            session_id: self.session_id.clone(),
            durable_session_id: self.durable_session_id.clone(),
            session_path: self.session_path.clone(),
            cwd: self.cwd.clone(),
            name: self.name.clone(),
            status: self.status,
            kind: self.kind.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

/// One call's view of every endpoint: the live sessions (each tagged with its `socket`), one address-book host per
/// endpoint (a dead endpoint carries its error), and the durable sessions a dead endpoint last held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadHostView {
    pub sessions: Vec<ThreadHostSession>,
    pub hosts: Vec<AddressBookHost>,
    pub disk: Vec<DiskSession>,
}

impl ThreadHostView {
    /// The live session a durable id names, if an endpoint lists it.
    pub fn target_session(&self, durable_id: &str) -> Option<&ThreadHostSession> {
        self.sessions
            .iter()
            .find(|session| session.thread_id() == durable_id)
    }
}

/// The command channel to the one endpoint that holds a session: `send` writes one record (`{type, sessionId, ...}`)
/// under the endpoint's attachment fence and answers the reply frame. A transport failure is the `Err`; a refusal the
/// engine answers is a frame with `success: false`. A session-scoped refusal of the fence itself comes back as an
/// `Err` of the form `<code>:<session id>` (`unknown_session`, `session_identity_mismatch`, `session_not_attached`).
pub trait SessionCommandPort: Send + Sync {
    fn send(&self, command: Value) -> BoxFuture<Result<Value, String>>;
}

/// The host surface the thread tools read: every endpoint's listing in one view, and the command channel of the
/// endpoint that listed a session. A view no endpoint answered is `Err("host_unavailable:<socket>")`.
pub trait ThreadHost: Send + Sync {
    fn list_view(&self) -> BoxFuture<Result<ThreadHostView, String>>;
    fn session_port(&self, session: &ThreadHostSession) -> Arc<dyn SessionCommandPort>;
    /// `open_session` on the operator endpoint (thread_create): the new session, named, with its listing row read back.
    /// A host that cannot open answers `unsupported:open_session`; a refusal the engine answers keeps its text
    /// (`model_unavailable: ...`).
    fn open_host_session(
        &self,
        _params: Value,
        _name: Option<String>,
    ) -> BoxFuture<Result<ThreadHostSession, String>> {
        Box::pin(async { Err("unsupported:open_session".to_owned()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_row_reads_both_spellings_and_the_engine_activity() {
        let row = serde_json::json!({
            "sessionId": "rpc-1",
            "durableSessionId": "d-1",
            "cwd": "/work",
            "name": "Builder",
            "status": "open",
            "archived": true,
            "activity": "queued",
            "waiting": { "kind": "rate_limit", "until": "2026-11-02T00:01:00.000Z", "attempt": 2, "maxAttempts": 5 },
            "engineVersion": "2026.11.1",
            "model": { "provider": "anthropic", "id": "opus" },
            "created_at": "2026-11-01T00:00:00.000Z",
            "updatedAt": "2026-11-02T00:00:00.000Z",
        });
        let session = ThreadHostSession::from_listing(
            &row,
            "/run/i-1.sock",
            GatewayEndpointKind::RpcHost,
            &["send".to_owned()],
        )
        .expect("session");
        assert_eq!(session.thread_id(), "d-1");
        assert!(session.archived);
        assert_eq!(session.activity.as_deref(), Some("queued"));
        assert_eq!(
            session
                .waiting
                .as_ref()
                .and_then(|waiting| waiting.get("attempt"))
                .and_then(Value::as_i64),
            Some(2)
        );
        assert_eq!(session.engine_version.as_deref(), Some("2026.11.1"));
        assert_eq!(
            session.created_at.as_deref(),
            Some("2026-11-01T00:00:00.000Z")
        );
        assert_eq!(
            session.updated_at.as_deref(),
            Some("2026-11-02T00:00:00.000Z")
        );
        assert_eq!(session.host_session().status, Some(HostSessionStatus::Open));
        assert!(
            ThreadHostSession::from_listing(
                &serde_json::json!({ "cwd": "/work" }),
                "/run/i-1.sock",
                GatewayEndpointKind::RpcHost,
                &[]
            )
            .is_none()
        );
    }
}
