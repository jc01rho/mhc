//! The production session host of mhc-relayd: the mhc rpc host of each registered agent dir, driven
//! through `maho_rpc::host_session_runner` (the short-lived attach-before-act client behind
//! `mhc host session`). Each record maps to one host action; the session is found by its handle or
//! durable id in the host's own listing.

use std::path::{Path, PathBuf};

use maho_rpc::host_runner::{HostOutcome, HostTarget};
use maho_rpc::host_session_runner::{
    HostSessionAction, HostSessionModel, HostSessionRequest, run_host_session_request,
};
use mhc_relayd::session_host::BoxFuture;
use mhc_relayd::{HostCommandOutcome, HostSession, SessionHost};
use serde_json::Value;

pub struct RpcSessionHost {
    default_agent_dir: PathBuf,
}

impl RpcSessionHost {
    pub fn new(default_agent_dir: PathBuf) -> Self {
        Self { default_agent_dir }
    }

    fn agent_dirs(&self, registered: &[PathBuf]) -> Vec<PathBuf> {
        if registered.is_empty() {
            vec![self.default_agent_dir.clone()]
        } else {
            registered.to_vec()
        }
    }
}

fn target_of(agent_dir: &Path) -> HostTarget {
    HostTarget {
        socket: maho_rpc::rpc_client::default_rpc_socket_path(agent_dir)
            .to_string_lossy()
            .into_owned(),
        agent_dir: agent_dir.to_path_buf(),
    }
}

/// Runs one host request on a thread of its own, so the host client's future need not be `Send`.
async fn run(target: HostTarget, action: HostSessionAction) -> HostOutcome {
    let joined = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        match runtime {
            Ok(runtime) => runtime.block_on(run_host_session_request(HostSessionRequest {
                target,
                action,
            })),
            Err(error) => HostOutcome {
                exit_code: 1,
                payload: serde_json::json!({ "reason": error.to_string() }),
            },
        }
    })
    .await;
    joined.unwrap_or_else(|error| HostOutcome {
        exit_code: 1,
        payload: serde_json::json!({ "reason": error.to_string() }),
    })
}

/// A host `list_sessions` row as a listed session.
pub fn host_session(row: &Value) -> Option<HostSession> {
    let text = |key: &str| row.get(key).and_then(Value::as_str).map(str::to_owned);
    let status = row.get("status").and_then(Value::as_str).unwrap_or("open");
    Some(HostSession {
        session_id: text("sessionId")?,
        durable_id: text("durableSessionId"),
        title: text("name"),
        project: text("cwd"),
        path: text("sessionPath"),
        state: if matches!(status, "closing" | "closed" | "quarantined") {
            "ended".into()
        } else {
            "idle".into()
        },
        updated_ms: None,
        kind: text("kind"),
        model: None,
    })
}

async fn list(agent_dir: &Path) -> Result<Vec<HostSession>, String> {
    let outcome = run(target_of(agent_dir), HostSessionAction::List).await;
    if outcome.exit_code == 0 {
        return Ok(outcome.payload["sessions"]
            .as_array()
            .map(|rows| rows.iter().filter_map(host_session).collect())
            .unwrap_or_default());
    }
    match outcome.payload["reason"].as_str() {
        Some("host_unavailable") => Ok(Vec::new()),
        Some(reason) => Err(reason.to_owned()),
        None => Err("the host did not list its sessions".into()),
    }
}

/// The host action an engine record asks for; `None` for a method the host runner does not carry.
pub fn action_of(reference: &str, record: &Value) -> Option<HostSessionAction> {
    let reference = reference.to_owned();
    let text = || {
        record
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some(match record.get("type")?.as_str()? {
        "prompt" | "follow_up" => HostSessionAction::Prompt {
            reference,
            text: text(),
        },
        "steer" => HostSessionAction::Steer {
            reference,
            text: text(),
        },
        "abort" => HostSessionAction::Abort { reference },
        "set_model" => HostSessionAction::Model {
            reference,
            model: HostSessionModel {
                provider: record.get("provider")?.as_str()?.to_owned(),
                id: record.get("modelId")?.as_str()?.to_owned(),
            },
        },
        "get_state" => HostSessionAction::State { reference },
        "close_session" => HostSessionAction::Close { reference },
        _ => return None,
    })
}

/// A host outcome as a command outcome: 0 applied, 3 refused (an unknown session is no longer
/// listed), 2 a malformed record, anything else uncertain.
pub fn outcome_of(outcome: HostOutcome) -> HostCommandOutcome {
    match outcome.exit_code {
        0 => HostCommandOutcome::Applied(outcome.payload),
        3 => match outcome.payload["reason"].as_str() {
            Some("unknown_session") => HostCommandOutcome::NotListed,
            Some(reason) => HostCommandOutcome::Refused(reason.to_owned()),
            None => HostCommandOutcome::Refused("refused".into()),
        },
        2 => HostCommandOutcome::Refused("malformed".into()),
        _ => HostCommandOutcome::Uncertain("request_gone".into()),
    }
}

impl SessionHost for RpcSessionHost {
    fn list_sessions<'a>(
        &'a self,
        agent_dirs: &'a [PathBuf],
    ) -> BoxFuture<'a, Result<Vec<HostSession>, String>> {
        Box::pin(async move {
            let mut sessions = Vec::new();
            for agent_dir in self.agent_dirs(agent_dirs) {
                sessions.extend(list(&agent_dir).await?);
            }
            Ok(sessions)
        })
    }

    fn command<'a>(
        &'a self,
        agent_dirs: &'a [PathBuf],
        session_id: &'a str,
        record: &'a Value,
    ) -> BoxFuture<'a, HostCommandOutcome> {
        Box::pin(async move {
            let Some(action) = action_of(session_id, record) else {
                return HostCommandOutcome::Refused("method_unclassified".into());
            };
            for agent_dir in self.agent_dirs(agent_dirs) {
                let listed = list(&agent_dir).await.is_ok_and(|sessions| {
                    sessions.iter().any(|session| session.is_named(session_id))
                });
                if listed {
                    return outcome_of(run(target_of(&agent_dir), action).await);
                }
            }
            HostCommandOutcome::NotListed
        })
    }
}
