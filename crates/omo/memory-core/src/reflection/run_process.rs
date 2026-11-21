//! Port of `reflection/run-process.ts` (omo #12517): how a process a run ledger recorded is judged.

use crate::locks::process_identity::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_comparable,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedProcessVerdict {
    Alive,
    Dead,
    Unknown,
    Absent,
}

/// A recorded start identity in the three states a ledger can hold: absent (`undefined`), JSON
/// `null`, or a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedStart<'a> {
    Undefined,
    Null,
    Identity(&'a str),
}

/// Test seams: production reads the real process table.
#[derive(Default, Clone, Copy)]
pub struct RecordedProcessSeams<'a> {
    pub pid_liveness: Option<&'a dyn Fn(u32) -> ProcessLiveness>,
    pub process_start: Option<&'a dyn Fn(u32) -> Option<String>>,
}

pub type RecordedProcessClassifier =
    fn(Option<u64>, RecordedStart<'_>, &RecordedProcessSeams<'_>) -> RecordedProcessVerdict;

fn liveness_of(pid: u32, seams: &RecordedProcessSeams<'_>) -> ProcessLiveness {
    seams
        .pid_liveness
        .map_or_else(|| get_pid_liveness(pid), |check| check(pid))
}

fn start_verdict(
    pid: u32,
    recorded_start: RecordedStart<'_>,
    seams: &RecordedProcessSeams<'_>,
) -> RecordedProcessVerdict {
    let actual = seams
        .process_start
        .map_or_else(|| get_process_start_identity(pid), |start| start(pid));
    match (recorded_start, actual) {
        (RecordedStart::Identity(recorded), Some(actual)) => {
            if start_identities_comparable(recorded, &actual) && recorded != actual {
                RecordedProcessVerdict::Dead
            } else {
                RecordedProcessVerdict::Alive
            }
        }
        _ => RecordedProcessVerdict::Unknown,
    }
}

/// A pid beyond what the OS can name cannot be checked, so it stays `unknown` (fail-closed).
fn os_pid(pid: u64) -> Option<u32> {
    u32::try_from(pid).ok()
}

/// A process a VALID run ledger recorded, decided the way reconciliation always has: a ledger without
/// a recorded start is `unknown` before liveness is read, because the queued, launching and deadline
/// paths that consume this verdict treat `unknown` as "keep waiting".
pub fn classify_recorded_process(
    pid: Option<u64>,
    recorded_start: RecordedStart<'_>,
    seams: &RecordedProcessSeams<'_>,
) -> RecordedProcessVerdict {
    let Some(pid) = pid else {
        return RecordedProcessVerdict::Absent;
    };
    if recorded_start == RecordedStart::Undefined {
        return RecordedProcessVerdict::Unknown;
    }
    let Some(pid) = os_pid(pid) else {
        return RecordedProcessVerdict::Unknown;
    };
    match liveness_of(pid, seams) {
        ProcessLiveness::Dead => RecordedProcessVerdict::Dead,
        ProcessLiveness::Unknown => RecordedProcessVerdict::Unknown,
        ProcessLiveness::Alive => start_verdict(pid, recorded_start, seams),
    }
}

/// Liveness first, for an UNPARSEABLE or stranded ledger only (omo #11920): such a ledger has no
/// queued or launching semantics to protect, and a pid the kernel no longer knows is gone whatever
/// start was recorded. The start identity only decides whether a live pid was reused.
pub fn classify_recorded_process_lenient(
    pid: Option<u64>,
    recorded_start: RecordedStart<'_>,
    seams: &RecordedProcessSeams<'_>,
) -> RecordedProcessVerdict {
    let Some(pid) = pid else {
        return RecordedProcessVerdict::Absent;
    };
    let Some(pid) = os_pid(pid) else {
        return RecordedProcessVerdict::Unknown;
    };
    match liveness_of(pid, seams) {
        ProcessLiveness::Dead => RecordedProcessVerdict::Dead,
        ProcessLiveness::Unknown => RecordedProcessVerdict::Unknown,
        ProcessLiveness::Alive if recorded_start == RecordedStart::Undefined => {
            RecordedProcessVerdict::Unknown
        }
        ProcessLiveness::Alive => start_verdict(pid, recorded_start, seams),
    }
}

#[cfg(test)]
#[path = "run_process_tests.rs"]
mod tests;
