//! Native thread component wake wiring: start at session_start, stop at shutdown.
//!
//! Upstream counterpart: `thread/component.ts` `startWake` (wake runtime + delivery consumer at session_start;
//! cold wake on the operator daemon). Keeps edits to `thread/component.rs` and `native.rs` to one-line calls.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::wake::carrier_connection::carrier_socket;
use super::wake::host_ensure::native_ensure_host;
use super::wake::runtime::{ThreadWakeOptions, ThreadWakeStart, start_thread_wake_at_session};
use super::wake::wake_runtime::{WakeAtSessionInput, start_wake_at_session};

pub struct WakeStartAtSessionArgs {
    pub mode: String,
    pub agent_dir: PathBuf,
    pub sessions_directory: PathBuf,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub engine_version: Option<String>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}

pub fn start_wake_at_session_wired(
    args: WakeStartAtSessionArgs,
) -> Option<Arc<super::wake::runtime::RunningThreadWake>> {
    let socket = carrier_socket(&args.agent_dir);
    let options = ThreadWakeOptions {
        env: args.env.clone(),
        latch: None,
        store: None,
        connect: super::wake::carrier_connection::carrier_connect(socket, 30_000),
        ensure_host: Some(native_ensure_host(args.agent_dir.clone(), args.env.clone())),
        deliveries: None,
        create_consumer: None,
        engine_version: args.engine_version.clone(),
        minimum_engine: None,
        chain_count_limit: None,
        memory_headroom: None,
        schedule: None,
        on_wake: None,
    };
    start_thread_wake_at_session(
        &args.mode,
        ThreadWakeStart {
            options,
            agent_dir: args.agent_dir,
            sessions_directory: args.sessions_directory,
            settings: super::wake::runtime::wake_settings_reader(
                args.env.clone(),
                args.cwd.clone(),
            ),
            engine_version: args.engine_version,
            warn: args.warn,
        },
    )
}

/// The cold wake on the operator daemon: the component starts the wake (and with it the delivery consumer) right at
/// registration when this session runs in the operator daemon (`host_socket` equals the operator socket `rpc.sock`);
/// any other host waits for `session_start`. Mirrors upstream's `hostSocketOf(pi)` + `isThreadSocket(hostSocket)`.
pub fn cold_wake_on_operator_daemon(env: &BTreeMap<String, String>, agent_dir: &PathBuf) -> bool {
    let socket = carrier_socket(agent_dir);
    let own = env.get("RPC_SOCKET").filter(|value| !value.is_empty());
    own.as_deref() == Some(socket.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_wake_on_operator_daemon_matches_own_socket() {
        let dir = std::env::temp_dir().join("cold-wake-test");
        let socket = carrier_socket(&dir);
        let mut env = BTreeMap::new();
        env.insert("RPC_SOCKET".into(), socket.clone());
        assert!(cold_wake_on_operator_daemon(&env, &dir));
    }
}
