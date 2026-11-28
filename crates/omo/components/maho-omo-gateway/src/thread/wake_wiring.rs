//! The production seams the wake's session start needs: the carrier's host connection, the engine-host ensure and the
//! socket the carrier opens sessions on (upstream `thread/component.ts` `startWake`).
//!
//! The wake itself is `wake::wake_runtime::start_wake_at_session`; this module only fills the [`ThreadWakeOptions`] a
//! host leaves empty, so the thread component's `session_start` handler is one call. The registration-time cold wake
//! (upstream `hostSocketOf(pi)` + `isThreadSocket`) is [`cold_wake_on_operator_daemon`], which no host calls yet: the
//! extension context carries no `session_context`, so a process cannot name its own `host_socket`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::ExtensionMode;

use super::wake::carrier_connection::{carrier_connect, carrier_socket};
use super::wake::host_ensure::native_ensure_host;
use super::wake::runtime::{RunningThreadWake, ThreadWakeOptions};
use super::wake::wake_runtime::{WakeAtSessionInput, start_wake_at_session};

/// One `session_start` of the thread component: the engine's mode, the agent directory, and the env the wake reads.
pub struct WakeStartAtSessionArgs {
    pub mode: ExtensionMode,
    pub agent_dir: PathBuf,
    pub sessions_directory: PathBuf,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub engine_version: Option<String>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}

/// The wake options a host fills itself: the carrier's authenticated host connection and the engine-host ensure. Every
/// other seam stays empty, so the running wake opens its own store and reads the machine's `session_wake`.
pub fn wired_wake_options(agent_dir: &Path, env: BTreeMap<String, String>) -> ThreadWakeOptions {
    ThreadWakeOptions {
        env,
        latch: None,
        store: None,
        connect: carrier_connect(carrier_socket(agent_dir), agent_dir.to_path_buf()),
        ensure_host: Some(native_ensure_host()),
        deliveries: None,
        create_consumer: None,
        engine_version: None,
        minimum_engine: None,
        chain_count_limit: None,
        memory_headroom: None,
        schedule: None,
        on_wake: None,
    }
}

/// Starts the process's wake for this agent directory at a session start, with the production seams. Answers the
/// running wake, or `None` when this session starts none (a mode other than `tui`/`rpc`, CI, a task child, an engine
/// below the wake minimum).
pub fn start_wake_at_session_wired(args: WakeStartAtSessionArgs) -> Option<Arc<RunningThreadWake>> {
    let WakeStartAtSessionArgs {
        mode,
        agent_dir,
        sessions_directory,
        cwd,
        env,
        engine_version,
        warn,
    } = args;
    start_wake_at_session(
        mode_name(mode),
        WakeAtSessionInput {
            options: wired_wake_options(&agent_dir, env),
            agent_dir,
            sessions_directory,
            cwd,
            engine_version,
            warn,
        },
    )
}

/// `ExtensionMode` on the wire: `"tui" | "rpc" | "json" | "print"` (the strings `maho-tool-child` serialises), which
/// is what the wake's mode gate reads.
fn mode_name(mode: ExtensionMode) -> &'static str {
    match mode {
        ExtensionMode::Tui => "tui",
        ExtensionMode::Rpc => "rpc",
        ExtensionMode::Json => "json",
        ExtensionMode::Print => "print",
    }
}

/// `isThreadSocket(hostSocket)` (upstream `live-surface-paths.ts`): whether this process's own `host_socket` is the
/// operator daemon's, the host `thread_create` opens sessions on. The caller resolves the thread socket, because the
/// extension context carries no `session_context` to read it from.
pub fn is_thread_socket(own_host_socket: Option<&str>, thread_socket: &str) -> bool {
    own_host_socket.is_some_and(|own| canonical_socket(own) == canonical_socket(thread_socket))
}

/// Whether the wake starts at registration rather than at a session start: only the operator daemon does, because a
/// host the device service cold-starts has no session yet (omo#13037 cold wake).
pub fn cold_wake_on_operator_daemon(own_host_socket: Option<&str>, agent_dir: &Path) -> bool {
    is_thread_socket(own_host_socket, &carrier_socket(agent_dir))
}

