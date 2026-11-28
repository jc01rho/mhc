//! The engine host ensure bound to the native host (upstream `thread/wake/host-ensure.ts`).
//!
//! Upstream asks the engine CLI that runs this session, `host ensure --policy never --socket <socket> --agent-dir
//! <agentDir>`: a running host is reused whatever its build, and one is started from omo's launch spec only when
//! nothing answers. Two things that call needs are not on this side of the boundary yet:
//!
//! * `mhc host ensure` lives in `maho-cli`'s `host_command::run_host_command`, a different project's subsystem whose
//!   launch spec is not the wake's, so the ensure here starts nothing.
//! * `task.host_idle_exit_ms` is not exposed on the bridge, so the ensure passes no idle-exit override (owner ruling
//!   2026-11-14).
//!
//! A host that is not running is then reported by the connection that follows the ensure: the carrier's open answers
//! `host_unavailable:<socket>` and the row stays queued, which is the answer a failed ensure gives too. The narrow
//! [`EnsureHost`] return keeps the seam where the real spawn binds.

use std::sync::Arc;

use super::carrier::EnsureHost;

/// The carrier's `EnsureHost` on this host: an ensure that starts nothing, until `mhc host ensure` owns spawning the
/// operator host here (see the module note).
pub fn native_ensure_host() -> EnsureHost {
    Arc::new(|| -> Result<(), String> { Ok(()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ensure_host_starts_no_host_until_the_subsystem_owns_it() {
        let ensure = native_ensure_host();
        assert!(ensure().is_ok());
    }
}
