//! omo `runners/rpc-host/read-rpc-diagnostic.ts` (omo 1c280530f6, omo#14498): an opt-in line for a failed local
//! read RPC. `OMO_THREAD_READ_TRACE=1` in the calling process turns it on. The line carries only the command, the
//! last socket phase, the elapsed and caller-stall milliseconds, a fixed code and the attempt count; never the
//! endpoint, a frame, auth bytes or the raw error message.

use std::sync::{Mutex, MutexGuard, PoisonError};

pub const READ_TRACE_ENV: &str = "OMO_THREAD_READ_TRACE";

/// Read commands answer the same however often they are sent: the ones a trace names and a dropped connection
/// redials.
const READ_COMMANDS: [&str; 4] = [
    "list_sessions",
    "get_protocol_info",
    "get_messages",
    "get_entries",
];

const SOCKET_CODES: [&str; 5] = ["EPIPE", "ECONNRESET", "ECONNREFUSED", "ENOENT", "ETIMEDOUT"];

pub fn is_read_command(command: Option<&str>) -> bool {
    command.is_some_and(|command| READ_COMMANDS.contains(&command))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadRpcPhase {
    Connect,
    Connected,
    Handshake,
    Request,
    ResponseBytes,
    ResponseFrame,
}

impl ReadRpcPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Connected => "connected",
            Self::Handshake => "handshake",
            Self::Request => "request",
            Self::ResponseBytes => "response-bytes",
            Self::ResponseFrame => "response-frame",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Progress {
    phase: ReadRpcPhase,
    attempts: u32,
}

/// One traced read call: the last phase its socket reached and how many connections it opened.
#[derive(Debug)]
pub struct ReadRpcDiagnostic {
    command: String,
    started_ms: f64,
    progress: Mutex<Progress>,
}

/// `Some` only when `trace` is exactly `"1"` and `command` is a read command.
pub fn read_rpc_diagnostic(
    trace: Option<&str>,
    command: Option<&str>,
    started_ms: f64,
) -> Option<ReadRpcDiagnostic> {
    if trace != Some("1") {
        return None;
    }
    let command = command.filter(|command| READ_COMMANDS.contains(command))?;
    Some(ReadRpcDiagnostic {
        command: command.to_owned(),
        started_ms,
        progress: Mutex::new(Progress {
            phase: ReadRpcPhase::Connect,
            attempts: 1,
        }),
    })
}

impl ReadRpcDiagnostic {
    pub fn at(&self, phase: ReadRpcPhase) {
        self.progress().phase = phase;
    }

    /// The connection was lost to a caller stall and is dialed again: a new conversation starts at `connect`.
    pub fn redialed(&self) {
        let mut progress = self.progress();
        progress.attempts += 1;
        progress.phase = ReadRpcPhase::Connect;
    }

    /// The `[thread-read-rpc]` line for a transport failure: `errno` is the socket error's code, `timed_out` whether
    /// the call's own deadline ended it, `stalled_ms` how long this process could not run during the call.
    pub fn failed(
        &self,
        errno: Option<&str>,
        timed_out: bool,
        stalled_ms: f64,
        now_ms: f64,
    ) -> String {
        let Progress { phase, attempts } = *self.progress();
        let fallback = if timed_out { "DEADLINE" } else { "OTHER" };
        let code = errno
            .and_then(|errno| SOCKET_CODES.iter().copied().find(|known| *known == errno))
            .unwrap_or(fallback);
        let command = &self.command;
        let phase = phase.as_str();
        let elapsed = (now_ms - self.started_ms).round() as i64;
        let stalled = stalled_ms.round() as i64;
        format!(
            "[thread-read-rpc] command={command} phase={phase} elapsed_ms={elapsed} code={code} caller_stall_ms={stalled} attempts={attempts}"
        )
    }

    fn progress(&self) -> MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
