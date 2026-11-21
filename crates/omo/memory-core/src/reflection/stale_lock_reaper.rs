//! Port of `reflection/stale-lock-reaper.ts` (omo #11976, #11920, #12517): one stale reflection-lock
//! reaper for session start, each turn and `doctor --fix`.
//!
//! It owns one decision table over `runtime/reflection/` (DESIGN 4.1) and an injectable settle: a
//! session settles through its reconciliation (worktree-tip recovery included), the CLI through the
//! minimal writers here. Anything not proven gone is kept and reported. Every rule carries a
//! `rule:` tag naming the scenario its test replays.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use regex::Regex;
use serde_json::{Map, Value};

use crate::fs::remove_tree;
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, WithLockError, with_lock};
use crate::locks::dead_owner_sweep::sweep_dead_owner_locks;
use crate::locks::domains::{reflection_scheduler_lock_path, run_finalization_lock_path};
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};
use crate::locks::process_identity::ProcessLiveness;
use crate::receipts::receipts::{MemoryReceiptInput, append_memory_receipt_once};
use crate::support::host::hostname as read_hostname;
use crate::support::time::{format_rfc3339_millis, now_millis, parse_rfc3339};

use super::abandoned::{AbandonedLauncher, AbandonedReason, AbandonedRecord, write_abandoned};
use super::orphan_sweep::REFLECTION_ORPHAN_GRACE_MS;
use super::quarantine::{
    QuarantineReason, QuarantineRecord, RESERVATION_EVIDENCE_FILENAME, write_quarantine,
};
use super::release_without_promotion::{
    ReleaseError, ReleaseWithoutPromotionOptions, ReleaseWithoutPromotionResult,
    release_reservation_without_promotion,
};
use super::reservation::{is_reserved_run, move_torn_reservation_aside};
use super::run_id_high_water::{
    RunIdHighWaterPaths, record_reflection_run_id_mark, scan_reflection_run_high_water,
};
use super::run_ledger::parse_reservation_run_ledger;
use super::run_process::{
    RecordedProcessClassifier, RecordedProcessSeams, RecordedProcessVerdict, RecordedStart,
    classify_recorded_process, classify_recorded_process_lenient,
};
use super::stale_holder_proof::{
    ReflectionLockProbe, ReflectionLockVerdict, classify_reflection_lock_holder,
    reflection_launcher_liveness,
};
use super::terminal_claim::{
    RunTerminalClaimKind, RunTerminalClaimSeams, RunTerminalPublication, claim_run_terminal,
    read_run_terminal_publication, run_terminal_claim_matches,
};

pub const REFLECTION_LAUNCH_WINDOW_MS: f64 = 60_000.0;
pub const STRANDED_RUN_GRACE_MS: f64 = REFLECTION_LAUNCH_WINDOW_MS + REFLECTION_ORPHAN_GRACE_MS;
pub const TORN_LOCK_MIN_AGE_MS: f64 = 60.0 * 60_000.0;

const TERMINAL_SENTINELS: [&str; 3] = ["final.json", "abandoned.json", "quarantined.json"];
const TOMBSTONE_PREFIX: &str = ".prune-";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const STILL_LAUNCHING: &str =
    "its supervisor is still launching and its hard deadline has not passed";

static TORN_RUN_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"reflection-run-(\d+)""#).expect("static pattern"));
static TORN_LAUNCHER_PID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""launcherPid"\s*:\s*(\d+)"#).expect("static pattern"));
static TORN_LAUNCHER_HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""launcherHostname"\s*:\s*"([^"]*)""#).expect("static pattern"));
static TORN_LAUNCHER_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""launcherProcessStart"\s*:\s*"([^"]*)""#).expect("static pattern")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleLockReapAction {
    LaunchInterrupted,
    ProcessesDead,
    LedgerUnreadable,
    LockTorn,
    TerminalRecorded,
    ReleaseSettled,
}

impl StaleLockReapAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LaunchInterrupted => "launch-interrupted",
            Self::ProcessesDead => "processes-dead",
            Self::LedgerUnreadable => "ledger-unreadable",
            Self::LockTorn => "lock-torn",
            Self::TerminalRecorded => "terminal-recorded",
            Self::ReleaseSettled => "release-settled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleLockDecision {
    None,
    Finalize {
        run_id: String,
    },
    Stuck {
        run_id: String,
        reason: String,
    },
    Reap {
        action: StaleLockReapAction,
        run_id: String,
        reason: String,
    },
}

impl StaleLockDecision {
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Finalize { run_id } | Self::Stuck { run_id, .. } | Self::Reap { run_id, .. } => {
                Some(run_id)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrandedRunAction {
    NoLedger,
    ProcessesDead,
    LedgerUnreadable,
}

impl StrandedRunAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoLedger => "no-ledger",
            Self::ProcessesDead => "processes-dead",
            Self::LedgerUnreadable => "ledger-unreadable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrandedRunDecision {
    Keep {
        reason: String,
    },
    Reap {
        action: StrandedRunAction,
        reason: String,
    },
}

/// The lock-holder probe plus the clock and directory seams the reaper reads through.
#[derive(Default, Clone, Copy)]
pub struct StaleLockProbe<'a> {
    pub base: ReflectionLockProbe<'a>,
    pub now: Option<&'a dyn Fn() -> f64>,
    /// Names of the subdirectories of the path; empty when it does not exist.
    pub list_directory: Option<&'a dyn Fn(&Path) -> Vec<String>>,
    pub modified_at: Option<&'a dyn Fn(&Path) -> Option<f64>>,
}

impl<'a> StaleLockProbe<'a> {
    fn now(&self) -> f64 {
        self.now
            .map_or_else(|| now_millis() as f64, |clock| clock())
    }

    fn hostname(&self) -> String {
        self.base.hostname.map_or_else(read_hostname, |host| host())
    }

    fn recorded_seams(&self) -> RecordedProcessSeams<'a> {
        RecordedProcessSeams {
            pid_liveness: self.base.pid_liveness,
            process_start: self.base.process_start,
        }
    }
}

enum TextRead {
    Missing,
    Unreadable,
    Text(String),
}

fn read_text(path: &Path, probe: &StaleLockProbe<'_>) -> TextRead {
    let read = match probe.base.read_text {
        Some(read) => read(path),
        None => std::fs::read_to_string(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "ENOENT".to_string()
            } else {
                error.to_string()
            }
        }),
    };
    match read {
        Ok(text) => TextRead::Text(text),
        Err(code) if code == "ENOENT" || code == "NotFound" => TextRead::Missing,
        Err(_) => TextRead::Unreadable,
    }
}