/// A socket path as the host stamps it: its directory canonicalised, its own name kept.
fn canonical_socket(socket: &str) -> PathBuf {
    let path = Path::new(socket);
    let (Some(directory), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    match std::fs::canonicalize(directory) {
        Ok(canonical) => canonical.join(name),
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::wake::runtime::OMO_SENPI_TASK_RPC_CHILD;

    fn args(mode: ExtensionMode, env: BTreeMap<String, String>) -> WakeStartAtSessionArgs {
        let dir = std::env::temp_dir().join("wake-wiring-test");
        WakeStartAtSessionArgs {
            mode,
            agent_dir: dir.clone(),
            sessions_directory: dir.join("sessions"),
            cwd: dir,
            env,
            engine_version: Some("2026.12.1".to_owned()),
            warn: Arc::new(|_| {}),
        }
    }

    #[test]
    fn the_engine_modes_are_named_as_the_wire_names_them() {
        assert_eq!(mode_name(ExtensionMode::Tui), "tui");
        assert_eq!(mode_name(ExtensionMode::Rpc), "rpc");
        assert_eq!(mode_name(ExtensionMode::Json), "json");
        assert_eq!(mode_name(ExtensionMode::Print), "print");
    }

    #[test]
    fn a_mode_that_is_neither_tui_nor_rpc_starts_no_wake() {
        assert!(
            start_wake_at_session_wired(args(ExtensionMode::Json, Default::default())).is_none()
        );
    }

    #[test]
    fn ci_starts_no_wake() {
        let mut env = BTreeMap::new();
        env.insert("CI".to_owned(), "1".to_owned());
        assert!(start_wake_at_session_wired(args(ExtensionMode::Tui, env)).is_none());
    }

    #[test]
    fn a_task_child_starts_no_wake() {
        let mut env = BTreeMap::new();
        env.insert(OMO_SENPI_TASK_RPC_CHILD.to_owned(), "1".to_owned());
        assert!(start_wake_at_session_wired(args(ExtensionMode::Rpc, env)).is_none());
    }

    #[test]
    fn an_engine_below_the_wake_minimum_starts_no_wake() {
        let mut start = args(ExtensionMode::Tui, Default::default());
        start.engine_version = Some("2026.11.13".to_owned());
        assert!(start_wake_at_session_wired(start).is_none());
    }

    #[test]
    fn the_wired_options_carry_the_host_connection_and_the_ensure() {
        let dir = std::env::temp_dir().join("wake-wiring-socket");
        let options = wired_wake_options(&dir, Default::default());
        let connect = options.connect.clone();
        let connection = connect().expect("a connection object opens without a host");
        connection.close();
        assert!(options.ensure_host.is_some());
    }

    #[test]
    fn the_operator_daemons_own_socket_is_the_thread_socket() {
        let dir = std::env::temp_dir().join("wake-wiring-daemon");
        let socket = carrier_socket(&dir);
        assert!(cold_wake_on_operator_daemon(Some(socket.as_str()), &dir));
    }

    #[test]
    fn another_host_is_not_the_operator_daemon() {
        let dir = std::env::temp_dir().join("wake-wiring-other");
        // A socket beside the operator's, whichever socket the brand's `RPC_SOCKET` names.
        let other = format!("{}.shard", carrier_socket(&dir));
        assert!(!cold_wake_on_operator_daemon(Some(other.as_str()), &dir));
    }

    #[test]
    fn a_process_that_names_no_socket_is_not_the_operator_daemon() {
        let dir = std::env::temp_dir().join("wake-wiring-none");
        assert!(!cold_wake_on_operator_daemon(None, &dir));
    }

    #[test]
    fn a_socket_is_compared_through_its_canonical_directory() {
        let dir = std::env::temp_dir().join("wake-wiring-canonical");
        std::fs::create_dir_all(&dir).expect("the test makes its directory");
        let direct = dir.join("rpc.sock").to_string_lossy().into_owned();
        let spelled = dir
            .join(".")
            .join("rpc.sock")
            .to_string_lossy()
            .into_owned();
        assert!(is_thread_socket(Some(spelled.as_str()), direct.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
