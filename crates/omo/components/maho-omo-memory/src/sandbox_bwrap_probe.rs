//! Port of `sandbox-bwrap-probe.ts` (omo #6873, #12378): whether a resolved sandbox executable can
//! actually start a sandbox, not merely whether it exists. On Ubuntu 24.04+ with
//! `kernel.apparmor_restrict_unprivileged_userns=1`, /usr/bin/bwrap is present and executable while
//! every invocation dies with `bwrap: setting up uid map: Permission denied`, so an existence check
//! alone selects a sandbox that kills every child at spawn.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxUsability {
    Usable,
    Unusable { reason: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BwrapSmokeResult {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub error_message: Option<String>,
    pub stderr: String,
}

const SMOKE_TIMEOUT_MS: u64 = 3_000;
const REASON_STDERR_CHARS: usize = 200;
const SMOKE_STDERR_GRACE: Duration = Duration::from_secs(1);

/// The smallest invocation that still exercises user-namespace setup: bwrap must unshare and write
/// its uid/gid maps before it can exec the inner command, which is exactly the step AppArmor blocks.
const SMOKE_ARGS: [&str; 8] = [
    "--ro-bind",
    "/",
    "/",
    "--proc",
    "/proc",
    "--dev",
    "/dev",
    "true",
];

pub fn classify_bwrap_smoke(result: &BwrapSmokeResult) -> SandboxUsability {
    if result.timed_out {
        return unusable(
            &format!("smoke test timed out after {SMOKE_TIMEOUT_MS}ms"),
            &result.stderr,
        );
    }
    if let Some(message) = &result.error_message {
        return unusable(
            &format!("smoke test could not run: {message}"),
            &result.stderr,
        );
    }
    match result.exit_code {
        Some(0) => SandboxUsability::Usable,
        Some(code) => unusable(&format!("smoke test exited {code}"), &result.stderr),
        None => unusable("smoke test exited without an exit code", &result.stderr),
    }
}

/// One DEFINITIVE verdict per executable path: the smoke exited (usable or not) or could not be
/// spawned. A timeout is transient (a loaded host, a slow first start), so it is returned to its
/// caller but never memoized; the next probe, which reflection runs once per spawn, runs the smoke
/// again. The lock is held across the smoke, so concurrent callers still trigger one spawn.
pub struct BwrapProbe {
    verdicts: Mutex<BTreeMap<String, SandboxUsability>>,
}

impl BwrapProbe {
    pub const fn new() -> Self {
        Self {
            verdicts: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn probe(
        &self,
        executable: &str,
        smoke: &dyn Fn(&str) -> BwrapSmokeResult,
    ) -> SandboxUsability {
        let mut verdicts = self.verdicts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(verdict) = verdicts.get(executable) {
            return verdict.clone();
        }
        let result = smoke(executable);
        let verdict = classify_bwrap_smoke(&result);
        if !result.timed_out {
            verdicts.insert(executable.to_string(), verdict.clone());
        }
        verdict
    }
}

impl Default for BwrapProbe {
    fn default() -> Self {
        Self::new()
    }
}

static PROCESS_PROBE: BwrapProbe = BwrapProbe::new();

pub fn probe_bwrap_usability(executable: &str) -> SandboxUsability {
    PROCESS_PROBE.probe(executable, &run_bwrap_smoke)
}

pub(crate) fn run_bwrap_smoke(executable: &str) -> BwrapSmokeResult {
    let mut child = match Command::new(executable)
        .args(SMOKE_ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return BwrapSmokeResult {
                error_message: Some(error.to_string()),
                ..BwrapSmokeResult::default()
            };
        }
    };
    let (sender, receiver) = mpsc::channel();
    if let Some(mut stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            let _ = sender.send(bytes);
        });
    }
    let deadline = Instant::now() + Duration::from_millis(SMOKE_TIMEOUT_MS);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return BwrapSmokeResult {
                    timed_out: true,
                    ..BwrapSmokeResult::default()
                };
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return BwrapSmokeResult {
                    error_message: Some(error.to_string()),
                    ..BwrapSmokeResult::default()
                };
            }
        }
    };
    let stderr = receiver
        .recv_timeout(SMOKE_STDERR_GRACE)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    BwrapSmokeResult {
        exit_code: status.code(),
        timed_out: false,
        error_message: None,
        stderr,
    }
}

fn unusable(cause: &str, stderr: &str) -> SandboxUsability {
    let tail = last_chars(stderr.trim(), REASON_STDERR_CHARS);
    SandboxUsability::Unusable {
        reason: if tail.is_empty() {
            cause.to_string()
        } else {
            format!("{cause}: {tail}")
        },
    }
}

fn last_chars(text: &str, limit: usize) -> &str {
    let count = text.chars().count();
    if count <= limit {
        return text;
    }
    text.char_indices()
        .nth(count - limit)
        .map_or(text, |(index, _)| &text[index..])
}

#[cfg(test)]
#[path = "sandbox_bwrap_probe_tests.rs"]
mod tests;
