//! Port of `reflection/terminal-claim.ts` (omo #12517): the exclusive claim on a run's terminal
//! state. A complete, fsynced claim becomes visible through a filesystem-exclusive hard link, so an
//! unreadable claim authorized nothing and is discarded rather than stranding the run forever.

use std::fmt;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::locks::acquire::{AcquireLockOptions, WithLockError, with_lock};
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};
use crate::locks::process_identity::{ProcessLiveness, get_pid_liveness};
use crate::support::random::random_uuid;

/// The recovery lock is waited on without a deadline; this is the longest wait the lock clock holds.
const RECOVERY_LOCK_WAIT_MS: u64 = u32::MAX as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunTerminalClaimKind {
    Publish,
    Abandon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryReason {
    Invalid,
    DeadPublish,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTerminalClaim {
    pub version: u32,
    pub kind: RunTerminalClaimKind,
    pub run_id: String,
    pub attempt: i64,
    pub claimant_pid: u32,
}

/// Test seams: production reads the real process table and recovers without a hook.
#[derive(Default, Clone, Copy)]
pub struct RunTerminalClaimSeams<'a> {
    pub pid_liveness: Option<&'a dyn Fn(u32) -> ProcessLiveness>,
    pub before_recovery: Option<&'a dyn Fn(RecoveryReason)>,
}

/// The claim file can be neither read nor replaced, so no process can ever settle this run through it.
#[derive(Debug)]
pub struct RunTerminalClaimUnrecoverableError {
    pub run_dir: PathBuf,
    pub cause: Option<String>,
}

impl fmt::Display for RunTerminalClaimUnrecoverableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Run terminal claim cannot be recovered in {}",
            self.run_dir.display()
        )
    }
}

impl std::error::Error for RunTerminalClaimUnrecoverableError {}

#[derive(Debug)]
pub enum TerminalClaimError {
    Unrecoverable(RunTerminalClaimUnrecoverableError),
    Invalid,
    Io(std::io::Error),
    Lock(String),
}

impl fmt::Display for TerminalClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unrecoverable(error) => error.fmt(f),
            Self::Invalid => f.write_str("Invalid run terminal claim"),
            Self::Io(error) => error.fmt(f),
            Self::Lock(message) => write!(f, "terminal claim recovery lock failed: {message}"),
        }
    }
}

impl std::error::Error for TerminalClaimError {}

impl From<std::io::Error> for TerminalClaimError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

struct ClaimSnapshot {
    raw: String,
    claim: Option<RunTerminalClaim>,
}

pub fn run_terminal_claim_path(run_dir: &Path) -> PathBuf {
    run_dir.join("terminal-claim.json")
}

pub fn claim_run_terminal(
    run_dir: &Path,
    run_id: &str,
    attempt: i64,
    kind: RunTerminalClaimKind,
    seams: &RunTerminalClaimSeams<'_>,
    claimant_pid: u32,
) -> Result<RunTerminalClaim, TerminalClaimError> {
    let path = run_terminal_claim_path(run_dir);
    let requested = RunTerminalClaim {
        version: 1,
        kind,
        run_id: run_id.to_string(),
        attempt,
        claimant_pid,
    };
    let mut discarded = 0;
    loop {
        if create_exclusive_claim(&path, &requested)? {
            return Ok(requested);
        }
        let snapshot = match read_claim_snapshot(&path) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Err(TerminalClaimError::Unrecoverable(
                    RunTerminalClaimUnrecoverableError {
                        run_dir: run_dir.to_path_buf(),
                        cause: Some(error.to_string()),
                    },
                ));
            }
        };
        let Some(snapshot) = snapshot else {
            continue;
        };
        let Some(existing) = snapshot.claim.clone() else {
            if discarded > 0 {
                return Err(unrecoverable(run_dir));
            }
            discarded += 1;
            if let Some(hook) = seams.before_recovery {
                hook(RecoveryReason::Invalid);
            }
            if let Some(replacement) = recover_claim(
                run_dir,
                &path,
                &snapshot,
                RecoveryReason::Invalid,
                run_id,
                seams,
            )? {
                return Ok(replacement);
            }
            continue;
        };
        if run_terminal_claim_matches(&existing, run_id, attempt, Some(kind)) {
            return Ok(existing);
        }
        if existing.kind != RunTerminalClaimKind::Publish
            || !claimant_is_confirmed_dead(&existing, seams)
        {
            return Ok(existing);
        }
        if let Some(hook) = seams.before_recovery {
            hook(RecoveryReason::DeadPublish);
        }
        if let Some(replacement) = recover_claim(
            run_dir,
            &path,
            &snapshot,
            RecoveryReason::DeadPublish,
            run_id,
            seams,
        )? {
            return Ok(replacement);
        }
    }
}

fn unrecoverable(run_dir: &Path) -> TerminalClaimError {
    TerminalClaimError::Unrecoverable(RunTerminalClaimUnrecoverableError {
        run_dir: run_dir.to_path_buf(),
        cause: None,
    })
}

pub fn read_run_terminal_claim(run_dir: &Path) -> Result<RunTerminalClaim, TerminalClaimError> {
    let raw = std::fs::read_to_string(run_terminal_claim_path(run_dir))?;
    let value = serde_json::from_str::<Value>(&raw).map_err(|_| TerminalClaimError::Invalid)?;
    parse_run_terminal_claim(&value).ok_or(TerminalClaimError::Invalid)
}

pub fn run_terminal_claim_matches(
    claim: &RunTerminalClaim,
    run_id: &str,
    attempt: i64,
    kind: Option<RunTerminalClaimKind>,
) -> bool {
    claim.run_id == run_id && claim.attempt == attempt && kind.is_none_or(|kind| claim.kind == kind)
}

