//! `rpc-host/session-client-opening.ts` + the `spawnedBy` half of `session-transport.ts`
//! (`HostSessionOpenInput.spawnedBy` / `toWireOpen`) and `open-session.ts` (`rpcChildAncestry`).
//!
//! The mhc assembly builds the `open_session` JSON itself (maho-cli `task_host.rs`); it calls
//! [`with_spawned_by`] on that request with the host's `get_protocol_info` engine version, and binds
//! the connection it dialed through the [`BindClient`] its owner handed it.

use std::sync::Arc;

use serde_json::Value;

use super::host_command_dispatch::HostRpcClient;
use super::session_wire::host_session_detached_error;
use crate::host::HostError;
use crate::runners::child_ancestry::rpc_child_ancestry;
use crate::runners::engine_spawned_by::engine_accepts_spawned_by;
use crate::runners::types::RpcRunnerSpec;

/// `SessionOpeningPorts.bindClient`: an open hands the connection it has just dialed to its owner.
/// `false` means the owner was released while that connection was still dialing: the open must stop
/// the connection and end with [`released_while_dialing`]. `None` unbinds after a failed open.
pub type BindClient = Arc<dyn Fn(Option<Arc<dyn HostRpcClient>>) -> bool + Send + Sync>;

/// `HostSessionDetachedError("open_session")`: how an open ends once its owner was released.
pub fn released_while_dialing() -> HostError {
    host_session_detached_error("open_session")
}

/// Adds the child's ancestry to an `open_session` request. No capability names `spawnedBy`, so the
/// host's engine version decides; an unproven host opens the child without ancestry (logged).
/// Returns whether the field was sent.
pub fn with_spawned_by(
    request: &mut Value,
    spec: &RpcRunnerSpec,
    host_engine_version: Option<&str>,
    socket: &str,
) -> bool {
    let Some(spawned_by) = rpc_child_ancestry(spec) else {
        return false;
    };
    if !engine_accepts_spawned_by(host_engine_version) {
        utils::logger::log(
            "senpi-task host engine does not accept spawnedBy; opening the child without ancestry",
            Some(&serde_json::json!({"socket": socket, "engineVersion": host_engine_version})),
        );
        return false;
    }
    request["spawnedBy"] = serde_json::json!(spawned_by);
    true
}

/// `assertHostUsable(info, retryFallback)` (omo #12759): under the designated-chain-only policy a host
/// that does not advertise `retry_fallback_profile` would run the child on its own fallback settings, so
/// it is refused as incompatible, naming the child's designated models.
pub fn designated_chain_host_refusal(
    capabilities: &Value,
    model: &str,
    fallback_models: &[String],
) -> Option<String> {
    let advertised = capabilities
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == "retry_fallback_profile"));
    (!advertised).then(|| {
        format!(
            "missing retry_fallback_profile; the child's designated models ({}) cannot be held to on this host",
            crate::runners::designated_fallback_profile::designated_chain_text(model, fallback_models)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // rpc-host-retry-fallback.test.ts (#12759): an older host is refused naming every designated model.
    #[test]
    fn a_host_without_retry_fallback_profile_is_refused_naming_the_designated_models() {
        let chain = vec!["openai/gpt-5.6-sol".to_string()];
        let refusal = designated_chain_host_refusal(
            &json!(["multi_session", "session_context"]),
            "anthropic/claude-opus-5-5",
            &chain,
        )
        .expect("refused");
        assert!(refusal.contains("missing retry_fallback_profile"));
        assert!(refusal.contains("(anthropic/claude-opus-5-5 -> openai/gpt-5.6-sol)"));
        assert_eq!(
            designated_chain_host_refusal(
                &json!(["multi_session", "retry_fallback_profile"]),
                "anthropic/claude-opus-5-5",
                &chain,
            ),
            None
        );
    }

    // rpc-host-spawned-by.test.ts
    #[test]
    fn open_carries_spawned_by_only_for_an_engine_that_accepts_it() {
        let spec = RpcRunnerSpec {
            task_id: "worker-task".into(),
            parent_session_id: Some("nested-parent".into()),
            root_session_id: Some("tree-root".into()),
            ..RpcRunnerSpec::default()
        };
        let mut accepted = json!({"sessionPath": "p", "cwd": "/w"});
        assert!(with_spawned_by(
            &mut accepted,
            &spec,
            Some("2026.10.25-2"),
            "sock"
        ));
        assert_eq!(
            accepted["spawnedBy"],
            json!({"parentSessionId": "nested-parent", "rootSessionId": "tree-root", "taskId": "worker-task"})
        );
        for version in [Some("2026.10.24-3"), Some("dev-build"), None] {
            let mut dropped = json!({"sessionPath": "p", "cwd": "/w"});
            assert!(!with_spawned_by(&mut dropped, &spec, version, "sock"));
            assert!(dropped.get("spawnedBy").is_none());
        }
        let orphan = RpcRunnerSpec::default();
        let mut none = json!({});
        assert!(!with_spawned_by(
            &mut none,
            &orphan,
            Some("2026.10.25-2"),
            "sock"
        ));
    }
}