fn parse_json(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

fn list_directory(path: &Path, probe: &StaleLockProbe<'_>) -> Vec<String> {
    if let Some(list) = probe.list_directory {
        return list(path);
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn modified_at(path: &Path, probe: &StaleLockProbe<'_>) -> Option<f64> {
    if let Some(modified) = probe.modified_at {
        return modified(path);
    }
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs_f64() * 1000.0)
}

fn exists(path: &Path, probe: &StaleLockProbe<'_>) -> bool {
    !matches!(read_text(path, probe), TextRead::Missing)
}

fn has_terminal_sentinel(run_dir: &Path, probe: &StaleLockProbe<'_>) -> bool {
    TERMINAL_SENTINELS
        .iter()
        .any(|name| exists(&run_dir.join(name), probe))
}

fn run_dir_name(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The outcome a `final.json` records: `None` when there is none, `"unknown"` when it cannot be read.
fn read_final_outcome(run_dir: &Path, probe: &StaleLockProbe<'_>) -> Option<String> {
    let parsed = match read_text(&run_dir.join("final.json"), probe) {
        TextRead::Missing => return None,
        TextRead::Unreadable => None,
        TextRead::Text(text) => parse_json(&text),
    };
    let outcome = parsed
        .as_ref()
        .and_then(|value| value.get("outcome"))
        .and_then(Value::as_str);
    Some(outcome.unwrap_or("unknown").to_string())
}

enum LedgerRead {
    Absent,
    Present { fields: Value, valid: bool },
}

fn valid_ledger(value: &Value, run_id: &str) -> bool {
    parse_reservation_run_ledger(value.clone()).is_ok_and(|ledger| ledger.run_id() == run_id)
}

fn read_ledger(run_dir: &Path, probe: &StaleLockProbe<'_>) -> LedgerRead {
    let parsed = match read_text(&run_dir.join("ledger.json"), probe) {
        TextRead::Missing => return LedgerRead::Absent,
        TextRead::Unreadable => None,
        TextRead::Text(text) => parse_json(&text),
    };
    match parsed {
        Some(value) => {
            let valid = valid_ledger(&value, &run_dir_name(run_dir));
            let fields = if value.is_object() {
                value
            } else {
                Value::Null
            };
            LedgerRead::Present { fields, valid }
        }
        None => LedgerRead::Present {
            fields: Value::Null,
            valid: false,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettledSentinel {
    Match,
    OtherGeneration,
}

fn iso_millis(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
        .map(|millis| millis as f64)
}

/// An abandoned.json or quarantined.json that settles THIS reservation (DESIGN.r3 13b c.14): same run
/// id, and a generation no older than the reservation (or equal to the ledger's start).
fn settled_sentinel(
    run_dir: &Path,
    run_id: &str,
    active: &Value,
    probe: &StaleLockProbe<'_>,
) -> Option<SettledSentinel> {
    for name in ["abandoned.json", "quarantined.json"] {
        let sentinel = match read_text(&run_dir.join(name), probe) {
            TextRead::Missing => continue,
            TextRead::Unreadable => Value::Null,
            TextRead::Text(text) => parse_json(&text).unwrap_or(Value::Null),
        };
        let generation_text = sentinel.get("generation").and_then(Value::as_str);
        let named = sentinel.get("runId").and_then(Value::as_str) == Some(run_id);
        let (true, Some(generation_text)) = (named, generation_text) else {
            return Some(SettledSentinel::OtherGeneration);
        };
        let generation = iso_millis(sentinel.get("generation"));
        let reserved_at = iso_millis(active.get("reservedAt"));
        let started_at = match read_ledger(run_dir, probe) {
            LedgerRead::Present { fields, .. } => fields
                .get("startedAt")
                .and_then(Value::as_str)
                .map(str::to_string),
            LedgerRead::Absent => None,
        };
        let not_older = matches!((generation, reserved_at), (Some(g), Some(r)) if g >= r);
        return if not_older || started_at.as_deref() == Some(generation_text) {
            Some(SettledSentinel::Match)
        } else {
            Some(SettledSentinel::OtherGeneration)
        };
    }
    None
}

/// The session's first reconcile check (DESIGN.r3 13b c.14): the active reservation's launcher is dead
/// on this host and its run already has a sentinel for this generation, so only the release is missing.
pub fn is_settled_under_dead_launcher(reflection_dir: &Path, probe: &StaleLockProbe<'_>) -> bool {
    matches!(
        decide_stale_reflection_lock(reflection_dir, probe).1,
        StaleLockDecision::Reap {
            action: StaleLockReapAction::ReleaseSettled,
            ..
        }
    )
}

enum RecordedProcesses {
    NotLive,
    Live(String),
}

/// A pid read from a ledger. One the OS cannot name maps to a value no process table holds, which
/// the classifiers read as "could not be checked".
fn ledger_pid(raw: f64) -> u64 {
    if raw.fract() == 0.0 && raw >= 0.0 {
        raw as u64
    } else {
        u64::MAX
    }
}

/// The supervisor and child a ledger recorded. A valid ledger is read with the strict classifier the
/// session uses; only an unparseable one is read leniently (DESIGN.r2 4.3).
fn recorded_processes(
    ledger: &Value,
    probe: &StaleLockProbe<'_>,
    classify: RecordedProcessClassifier,
) -> RecordedProcesses {
    for (role, pid_key, start_key) in [
        ("supervisor", "pid", "processStart"),
        ("child", "childPid", "childProcessStart"),
    ] {
        let raw_pid = ledger.get(pid_key).and_then(Value::as_f64);
        let start = match ledger.get(start_key) {
            Some(Value::String(identity)) => RecordedStart::Identity(identity.as_str()),
            Some(Value::Null) => RecordedStart::Null,
            _ => RecordedStart::Undefined,
        };
        let verdict = classify(raw_pid.map(ledger_pid), start, &probe.recorded_seams());
        let label = raw_pid.map_or_else(String::new, |pid| pid.to_string());
        match verdict {
            RecordedProcessVerdict::Alive => {
                return RecordedProcesses::Live(format!("its {role} pid {label} is still running"));
            }
            RecordedProcessVerdict::Unknown => {
                return RecordedProcesses::Live(format!(
                    "its {role} pid {label} could not be checked"
                ));
            }
            RecordedProcessVerdict::Dead | RecordedProcessVerdict::Absent => {}
        }
    }
    RecordedProcesses::NotLive
}

/// A valid ledger the launcher wrote with `launching: true` before its detached supervisor recorded a
/// pid: both pids read absent while that supervisor may still be starting, so reconciliation keeps it
/// until the hard deadline, and so does the reaper.
fn still_launching(fields: &Value, valid: bool, now: f64) -> bool {
    let hard_deadline_at = fields.get("hardDeadlineAt").and_then(Value::as_f64);
    valid
        && fields.get("launching").and_then(Value::as_bool) == Some(true)
        && hard_deadline_at.is_some_and(|deadline| now <= deadline)
}

fn pid_u32(raw: f64) -> Option<u32> {
    (raw.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&raw)).then_some(raw as u32)
}

fn capture(pattern: &Regex, bytes: &str) -> Option<String> {
    pattern
        .captures(bytes)
        .and_then(|found| found.get(1))
        .map(|group| group.as_str().to_string())
}

/// The fail-closed conditions under which a torn active.lock may be moved aside (DESIGN.r2 4.4, M4);
/// the first one that fails is the reason it is kept.
fn torn_lock_blocker(
    reflection_dir: &Path,
    bytes: &str,
    probe: &StaleLockProbe<'_>,
) -> Option<String> {
    let now = probe.now();
    let lock_at = modified_at(&reflection_dir.join("active.lock"), probe);
    if lock_at.is_none_or(|at| now - at <= TORN_LOCK_MIN_AGE_MS) {
        // rule:torn-recent
        return Some("it is recent".to_string());
    }
    let host = capture(&TORN_LAUNCHER_HOST, bytes);
    if let Some(host) = &host
        && *host != probe.hostname()
    {
        // rule:torn-other-host
        return Some(format!("its launcher is on host {host}"));
    }
    let pid = capture(&TORN_LAUNCHER_PID, bytes);
    if pid.is_some() && host.is_none() {
        // The reservation writes the pid before the hostname, so a pid with no complete host may be another host's.
        // rule:torn-host-unknown
        return Some("its launcher host is unknown".to_string());
    }
    if let Some(pid) = &pid {
        let start = capture(&TORN_LAUNCHER_START, bytes).map(Value::String);
        let liveness = pid
            .parse::<u32>()
            .map_or(ProcessLiveness::Unknown, |parsed| {
                reflection_launcher_liveness(parsed, start.as_ref(), &probe.base)
            });
        if liveness != ProcessLiveness::Dead {
            // rule:torn-launcher-alive
            return Some(format!("its launcher pid {pid} may still be alive"));
        }
    }
    let runs_dir = reflection_dir.join("runs");
    for name in list_directory(&runs_dir, probe) {
        if name.starts_with('.') {
            continue;
        }
        let run_dir = runs_dir.join(&name);
        if has_terminal_sentinel(&run_dir, probe) {
            continue;
        }
        if modified_at(&run_dir, probe).is_none_or(|at| now - at <= REFLECTION_ORPHAN_GRACE_MS) {
            // rule:torn-young-run-dir
            return Some(format!("run {name} is a young run dir"));
        }
        let LedgerRead::Present { fields, valid } = read_ledger(&run_dir, probe) else {
            continue;
        };
        if still_launching(&fields, valid, now) {
            // rule:torn-launching-run
            return Some(format!("run {name} is still launching"));
        }
        if let RecordedProcesses::Live(_) =
            recorded_processes(&fields, probe, classify_recorded_process)
        {
            // rule:torn-live-run
            return Some(format!("run {name} may still be live"));
        }
    }
    None
}

fn torn_lock(
    reflection_dir: &Path,
    bytes: &str,
    reason: &str,
    probe: &StaleLockProbe<'_>,
) -> StaleLockDecision {
    match torn_lock_blocker(reflection_dir, bytes, probe) {
        Some(blocker) => StaleLockDecision::Stuck {
            run_id: "unknown".to_string(),
            reason: format!("{reason} and {blocker}"),
        },
        // rule:lock-torn
        None => StaleLockDecision::Reap {
            action: StaleLockReapAction::LockTorn,
            run_id: "unknown".to_string(),
            reason: reason.to_string(),
        },
    }
}

fn stuck_decision(run_id: &str, reason: &str) -> StaleLockDecision {
    StaleLockDecision::Stuck {
        run_id: run_id.to_string(),
        reason: reason.to_string(),
    }
}

fn reap_decision(
    action: StaleLockReapAction,
    run_id: &str,
    reason: impl Into<String>,
) -> StaleLockDecision {
    StaleLockDecision::Reap {
        action,
        run_id: run_id.to_string(),
        reason: reason.into(),
    }
}

fn decide_stuck(
    reflection_dir: &Path,
    run_id: &str,
    stuck_reason: &str,
    probe: &StaleLockProbe<'_>,
) -> StaleLockDecision {
    let pass_through = || stuck_decision(run_id, stuck_reason);
    let TextRead::Text(lock_text) = read_text(&reflection_dir.join("active.lock"), probe) else {
        return pass_through();
    };
    let Some(parsed) = parse_json(&lock_text) else {
        return torn_lock(
            reflection_dir,
            &lock_text,
            "active.lock is not valid JSON",
            probe,
        );
    };
    let launcher_pid = parsed.get("launcherPid").and_then(Value::as_f64);
    let launcher_hostname = parsed.get("launcherHostname").and_then(Value::as_str);
    let (Some(launcher_pid), Some(launcher_hostname)) = (launcher_pid, launcher_hostname) else {
        if !is_reserved_run(&parsed) {
            return torn_lock(
                reflection_dir,
                &lock_text,
                "active.lock is not a reservation",
                probe,
            );
        }
        // rule:legacy-lock
        return stuck_decision(
            run_id,
            "its launcher identity is missing; a session reconciles it",
        );
    };
    if launcher_hostname != probe.hostname() {
        return pass_through();
    }
    let Some(launcher) = pid_u32(launcher_pid) else {
        return pass_through();
    };
    let liveness =
        reflection_launcher_liveness(launcher, parsed.get("launcherProcessStart"), &probe.base);
    if liveness != ProcessLiveness::Dead {
        return pass_through();
    }
    let run_dir = reflection_dir.join("runs").join(run_id);
    if exists(&run_dir.join("outcome.json"), probe) {
        return pass_through();
    }
    if !is_reserved_run(&parsed) {
        return torn_lock(
            reflection_dir,
            &lock_text,
            "active.lock is not a reservation",
            probe,
        );
    }
    match read_final_outcome(&run_dir, probe).as_deref() {
        Some("merged" | "no_changes") => {
            // rule:final-folded
            return stuck_decision(
                run_id,
                "it finished with a folded outcome that only a session can settle; open a session",
            );
        }
        Some(_) => {
            // rule:terminal-recorded
            return reap_decision(
                StaleLockReapAction::TerminalRecorded,
                run_id,
                "the run already recorded its terminal state",
            );
        }
        None => {}
    }
    match settled_sentinel(&run_dir, run_id, &parsed, probe) {
        Some(SettledSentinel::Match) => {
            // rule:release-settled
            return reap_decision(
                StaleLockReapAction::ReleaseSettled,
                run_id,
                "it was already settled and only the release is missing",
            );
        }
        Some(SettledSentinel::OtherGeneration) => {
            // rule:settled-other-generation
            return stuck_decision(
                run_id,
                "its run dir holds a terminal record from another generation",
            );
        }
        None => {}
    }
    let now = probe.now();
    let LedgerRead::Present { fields, valid } = read_ledger(&run_dir, probe) else {
        let reserved_at = iso_millis(parsed.get("reservedAt"));
        if reserved_at.is_some_and(|at| now - at <= REFLECTION_LAUNCH_WINDOW_MS) {
            // rule:launch-window
            return stuck_decision(
                run_id,
                "its launcher is gone but the launch window has not closed",
            );
        }
        // rule:launch-interrupted
        return reap_decision(
            StaleLockReapAction::LaunchInterrupted,
            run_id,
            format!("launcher pid {launcher_pid} is gone and the run never started"),
        );
    };
    if still_launching(&fields, valid, now) {
        // rule:run-launching
        return stuck_decision(run_id, STILL_LAUNCHING);
    }
    let classify: RecordedProcessClassifier = if valid {
        classify_recorded_process
    } else {
        classify_recorded_process_lenient
    };
    if let RecordedProcesses::Live(reason) = recorded_processes(&fields, probe, classify) {
        // rule:run-process-alive
        return stuck_decision(run_id, &reason);
    }
    if valid {
        // rule:processes-dead
        return reap_decision(
            StaleLockReapAction::ProcessesDead,
            run_id,
            "its launcher, supervisor and child are all gone and it recorded no outcome",
        );
    }
    // rule:ledger-unreadable
    reap_decision(
        StaleLockReapAction::LedgerUnreadable,
        run_id,
        "its ledger cannot be read and its recorded processes are gone",
    )
}

pub fn decide_stale_reflection_lock(
    reflection_dir: &Path,
    probe: &StaleLockProbe<'_>,
) -> (ReflectionLockVerdict, StaleLockDecision) {
    let verdict = classify_reflection_lock_holder(reflection_dir, &probe.base);
    let decision = match &verdict {
        ReflectionLockVerdict::None | ReflectionLockVerdict::Live { .. } => StaleLockDecision::None,
        // rule:finalize
        ReflectionLockVerdict::Dead { run_id } => StaleLockDecision::Finalize {
            run_id: run_id.clone(),
        },
        ReflectionLockVerdict::Stuck { run_id, reason } => {
            decide_stuck(reflection_dir, run_id, reason, probe)
        }
    };
    (verdict, decision)
}

/// When a ledger-less run dir was last launched: its prelaunch.json's mtime, else the dir's own (DESIGN.r3 c.16).
pub fn stranded_since(run_dir: &Path, probe: &StaleLockProbe<'_>) -> Option<f64> {
    let prelaunch = run_dir.join("prelaunch.json");
    if exists(&prelaunch, probe) {
        modified_at(&prelaunch, probe)
    } else {
        modified_at(run_dir, probe)
    }
}

pub fn decide_stranded_run(run_dir: &Path, probe: &StaleLockProbe<'_>) -> StrandedRunDecision {
    let keep = |reason: &str| StrandedRunDecision::Keep {
        reason: reason.to_string(),
    };
    let now = probe.now();
    let (fields, valid) = match read_ledger(run_dir, probe) {
        LedgerRead::Absent => {
            if let TextRead::Text(text) = read_text(&run_dir.join("prelaunch.json"), probe) {
                let named = parse_json(&text).and_then(|value| {
                    value
                        .get("runId")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
                match named {
                    // rule:stranded-prelaunch-unreadable
                    None => return keep("its prelaunch.json cannot be read"),
                    // rule:stranded-prelaunch-mismatch
                    Some(named) if named != run_dir_name(run_dir) => {
                        return keep("its prelaunch.json names another run");
                    }
                    Some(_) => {}
                }
            }
            if stranded_since(run_dir, probe)
                .is_none_or(|since| now - since <= STRANDED_RUN_GRACE_MS)
            {
                // rule:stranded-young
                return keep("it is inside the launch grace window");
            }
            // rule:stranded-no-ledger
            return StrandedRunDecision::Reap {
                action: StrandedRunAction::NoLedger,
                reason: "it never got a ledger".to_string(),
            };
        }
        LedgerRead::Present { fields, valid } => (fields, valid),
    };
    if still_launching(&fields, valid, now) {
        // rule:stranded-launching
        return keep(STILL_LAUNCHING);
    }
    let classify: RecordedProcessClassifier = if valid {
        classify_recorded_process
    } else {
        classify_recorded_process_lenient
    };
    if let RecordedProcesses::Live(reason) = recorded_processes(&fields, probe, classify) {
        // rule:stranded-process-alive
        return keep(&reason);
    }
    if valid {
        // rule:stranded-processes-dead
        return StrandedRunDecision::Reap {
            action: StrandedRunAction::ProcessesDead,
            reason: "its supervisor and child are gone and it recorded no outcome".to_string(),
        };
    }
    // rule:stranded-ledger-unreadable
    StrandedRunDecision::Reap {
        action: StrandedRunAction::LedgerUnreadable,
        reason: "its ledger cannot be read and its recorded processes are gone".to_string(),
    }
}

pub type ReceiptAppender<'a> = dyn Fn(&Path, &MemoryReceiptInput) -> Result<(), String> + 'a;

pub struct StaleLockReaperOptions<'a> {
    pub runtime_dir: &'a Path,
    pub probe: StaleLockProbe<'a>,
    /// In-session settle (reconciliation). Absent means the minimal settle from the CLI.
    pub settle: Option<&'a dyn Fn() -> Result<(), String>>,
    /// Settle even when the lock needs nothing (session start reconciles every run).
    pub settle_always: bool,
    /// False when this session already settled this run once without releasing it.
    pub should_settle: Option<&'a dyn Fn(&str) -> bool>,
    pub dry_run: bool,
    pub append_receipt: Option<&'a ReceiptAppender<'a>>,
}

impl<'a> StaleLockReaperOptions<'a> {
    pub fn new(runtime_dir: &'a Path) -> Self {
        Self {
            runtime_dir,
            probe: StaleLockProbe::default(),
            settle: None,
            settle_always: false,
            should_settle: None,
            dry_run: false,
            append_receipt: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleLockReaperAction {
    None,
    Settled,
    SettleDeclined,
    Reaped,
    Kept,
    WouldReap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedRunReport {
    pub run_id: String,
    pub decision: StrandedRunDecision,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneReport {
    pub name: String,
    pub removed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleLockReaperReport {
    pub before: ReflectionLockVerdict,
    pub decision: StaleLockDecision,
    pub action: StaleLockReaperAction,
    pub after: ReflectionLockVerdict,
    pub stranded: Vec<StrandedRunReport>,
    pub tombstones: Vec<TombstoneReport>,
    pub warnings: Vec<String>,
    pub dry_run: bool,
    /// A queued run left in `pending.json` for the next session (never promoted from the CLI).
    pub pending_kept: Option<String>,
    pub kept_reason: Option<String>,
}

struct ReaperContext<'a> {
    runtime_dir: PathBuf,
    reflection_dir: PathBuf,
    transcripts_dir: PathBuf,
    locks_dir: PathBuf,
    reflection_sessions_dir: PathBuf,
    worktrees_dir: PathBuf,
    probe: StaleLockProbe<'a>,
    append_receipt: Option<&'a ReceiptAppender<'a>>,
    warnings: RefCell<Vec<String>>,
}

impl<'a> ReaperContext<'a> {
    fn new(
        runtime_dir: &Path,
        probe: StaleLockProbe<'a>,
        append_receipt: Option<&'a ReceiptAppender<'a>>,
    ) -> Self {
        Self {
            runtime_dir: runtime_dir.to_path_buf(),
            reflection_dir: runtime_dir.join("reflection"),
            transcripts_dir: runtime_dir.join("transcripts"),
            locks_dir: runtime_dir.join("locks"),
            reflection_sessions_dir: runtime_dir.join("reflection-sessions"),
            worktrees_dir: runtime_dir.join("worktrees"),
            probe,
            append_receipt,
            warnings: RefCell::new(Vec::new()),
        }
    }

    fn now(&self) -> f64 {
        self.probe.now()
    }

    fn now_iso(&self) -> String {
        format_rfc3339_millis(self.now() as i64)
    }

    fn emit_receipt(&self, input: MemoryReceiptInput) {
        let written = match self.append_receipt {
            Some(append) => append(self.runtime_dir.as_path(), &input),
            None => append_memory_receipt_once(&self.runtime_dir, &input).map(|_| ()),
        };
        if let Err(message) = written {
            let id = match &input {
                MemoryReceiptInput::Run { run_id, .. } => run_id,
                MemoryReceiptInput::Facts { batch_id, .. } => batch_id,
            };
            self.warnings
                .borrow_mut()
                .push(format!("receipt for {id} not written: {message}"));
        }
    }
}

fn io_message(error: std::io::Error) -> String {
    error.to_string()
}

/// Runs `task` under `lock_path` without waiting; `None` means another process holds it.
fn under_lock<T>(
    lock_path: &Path,
    purpose: &str,
    task: impl FnOnce() -> Result<T, String>,
) -> Result<Option<T>, String> {
    let record = create_lock_record(purpose, CreateLockRecordOptions { run_id: None })
        .map_err(|error| error.to_string())?;
    let no_wait = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        ..Default::default()
    };
    match with_lock(lock_path, &record, &no_wait, task) {
        Ok(value) => Ok(Some(value)),
        Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => Ok(None),
        Err(WithLockError::Acquire(error)) => Err(error.to_string()),
        Err(WithLockError::User(message)) => Err(message),
    }
}

fn finalization_lock(context: &ReaperContext<'_>, run_id: &str) -> Result<PathBuf, String> {
    run_finalization_lock_path(&context.locks_dir, run_id).map_err(|error| error.to_string())
}

fn iso_or(value: Option<&Value>, fallback: &str) -> String {
    match value.and_then(Value::as_str) {
        Some(text) if parse_rfc3339(text).is_some() => text.to_string(),
        _ => fallback.to_string(),
    }
}

#[derive(Debug, Clone)]
struct RunIdentity {
    kind: String,
    trigger: String,
    origin: Option<String>,
}

fn identity_from_ledger(fields: &Value, fallback: &RunIdentity) -> RunIdentity {
    let kind = match fields.get("kind").and_then(Value::as_str) {
        Some(kind @ ("dream" | "reflection")) => kind.to_string(),
        _ => fallback.kind.clone(),
    };
    let trigger = fields
        .get("trigger")
        .and_then(Value::as_str)
        .map_or_else(|| fallback.trigger.clone(), str::to_string);
    let origin = fields
        .get("origin")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| fallback.origin.clone());
    RunIdentity {
        kind,
        trigger,
        origin,
    }
}

fn identity_from_reservation(active: &Value) -> RunIdentity {
    let request = active.get("request");
    let trigger = request
        .and_then(|request| request.get("trigger"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let origin = request
        .and_then(|request| request.get("origin"))
        .and_then(Value::as_str)
        .map(str::to_string);
    RunIdentity {
        kind: if trigger == "dream" {
            "dream".to_string()
        } else {
            "reflection".to_string()
        },
        trigger,
        origin,
    }
}

fn run_receipt(
    identity: &RunIdentity,
    run_id: &str,
    event: &str,
    generation: &str,
    reason: &str,
) -> MemoryReceiptInput {
    MemoryReceiptInput::Run {
        kind: identity.kind.clone(),
        run_id: run_id.to_string(),
        trigger: identity
            .origin
            .clone()
            .unwrap_or_else(|| identity.trigger.clone()),
        event: event.to_string(),
        generation: generation.to_string(),
        sha: None,
        reason: Some(reason.to_string()),
        detail: None,
    }
}

fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn abandon_run(
    context: &ReaperContext<'_>,
    run_dir: &Path,
    run_id: &str,
    reason: AbandonedReason,
    generation: &str,
    identity: &RunIdentity,
    launcher: Option<&Value>,
) -> Result<(), String> {
    create_private_dir_all(run_dir).map_err(io_message)?;
    let launcher = launcher.map(|launcher| AbandonedLauncher {
        pid: launcher
            .get("launcherPid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        hostname: launcher
            .get("launcherHostname")
            .and_then(Value::as_str)
            .map(str::to_string),
        process_start: launcher
            .get("launcherProcessStart")
            .and_then(Value::as_str)
            .map(str::to_string),
    });
    write_abandoned(
        run_dir,
        &AbandonedRecord {
            version: 1,
            run_id: run_id.to_string(),
            abandoned_at: context.now_iso(),
            reason,
            generation: generation.to_string(),
            kind: identity.kind.clone(),
            trigger: identity.trigger.clone(),
            origin: identity.origin.clone(),
            launcher,
        },
    )
    .map_err(io_message)?;
    context.emit_receipt(run_receipt(
        identity,
        run_id,
        "abandoned",
        generation,
        reason.as_str(),
    ));
    Ok(())
}

fn quarantine_run(
    context: &ReaperContext<'_>,
    run_dir: &Path,
    run_id: &str,
    generation: &str,
    identity: &RunIdentity,
    reservation: Option<&Value>,
    reason: QuarantineReason,
) -> Result<(), String> {
    let mut evidence: Vec<String> = std::fs::read_dir(run_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    evidence.sort();
    if reservation.is_some() {
        evidence.push(RESERVATION_EVIDENCE_FILENAME.to_string());
    }
    write_quarantine(
        run_dir,
        &QuarantineRecord {
            version: 1,
            run_id: run_id.to_string(),
            kind: identity.kind.clone(),
            trigger: identity.trigger.clone(),
            origin: identity.origin.clone(),
            generation: generation.to_string(),
            reason,
            quarantined_at: context.now_iso(),
            evidence,
        },
        reservation,
    )
    .map_err(io_message)?;
    context.emit_receipt(run_receipt(
        identity,
        run_id,
        "quarantined",
        generation,
        reason.as_str(),
    ));
    Ok(())
}

fn reserved_run_id(path: &Path) -> Option<String> {
    let parsed = parse_json(&std::fs::read_to_string(path).ok()?)?;
    parsed.get("runId")?.as_str().map(str::to_string)
}

fn read_active_fields(reflection_dir: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(reflection_dir.join("active.lock")).ok()?;
    parse_json(&raw).filter(Value::is_object)
}

fn release_reservation(
    context: &ReaperContext<'_>,
    run_id: &str,
) -> Result<ReleaseWithoutPromotionResult, String> {
    let released = release_reservation_without_promotion(&ReleaseWithoutPromotionOptions {
        reflection_dir: &context.reflection_dir,
        transcripts_dir: &context.transcripts_dir,
        locks_dir: &context.locks_dir,
        run_id,
    });
    match released {
        Ok(result) => Ok(result),
        Err(ReleaseError::Contention(_)) => Ok(ReleaseWithoutPromotionResult {
            released: false,
            pending_run_id: None,
        }),
        Err(ReleaseError::Failed(message)) => Err(message),
    }
}

enum MinimalSettle {
    Settled {
        pending_run_id: Option<String>,
        already_released: bool,
    },
    Kept(String),
}

fn kept(reason: impl Into<String>) -> Result<MinimalSettle, String> {
    Ok(MinimalSettle::Kept(reason.into()))
}

fn settle_under_finalize_lock(
    context: &ReaperContext<'_>,
    run_id: &str,
    run_dir: &Path,
    action: StaleLockReapAction,
) -> Result<MinimalSettle, String> {
    let again = decide_stale_reflection_lock(&context.reflection_dir, &context.probe).1;
    let unchanged = matches!(
        &again,
        StaleLockDecision::Reap { action: again_action, run_id: again_run, .. }
            if *again_action == action && again_run == run_id
    );
    if !unchanged {
        return kept("the decision changed");
    }
    let active =
        read_active_fields(&context.reflection_dir).unwrap_or_else(|| Value::Object(Map::new()));
    let identity = identity_from_reservation(&active);
    let reserved_at = iso_or(active.get("reservedAt"), &context.now_iso());
    match action {
        StaleLockReapAction::LaunchInterrupted => {
            abandon_run(
                context,
                run_dir,
                run_id,
                AbandonedReason::LaunchInterrupted,
                &reserved_at,
                &identity,
                Some(&active),
            )?;
            return Ok(MinimalSettle::Settled {
                pending_run_id: None,
                already_released: false,
            });
        }
        // An earlier settle wrote the sentinel and stopped before releasing (REVIEW-2 N2): only release,
        // inside this finalize lock and after the re-decide above (DESIGN.r4 c.20).
        StaleLockReapAction::TerminalRecorded | StaleLockReapAction::ReleaseSettled => {
            let released = release_reservation(context, run_id)?;
            return if released.released {
                Ok(MinimalSettle::Settled {
                    pending_run_id: released.pending_run_id,
                    already_released: true,
                })
            } else {
                kept("the reservation changed before it could be released")
            };
        }
        StaleLockReapAction::ProcessesDead | StaleLockReapAction::LedgerUnreadable => {}
        StaleLockReapAction::LockTorn => {
            return kept(format!("{} is not settled from here", action.as_str()));
        }
    }
    let fields = match read_ledger(run_dir, &StaleLockProbe::default()) {
        LedgerRead::Present { fields, .. } => fields,
        LedgerRead::Absent => Value::Null,
    };
    let attempt = fields
        .get("attempt")
        .and_then(Value::as_f64)
        .map(|attempt| attempt as i64);
    if let Some(published) = read_run_terminal_publication(run_dir, attempt).map_err(io_message)? {
        let state = if published == RunTerminalPublication::Settled {
            "settled"
        } else {
            "publishing its outcome"
        };
        return kept(format!("the run is already {state}"));
    }
    let claim_attempt = attempt.unwrap_or(1);
    let claim = claim_run_terminal(
        run_dir,
        run_id,
        claim_attempt,
        RunTerminalClaimKind::Abandon,
        &RunTerminalClaimSeams {
            pid_liveness: context.probe.base.pid_liveness,
            before_recovery: None,
        },
        std::process::id(),
    )
    .map_err(|error| error.to_string())?;
    if !run_terminal_claim_matches(
        &claim,
        run_id,
        claim_attempt,
        Some(RunTerminalClaimKind::Abandon),
    ) {
        return kept("another process claimed the run");
    }
    let generation = iso_or(fields.get("startedAt"), &reserved_at);
    let identity = identity_from_ledger(&fields, &identity);
    if action == StaleLockReapAction::ProcessesDead {
        abandon_run(
            context,
            run_dir,
            run_id,
            AbandonedReason::ProcessesDead,
            &generation,
            &identity,
            None,
        )?;
    } else {
        let reservation = is_reserved_run(&active).then_some(&active);
        quarantine_run(
            context,
            run_dir,
            run_id,
            &generation,
            &identity,
            reservation,
            QuarantineReason::LedgerUnreadable,
        )?;
    }
    Ok(MinimalSettle::Settled {
        pending_run_id: None,
        already_released: false,
    })
}

fn settle_active_minimally(
    context: &ReaperContext<'_>,
    action: StaleLockReapAction,
    run_id: &str,
) -> Result<MinimalSettle, String> {
    let run_dir = context.reflection_dir.join("runs").join(run_id);
    let written = under_lock(
        &finalization_lock(context, run_id)?,
        "reflection-finalize",
        || settle_under_finalize_lock(context, run_id, &run_dir, action),
    )?;
    let Some(written) = written else {
        return kept("a session is finalizing it");
    };
    match written {
        MinimalSettle::Kept(_)
        | MinimalSettle::Settled {
            already_released: true,
            ..
        } => Ok(written),
        MinimalSettle::Settled { .. } => {
            let released = release_reservation(context, run_id)?;
            if released.released {
                Ok(MinimalSettle::Settled {
                    pending_run_id: released.pending_run_id,
                    already_released: false,
                })
            } else {
                kept("the reservation changed before it could be released")
            }
        }
    }
}

fn torn_run_numbers(bytes: &str) -> Vec<u64> {
    TORN_RUN_ID
        .captures_iter(bytes)
        .filter_map(|found| found.get(1)?.as_str().parse::<u64>().ok())
        .filter(|number| *number <= MAX_SAFE_INTEGER)
        .collect()
}

fn reap_torn_lock(context: &ReaperContext<'_>) -> Result<bool, String> {
    let moved = under_lock(
        &reflection_scheduler_lock_path(&context.locks_dir),
        "reflection-scheduler",
        || {
            let again = decide_stale_reflection_lock(&context.reflection_dir, &context.probe).1;
            if !matches!(
                again,
                StaleLockDecision::Reap {
                    action: StaleLockReapAction::LockTorn,
                    ..
                }
            ) {
                return Ok(false);
            }
            let bytes = std::fs::read_to_string(context.reflection_dir.join("active.lock"))
                .map_err(io_message)?;
            let named = torn_run_numbers(&bytes);
            let scanned = scan_reflection_run_high_water(RunIdHighWaterPaths {
                reflection: &context.reflection_dir,
                reflection_sessions: &context.reflection_sessions_dir,
                worktrees: &context.worktrees_dir,
            });
            let high_water = named.iter().copied().fold(scanned, u64::max);
            if high_water > 0 {
                record_reflection_run_id_mark(&context.reflection_dir, high_water)
                    .map_err(io_message)?;
            }
            let at = context.now() as i64;
            let moved = move_torn_reservation_aside(&context.reflection_dir, &bytes, at)
                .map_err(io_message)?;
            if moved.is_none() {
                return Ok(false);
            }
            let torn_run = named.iter().max().map_or_else(
                || "unknown".to_string(),
                |number| format!("reflection-run-{number}"),
            );
            context.emit_receipt(MemoryReceiptInput::Run {
                kind: "reflection".to_string(),
                run_id: torn_run,
                trigger: "unknown".to_string(),
                event: "abandoned".to_string(),
                generation: format_rfc3339_millis(at),
                sha: None,
                reason: Some("lock_torn".to_string()),
                detail: None,
            });
            Ok(true)
        },
    )?;
    Ok(moved == Some(true))
}

fn settle_stranded_run_under_lock(
    context: &ReaperContext<'_>,
    run_dir: &Path,
    run_id: &str,
    action: StrandedRunAction,
) -> Result<bool, String> {
    let bare = StaleLockProbe::default();
    let since = stranded_since(run_dir, &bare);
    let applied = under_lock(
        &finalization_lock(context, run_id)?,
        "reflection-finalize",
        || {
            if has_terminal_sentinel(run_dir, &bare) {
                return Ok(false);
            }
            let fields = match read_ledger(run_dir, &bare) {
                LedgerRead::Present { fields, .. } => fields,
                LedgerRead::Absent => Value::Null,
            };
            let unknown = RunIdentity {
                kind: "reflection".to_string(),
                trigger: "unknown".to_string(),
                origin: None,
            };
            let fallback = format_rfc3339_millis(since.unwrap_or_else(|| context.now()) as i64);
            if action == StrandedRunAction::NoLedger {
                quarantine_run(
                    context,
                    run_dir,
                    run_id,
                    &fallback,
                    &unknown,
                    None,
                    QuarantineReason::StrandedNoLedger,
                )?;
                return Ok(true);
            }
            let generation = iso_or(fields.get("startedAt"), &fallback);
            let identity = identity_from_ledger(&fields, &unknown);
            if action == StrandedRunAction::ProcessesDead {
                abandon_run(
                    context,
                    run_dir,
                    run_id,
                    AbandonedReason::ProcessesDead,
                    &generation,
                    &identity,
                    None,
                )?;
            } else {
                quarantine_run(
                    context,
                    run_dir,
                    run_id,
                    &generation,
                    &identity,
                    None,
                    QuarantineReason::LedgerUnreadable,
                )?;
            }
            Ok(true)
        },
    )?;
    Ok(applied == Some(true))
}

pub struct QuarantineStrandedRunOptions<'a> {
    pub runtime_dir: &'a Path,
    pub run_dir: &'a Path,
    pub run_id: &'a str,
    pub action: StrandedRunAction,
    pub now: Option<&'a dyn Fn() -> f64>,
    pub append_receipt: Option<&'a ReceiptAppender<'a>>,
}

/// The one writer for a run dir outside the reservation that never settled (DESIGN.r3 13b c.16): the
/// session's reconcile and the CLI reaper both settle it here, under its finalize lock, re-checking
/// that no sentinel appeared. Returns false when the lock is held or a sentinel already exists.
pub fn quarantine_stranded_run(options: &QuarantineStrandedRunOptions<'_>) -> Result<bool, String> {
    let probe = StaleLockProbe {
        now: options.now,
        ..Default::default()
    };
    let context = ReaperContext::new(options.runtime_dir, probe, options.append_receipt);
    settle_stranded_run_under_lock(&context, options.run_dir, options.run_id, options.action)
}

fn stranded_runs(
    context: &ReaperContext<'_>,
    apply: bool,
) -> Result<Vec<StrandedRunReport>, String> {
    let bare = StaleLockProbe::default();
    let runs_dir = context.reflection_dir.join("runs");
    let reserved: Vec<String> = ["active.lock", "pending.json"]
        .iter()
        .filter_map(|name| reserved_run_id(&context.reflection_dir.join(name)))
        .collect();
    let mut reports = Vec::new();
    for run_id in list_directory(&runs_dir, &bare) {
        if run_id.starts_with('.') || reserved.contains(&run_id) {
            continue;
        }
        let run_dir = runs_dir.join(&run_id);
        if has_terminal_sentinel(&run_dir, &bare) {
            continue;
        }
        let decision = decide_stranded_run(&run_dir, &context.probe);
        let action = match &decision {
            StrandedRunDecision::Reap { action, .. } if apply => Some(*action),
            _ => None,
        };
        let Some(action) = action else {
            reports.push(StrandedRunReport {
                run_id,
                decision,
                applied: false,
            });
            continue;
        };
        let applied = settle_stranded_run_under_lock(context, &run_dir, &run_id, action)?;
        reports.push(StrandedRunReport {
            run_id,
            decision,
            applied,
        });
    }
    Ok(reports)
}

fn remove_tombstones(context: &ReaperContext<'_>, apply: bool) -> Vec<TombstoneReport> {
    let runs_dir = context.reflection_dir.join("runs");
    let mut names: Vec<String> = std::fs::read_dir(&runs_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with(TOMBSTONE_PREFIX))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            if !apply {
                return TombstoneReport {
                    name,
                    removed: false,
                    error: None,
                };
            }
            match remove_tree(&runs_dir.join(&name)) {
                Ok(()) => TombstoneReport {
                    name,
                    removed: true,
                    error: None,
                },
                Err(error) => TombstoneReport {
                    name,
                    removed: false,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect()
}

pub fn run_stale_lock_reaper(
    options: &StaleLockReaperOptions<'_>,
) -> Result<StaleLockReaperReport, String> {
    let probe = options.probe;
    let context = ReaperContext::new(options.runtime_dir, probe, options.append_receipt);
    let (before, decision) = decide_stale_reflection_lock(&context.reflection_dir, &probe);
    let minimal = options.settle.is_none();
    let dry_run = options.dry_run;
    let mut pending_kept = None;
    let mut kept_reason = None;
    let action = match (&decision, dry_run) {
        (StaleLockDecision::Reap { .. }, true) => {
            pending_kept = reserved_run_id(&context.reflection_dir.join("pending.json"));
            StaleLockReaperAction::WouldReap
        }
        (StaleLockDecision::None, true) => StaleLockReaperAction::None,
        (_, true) => StaleLockReaperAction::Kept,
        (
            StaleLockDecision::Reap {
                action: StaleLockReapAction::LockTorn,
                ..
            },
            false,
        ) => {
            if reap_torn_lock(&context)? {
                StaleLockReaperAction::Reaped
            } else {
                StaleLockReaperAction::Kept
            }
        }
        (
            StaleLockDecision::Reap { run_id, .. } | StaleLockDecision::Finalize { run_id },
            false,
        ) => match options.settle {
            None => {
                let settled = match &decision {
                    StaleLockDecision::Reap { action, .. } => {
                        settle_active_minimally(&context, *action, run_id)?
                    }
                    _ => MinimalSettle::Kept(
                        "it recorded its outcome; open a session to finalize it".to_string(),
                    ),
                };
                match settled {
                    MinimalSettle::Settled { pending_run_id, .. } => {
                        pending_kept = pending_run_id;
                        StaleLockReaperAction::Reaped
                    }
                    MinimalSettle::Kept(reason) => {
                        kept_reason = Some(reason);
                        StaleLockReaperAction::Kept
                    }
                }
            }
            Some(_)
                if options
                    .should_settle
                    .is_some_and(|should| !should(run_id.as_str())) =>
            {
                StaleLockReaperAction::SettleDeclined
            }
            Some(settle) => {
                settle()?;
                StaleLockReaperAction::Settled
            }
        },
        (StaleLockDecision::None, false) => StaleLockReaperAction::None,
        (StaleLockDecision::Stuck { .. }, false) => StaleLockReaperAction::Kept,
    };
    if let Some(settle) = options.settle
        && options.settle_always
        && !dry_run
        && action != StaleLockReaperAction::Settled
    {
        settle()?;
    }
    let stranded = if minimal {
        stranded_runs(&context, !dry_run)?
    } else {
        Vec::new()
    };
    let tombstones = if minimal {
        remove_tombstones(&context, !dry_run)
    } else {
        Vec::new()
    };
    let after = if dry_run {
        before.clone()
    } else {
        classify_reflection_lock_holder(&context.reflection_dir, &probe.base)
    };
    Ok(StaleLockReaperReport {
        before,
        decision,
        action,
        after,
        stranded,
        tombstones,
        warnings: context.warnings.into_inner(),
        dry_run,
        pending_kept,
        kept_reason,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeadOwnerLockReport {
    pub reclaimed: Vec<String>,
    pub kept: Vec<(String, String)>,
}

const OWNER_NOT_PROVABLY_DEAD: &str = "owner is alive or not provably dead";

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn reclaim_dead_owner_locks(locks_dir: &Path) -> DeadOwnerLockReport {
    let mut names: Vec<String> = std::fs::read_dir(locks_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".lock"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut reclaimed: Vec<String> = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    sweep_dead_owner_locks(
        locks_dir,
        |lock_path, message| failures.push((file_name_of(lock_path), message.to_string())),
        |lock_path, _| reclaimed.push(file_name_of(lock_path)),
    );
    let kept = names
        .into_iter()
        .filter(|name| !reclaimed.contains(name))
        .map(|name| {
            let reason = failures
                .iter()
                .find(|(failed, _)| *failed == name)
                .map_or_else(
                    || OWNER_NOT_PROVABLY_DEAD.to_string(),
                    |(_, reason)| reason.clone(),
                );
            (name, reason)
        })
        .collect();
    DeadOwnerLockReport { reclaimed, kept }
}

pub fn format_stale_lock_fix_lines(
    identity: &str,
    report: &StaleLockReaperReport,
    locks: Option<&DeadOwnerLockReport>,
) -> Vec<String> {
    let mut lines = Vec::new();
    let decision = &report.decision;
    if let StaleLockDecision::Reap { run_id, .. } | StaleLockDecision::Finalize { run_id } =
        decision
    {
        let label = match decision {
            StaleLockDecision::Reap { action, .. } => action.as_str(),
            _ => "finalize",
        };
        let released_by_settle = report.action == StaleLockReaperAction::Settled
            && (report.after == ReflectionLockVerdict::None
                || report.after.run_id().is_some_and(|after| after != run_id));
        if report.action == StaleLockReaperAction::WouldReap {
            lines.push(format!(
                "WOULD FIX {identity} active.lock run {run_id} ({label})"
            ));
            if let Some(pending) = &report.pending_kept {
                lines.push(format!(
                    "INFO {identity} pending {pending} stays queued for the next session"
                ));
            }
        } else if report.action == StaleLockReaperAction::Reaped || released_by_settle {
            lines.push(format!(
                "FIXED {identity} active.lock run {run_id} ({label})"
            ));
            if let Some(pending) = &report.pending_kept {
                lines.push(format!(
                    "INFO {identity} pending {pending} kept for the next session"
                ));
            }
        } else if matches!(decision, StaleLockDecision::Finalize { .. })
            && report.action == StaleLockReaperAction::Kept
        {
            lines.push(format!(
                "KEPT {identity} active.lock run {run_id} (it recorded its outcome; open a session to finalize it)"
            ));
        } else {
            let why = report
                .kept_reason
                .clone()
                .unwrap_or_else(|| format!("{label} did not release the lock"));
            lines.push(format!("KEPT {identity} active.lock run {run_id} ({why})"));
        }
    } else if let StaleLockDecision::Stuck { run_id, reason } = decision {
        lines.push(format!(
            "KEPT {identity} active.lock run {run_id} ({reason})"
        ));
    }
    for entry in &report.stranded {
        let StrandedRunDecision::Reap { action, .. } = &entry.decision else {
            continue;
        };
        let state = if entry.applied {
            "FIXED"
        } else if report.dry_run {
            "WOULD FIX"
        } else {
            "KEPT"
        };
        lines.push(format!(
            "{state} {identity} run {} ({})",
            entry.run_id,
            action.as_str()
        ));
    }
    for tombstone in &report.tombstones {
        if tombstone.removed {
            lines.push(format!(
                "FIXED {identity} removed tombstone {}",
                tombstone.name
            ));
        } else if let Some(error) = &tombstone.error {
            lines.push(format!(
                "KEPT {identity} tombstone {} ({error})",
                tombstone.name
            ));
        }
    }
    for warning in &report.warnings {
        lines.push(format!("WARN {identity} {warning}"));
    }
    if let Some(locks) = locks {
        let failed: Vec<String> = locks
            .kept
            .iter()
            .filter(|(_, reason)| reason != OWNER_NOT_PROVABLY_DEAD)
            .map(|(name, reason)| format!("{name}: {reason}"))
            .collect();
        let detail = if failed.is_empty() {
            String::new()
        } else {
            format!(" ({})", failed.join("; "))
        };
        lines.push(format!(
            "INFO {identity} locks reclaimed {}, kept {}{detail}",
            locks.reclaimed.len(),
            locks.kept.len()
        ));
    }
    lines
}

#[cfg(test)]
#[path = "stale_lock_reaper_tests.rs"]
mod tests;