fn create_exclusive_claim(path: &Path, claim: &RunTerminalClaim) -> std::io::Result<bool> {
    let candidate = PathBuf::from(format!(
        "{}.{}.{}.candidate",
        path.display(),
        std::process::id(),
        random_uuid()
    ));
    write_durable_claim(&candidate, claim)?;
    let linked = std::fs::hard_link(&candidate, path);
    remove_claim(&candidate)?;
    match linked {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    }
    sync_directory(path.parent().unwrap_or_else(|| Path::new(".")))?;
    Ok(true)
}

fn write_durable_claim(path: &Path, claim: &RunTerminalClaim) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let mut bytes = serde_json::to_vec_pretty(claim)
        .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if written.is_err() {
        let _ = std::fs::remove_file(path);
    }
    written
}

fn read_claim_snapshot(path: &Path) -> std::io::Result<Option<ClaimSnapshot>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let claim = serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| parse_run_terminal_claim(&value));
    Ok(Some(ClaimSnapshot { raw, claim }))
}

fn recover_claim(
    run_dir: &Path,
    claim_path: &Path,
    inspected: &ClaimSnapshot,
    reason: RecoveryReason,
    run_id: &str,
    seams: &RunTerminalClaimSeams<'_>,
) -> Result<Option<RunTerminalClaim>, TerminalClaimError> {
    let record = create_lock_record(
        "reflection-finalize",
        CreateLockRecordOptions {
            run_id: Some(run_id.to_string()),
        },
    )
    .map_err(|error| TerminalClaimError::Lock(error.to_string()))?;
    let options = AcquireLockOptions {
        wait_timeout_ms: Some(RECOVERY_LOCK_WAIT_MS),
        ..Default::default()
    };
    with_lock(
        &run_dir.join("terminal-claim-recovery.lock"),
        &record,
        &options,
        || -> Result<Option<RunTerminalClaim>, TerminalClaimError> {
            let Some(current) = read_claim_snapshot(claim_path)? else {
                return Ok(None);
            };
            if current.raw != inspected.raw {
                return Ok(current.claim);
            }
            match reason {
                RecoveryReason::Invalid => {
                    if current.claim.is_some() {
                        return Ok(current.claim);
                    }
                }
                RecoveryReason::DeadPublish => {
                    let Some(claim) = current.claim else {
                        return Ok(None);
                    };
                    if claim.kind != RunTerminalClaimKind::Publish
                        || !claimant_is_confirmed_dead(&claim, seams)
                    {
                        return Ok(Some(claim));
                    }
                }
            }
            remove_claim(claim_path)?;
            Ok(None)
        },
    )
    .map_err(|error| match error {
        WithLockError::Acquire(error) => TerminalClaimError::Lock(error.to_string()),
        WithLockError::User(error) => error,
    })
}

fn remove_claim(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn claimant_is_confirmed_dead(claim: &RunTerminalClaim, seams: &RunTerminalClaimSeams<'_>) -> bool {
    let liveness = seams.pid_liveness.map_or_else(
        || get_pid_liveness(claim.claimant_pid),
        |check| check(claim.claimant_pid),
    );
    liveness == ProcessLiveness::Dead
}

fn parse_run_terminal_claim(value: &Value) -> Option<RunTerminalClaim> {
    let record = value.as_object()?;
    if record.get("version").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let kind = match record.get("kind").and_then(Value::as_str)? {
        "publish" => RunTerminalClaimKind::Publish,
        "abandon" => RunTerminalClaimKind::Abandon,
        _ => return None,
    };
    let run_id = record.get("runId").and_then(Value::as_str)?.to_string();
    let attempt = record.get("attempt").and_then(Value::as_f64)?;
    let claimant = record.get("claimantPid").and_then(Value::as_f64)?;
    if attempt.fract() != 0.0 || claimant.fract() != 0.0 || claimant <= 0.0 {
        return None;
    }
    Some(RunTerminalClaim {
        version: 1,
        kind,
        run_id,
        attempt: attempt as i64,
        claimant_pid: u32::try_from(claimant as u64).ok()?,
    })
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path).and_then(|directory| directory.sync_all())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTerminalPublication {
    Settled,
    Finalize,
    Veto,
}

/// What already settled a run, read before anyone claims it (DESIGN.r2 4.1a step iii): a terminal
/// sentinel, an outcome published for this attempt, or a publication in progress. `None` means
/// nothing has, and a terminal claim may be attempted.
pub fn read_run_terminal_publication(
    run_dir: &Path,
    attempt: Option<i64>,
) -> std::io::Result<Option<RunTerminalPublication>> {
    for name in ["final.json", "abandoned.json", "quarantined.json"] {
        if file_exists(&run_dir.join(name))? {
            return Ok(Some(RunTerminalPublication::Settled));
        }
    }
    let wanted = attempt.map(|attempt| attempt as f64);
    let attempt_matches = |fields: &Value| fields.get("attempt").and_then(Value::as_f64) == wanted;
    if let Some(outcome) = read_json_file(&run_dir.join("outcome.json"))?
        && attempt_matches(&outcome)
    {
        return Ok(Some(RunTerminalPublication::Finalize));
    }
    if let Some(publishing) = read_json_file(&run_dir.join("publishing.json"))?
        && publishing.get("version").and_then(Value::as_f64) == Some(1.0)
        && attempt_matches(&publishing)
    {
        return Ok(Some(RunTerminalPublication::Veto));
    }
    Ok(None)
}

fn file_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::read(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_json_file(path: &Path) -> std::io::Result<Option<Value>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(match serde_json::from_str::<Value>(&raw) {
        Ok(value) if value.is_object() => value,
        _ => Value::Object(serde_json::Map::new()),
    }))
}

#[cfg(test)]
#[path = "terminal_claim_tests.rs"]
mod tests;
