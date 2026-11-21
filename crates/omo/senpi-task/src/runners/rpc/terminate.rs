//! `runners/rpc/terminate.ts`: THE definition of RPC child termination (single-writer rule).
//! This is the only module that sends process signals to an RPC child.

use std::io;
use std::time::Duration;

use crate::runners::rpc::process::RpcChildProcess;
use crate::runners::types::TerminateOptions;

const DEFAULT_SIGKILL_DELAY_MS: u64 = 5_000;

/// Terminate the owned process tree. POSIX sends SIGTERM to the process group, waits up to the
/// delay, then SIGKILLs the survivors; a child without its own group is signalled directly.
pub fn terminate_rpc_child(child: &RpcChildProcess, options: TerminateOptions) -> io::Result<()> {
    let Some(pid) = child.pid() else {
        return Ok(());
    };
    let delay = Duration::from_millis(options.sigkill_delay_ms.unwrap_or(DEFAULT_SIGKILL_DELAY_MS));
    platform::terminate(child, pid, delay)
}

#[cfg(unix)]
mod platform {
    use super::{Duration, RpcChildProcess, io};

    pub(super) fn terminate(child: &RpcChildProcess, pid: u32, delay: Duration) -> io::Result<()> {
        let pid = libc::pid_t::try_from(pid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
        if !group_exists(pid) {
            return terminate_direct(child, pid, delay);
        }
        signal_group(pid, libc::SIGTERM)?;
        child.wait_exit_timeout(delay);
        if group_exists(pid) {
            signal_group(pid, libc::SIGKILL)?;
        }
        child.wait_exit();
        Ok(())
    }

    fn terminate_direct(
        child: &RpcChildProcess,
        pid: libc::pid_t,
        delay: Duration,
    ) -> io::Result<()> {
        if child.has_exited() {
            return Ok(());
        }
        signal(pid, libc::SIGTERM)?;
        if child.wait_exit_timeout(delay).is_none() {
            signal(pid, libc::SIGKILL)?;
        }
        child.wait_exit();
        Ok(())
    }

    fn group_exists(pid: libc::pid_t) -> bool {
        match signal(-pid, 0) {
            Ok(()) => true,
            Err(error) => error.raw_os_error() != Some(libc::ESRCH),
        }
    }

    fn signal_group(pid: libc::pid_t, signal_number: libc::c_int) -> io::Result<()> {
        match signal(-pid, signal_number) {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            other => other,
        }
    }

    fn signal(target: libc::pid_t, signal_number: libc::c_int) -> io::Result<()> {
        // SAFETY: kill(2) only reads its two integer arguments.
        if unsafe { libc::kill(target, signal_number) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use super::{Duration, RpcChildProcess, io};

    /// The one window the tree kill and the child's exit are observed in (#14300).
    const PROCESS_EXIT_OBSERVATION_TIMEOUT: Duration = Duration::from_millis(2_000);
    const PROCESS_EXIT_OBSERVATION_INTERVAL: Duration = Duration::from_millis(10);

    pub(super) fn terminate(child: &RpcChildProcess, pid: u32, _delay: Duration) -> io::Result<()> {
        if child.has_exited() {
            return Ok(());
        }
        let mut taskkill = Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        // taskkill.exe itself can hang (AV hooks, a tree member it cannot open): its completion is
        // not the cleanup contract, the child's exit is. Race the tool's end against the child's
        // exit inside one bounded window (#14300: an open-ended taskkill wait held a cleanup hook for
        // 46.8 s). A tree kill still running then is left to finish on its own: stopping it would end
        // /T /F partway and orphan descendants the child's own exit cannot reach.
        let deadline = Instant::now() + PROCESS_EXIT_OBSERVATION_TIMEOUT;
        loop {
            let exited = child
                .wait_exit_timeout(PROCESS_EXIT_OBSERVATION_INTERVAL)
                .is_some();
            if exited || taskkill.try_wait()?.is_some() || Instant::now() >= deadline {
                break;
            }
        }
        // The forced kill ends the process, but its exit is observed for a bounded window, never
        // waited on open-ended (#14106).
        child.wait_exit_timeout(PROCESS_EXIT_OBSERVATION_TIMEOUT);
        Ok(())
    }
}
