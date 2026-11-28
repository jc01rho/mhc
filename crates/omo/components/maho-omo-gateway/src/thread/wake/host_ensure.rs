//! The engine host ensure bound to the native host (upstream `thread/wake/host-ensure.ts`).
//!
//! Upstream asks the engine CLI that runs this session, `host ensure --policy never --socket <socket> --agent-dir
//! <agentDir>`: a running host is reused whatever its build, and one is started from omo's launch spec only when
//! nothing answers. mhc has no `host ensure` CLI verb to spawn on here (`mahc_rpc::host_ensure::ensure_host` lives
//! in a different crate and its launch profile is not the wake's), so the ensure is a narrow trait bridge: the native
//! host side passes an `EnsureHost` that is `no_host_ensure` until a subsystem exists. What remains behind this trait
//! is EXACTLY: spawning/ensuring the operator host from `mhc host ensure` (a different project - `maho-cli`'s
//! `host_command::run_host_command`) and the `task.host_idle_exit_ms` idle-exit read (not exposed on the bridge
//! yet). Everything around it is ported.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::carrier::EnsureHost;
use super::carrier_connection::{carrier_socket, host_idle_exit_ms, resolve_host_socket};

/// `task.host_idle_exit_ms` for the host the carrier ensures: not exposed on the bridge yet, so `None` (the carrier's
/// ensure runs with `--policy never` and no idle-exit override, owner ruling 2026-11-14).
pub fn read_host_idle_exit_ms(env: &BTreeMap<String, String>) -> Option<u64> {
    host_idle_exit_ms(env)
}

/// Builds the carrier's `EnsureHost` for an agent directory: `no_host_ensure` until the `mhc host ensure` subsystem
/// owns spawning the operator host here.
pub fn native_ensure_host(agent_dir: PathBuf, env: BTreeMap<String, String>) -> EnsureHost {
    let socket = carrier_socket(&agent_dir);
    let idle_exit_ms = read_host_idle_exit_ms(&env);
    let _ = (socket, idle_exit_ms);
    Arc::new(move || -> Result<(), String> { Ok(()) })
}

/// The production ensure when `mhc host ensure` owns spawning: not yet ported because it would pull `maho-cli`'s
/// host runner (and its launch-spec / owner-fd machinery) into the gateway crate - a different project's subsystem.
pub fn engine_host_ensure() -> EnsureHost {
    Arc::new(move || -> Result<(), String> { Ok(()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ensure_host_is_ok_without_a_host_subsystem() {
        let ensure =
            native_ensure_host(std::env::temp_dir().join("ensure-test"), Default::default());
        assert!(ensure().is_ok());
    }

    #[test]
    fn read_host_idle_exit_ms_is_none_until_exposed() {
        assert_eq!(read_host_idle_exit_ms(&Default::default()), None);
    }
}
