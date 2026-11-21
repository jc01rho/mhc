//! The port to this device's engine host: what sessions it serves, and one engine RPC record into one
//! of them. The production host (the mhc rpc host of each registered agent dir) lives with the CLI;
//! tests use their own.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One session the host serves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostSession {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `running`, `idle` or `ended`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
}

impl HostSession {
    /// Whether `id` names this session: its listing key or its durable id.
    pub fn is_named(&self, id: &str) -> bool {
        self.session_id == id || self.durable_id.as_deref() == Some(id)
    }
}

/// How the host took one record.
#[derive(Clone, Debug, PartialEq)]
pub enum HostCommandOutcome {
    /// The engine applied it; its answer.
    Applied(Value),
    /// The engine refused it, with its reason.
    Refused(String),
    /// The host no longer lists that session (it ended or parked elsewhere): nothing was sent
    /// (`route_unlisted`, omo v5.1.79).
    NotListed,
    /// It may or may not have run.
    Uncertain(String),
}

pub trait SessionHost: Send + Sync {
    fn list_sessions<'a>(
        &'a self,
        agent_dirs: &'a [PathBuf],
    ) -> BoxFuture<'a, Result<Vec<HostSession>, String>>;

    fn command<'a>(
        &'a self,
        agent_dirs: &'a [PathBuf],
        session_id: &'a str,
        record: &'a Value,
    ) -> BoxFuture<'a, HostCommandOutcome>;
}

/// A device with no engine host: it lists nothing and takes no command.
pub struct NoSessionHost;

impl SessionHost for NoSessionHost {
    fn list_sessions<'a>(
        &'a self,
        _agent_dirs: &'a [PathBuf],
    ) -> BoxFuture<'a, Result<Vec<HostSession>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn command<'a>(
        &'a self,
        _agent_dirs: &'a [PathBuf],
        _session_id: &'a str,
        _record: &'a Value,
    ) -> BoxFuture<'a, HostCommandOutcome> {
        Box::pin(async { HostCommandOutcome::NotListed })
    }
}
