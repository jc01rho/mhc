//! Port of `retention/policy.ts` (omo #12532/#12538): the retention windows, the shared pass
//! context, the keep reasons and the small filesystem helpers every store walk uses.

use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Write};
use std::path::Path;

use serde::Serialize;

use super::budget::PassBudget;
use crate::fs::resilient::{read_dir_names, symlink_metadata};
use crate::identity::layout::MemoryIdentityPaths;
use crate::journal::store::JournalError;

pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Windows in days; `memory.runtime.*` in omo.json, per-agent overridable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuntimeRetentionPolicy {
    /// Folded reflection runs, fully-reflected transcripts, idle sidecars, legacy stores.
    pub retention_days: f64,
    /// Never- and partly-reflected transcript journals, when `discard_unreflected_journals` is on.
    pub abandoned_journal_days: f64,
    /// Off (the default) keeps every journal reflection never folded into memory; on is
    /// health-gated.
    pub discard_unreflected_journals: bool,
    /// Consumed completion records and recall ledgers.
    pub completions_days: f64,
    /// Wall-clock budget of one pass per identity; an interrupted pass resumes from its cursor.
    pub pass_budget_ms: u64,
}

pub const DEFAULT_RUNTIME_RETENTION: RuntimeRetentionPolicy = RuntimeRetentionPolicy {
    retention_days: 7.0,
    abandoned_journal_days: 14.0,
    discard_unreflected_journals: false,
    completions_days: 30.0,
    pass_budget_ms: 60_000,
};

impl Default for RuntimeRetentionPolicy {
    fn default() -> Self {
        DEFAULT_RUNTIME_RETENTION
    }
}

/// `days` as whole milliseconds.
pub fn days_ms(days: f64) -> i64 {
    (days * DAY_MS as f64) as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetentionKeepReason {
    Anchor,
    Locked,
    Live,
    Malformed,
    Newest,
    NoMark,
    NoTerminal,
    Owned,
    PendingCompaction,
    PendingDelivery,
    Pruned,
    Recent,
    Reserved,
    RunPresent,
    StateUnreadable,
    UnfoldedTerminal,
    Unclassified,
    SchedulerBusy,
    ClaimFailed,
    ReflectionUnhealthy,
    UnreflectedRetentionOff,
    ChangedDuringRemoval,
    StateBehindTranscript,
}

impl RetentionKeepReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Locked => "locked",
            Self::Live => "live",
            Self::Malformed => "malformed",
            Self::Newest => "newest",
            Self::NoMark => "no-mark",
            Self::NoTerminal => "no-terminal",
            Self::Owned => "owned",
            Self::PendingCompaction => "pending-compaction",
            Self::PendingDelivery => "pending-delivery",
            Self::Pruned => "pruned",
            Self::Recent => "recent",
            Self::Reserved => "reserved",
            Self::RunPresent => "run-present",
            Self::StateUnreadable => "state-unreadable",
            Self::UnfoldedTerminal => "unfolded-terminal",
            Self::Unclassified => "unclassified",
            Self::SchedulerBusy => "scheduler-busy",
            Self::ClaimFailed => "claim-failed",
            Self::ReflectionUnhealthy => "reflection-unhealthy",
            Self::UnreflectedRetentionOff => "unreflected-retention-off",
            Self::ChangedDuringRemoval => "changed-during-removal",
            Self::StateBehindTranscript => "state-behind-transcript",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionStoreResult {
    pub removed: Vec<String>,
    pub kept: BTreeMap<String, RetentionKeepReason>,
}

/// Why a retention step stopped; an abort is the caller's cancel, never a failure of the store.
#[derive(Debug)]
pub enum RetentionError {
    Aborted,
    Io(io::Error),
    Journal(JournalError),
    Other(String),
}

impl std::fmt::Display for RetentionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Aborted => write!(f, "The operation was aborted"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Journal(error) => write!(f, "{error}"),
            Self::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for RetentionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Journal(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for RetentionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<JournalError> for RetentionError {
    fn from(error: JournalError) -> Self {
        match error {
            JournalError::Aborted => Self::Aborted,
            other => Self::Journal(other),
        }
    }
}

/// Warnings carry a message and its fields, as the TypeScript `warn(message, fields)` does.
pub type WarnFn<'a> = &'a dyn Fn(&str, &[(&str, String)]);

/// What every store walk of one identity shares. The pass sets `budget`; the doctors and the cap
/// build one without it.
#[derive(Clone, Copy)]
pub struct RetentionContext<'a> {
    pub paths: &'a MemoryIdentityPaths,
    pub policy: RuntimeRetentionPolicy,
    pub now: &'a dyn Fn() -> i64,
    pub is_live_session: Option<&'a dyn Fn(&str) -> bool>,
    /// `memory.reflection.enabled` for the identity; the CLI and the host sweep pass true.
    pub reflection_enabled: Option<bool>,
    /// The abort signal: true once the caller wants the pass to stop.
    pub cancel: Option<&'a dyn Fn() -> bool>,
    pub warn: Option<WarnFn<'a>>,
    /// Observed before every claim rename; an error refuses that claim (platform refusals in tests).
    pub before_claim_rename: Option<&'a dyn Fn(&Path, &Path) -> io::Result<()>>,
    /// Set by the pass: bounds the walk and records the resume point.
    pub budget: Option<&'a PassBudget<'a>>,
    /// Observed between a journal's lock-free selection and its claim under `state.lock`.
    pub before_journal_claim: Option<&'a dyn Fn(&str)>,
    /// Observed after a claim's lock is released and before its tombstone is deleted.
    pub before_tombstone_delete: Option<&'a dyn Fn(&Path)>,
}

impl<'a> RetentionContext<'a> {
    pub fn new(
        paths: &'a MemoryIdentityPaths,
        policy: RuntimeRetentionPolicy,
        now: &'a dyn Fn() -> i64,
    ) -> Self {
        Self {
            paths,
            policy,
            now,
            is_live_session: None,
            reflection_enabled: None,
            cancel: None,
            warn: None,
            before_claim_rename: None,
            budget: None,
            before_journal_claim: None,
            before_tombstone_delete: None,
        }
    }

    pub fn now_ms(&self) -> i64 {
        (self.now)()
    }

    pub fn aborted(&self) -> bool {
        self.cancel.is_some_and(|cancel| cancel())
    }

    pub fn check_abort(&self) -> Result<(), RetentionError> {
        check_abort(self.cancel)
    }

    pub fn is_live(&self, session_id: &str) -> bool {
        self.is_live_session
            .is_some_and(|is_live| is_live(session_id))
    }

    pub fn reflection_enabled(&self) -> bool {
        self.reflection_enabled.unwrap_or(true)
    }

    pub fn warn_with(&self, message: &str, fields: &[(&str, String)]) {
        if let Some(warn) = self.warn {
            warn(message, fields);
        }
    }
}

pub fn check_abort(cancel: Option<&dyn Fn() -> bool>) -> Result<(), RetentionError> {
    if cancel.is_some_and(|cancel| cancel()) {
        Err(RetentionError::Aborted)
    } else {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct StoreTally {
    pub removed: Vec<String>,
    pub kept: BTreeMap<String, RetentionKeepReason>,
}

impl StoreTally {
    pub fn keep(&mut self, name: &str, reason: RetentionKeepReason) {
        self.kept.insert(name.to_string(), reason);
    }

    pub fn result(&self) -> RetentionStoreResult {
        let mut removed = self.removed.clone();
        removed.sort();
        RetentionStoreResult {
            removed,
            kept: self.kept.clone(),
        }
    }
}

/// A path that is not there, or whose parent is not a directory.
pub fn is_gone(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Sorted entry names; a missing directory has none.
pub fn list_names(directory: &Path) -> io::Result<Vec<String>> {
    match read_dir_names(directory) {
        Ok(names) => Ok(names),
        Err(error) if is_gone(&error) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Modification time in epoch milliseconds, 0 when the platform reports none.
pub fn mtime_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// `lstat` that treats a vanished path as absent.
pub fn lstat_or_none(path: &Path) -> Option<std::fs::Metadata> {
    symlink_metadata(path).ok()
}

/// Newest mtime of the path and everything under it; symlinks count as themselves.
pub fn newest_mtime(path: &Path) -> io::Result<Option<i64>> {
    let Some(metadata) = lstat_or_none(path) else {
        return Ok(None);
    };
    let mut newest = mtime_ms(&metadata);
    if !metadata.is_dir() {
        return Ok(Some(newest));
    }
    for name in list_names(path)? {
        if let Some(inner) = newest_mtime(&path.join(name))?
            && inner > newest
        {
            newest = inner;
        }
    }
    Ok(Some(newest))
}

pub fn idle_since(path: &Path, cutoff_ms: i64) -> io::Result<bool> {
    Ok(newest_mtime(path)?.is_some_and(|newest| newest <= cutoff_ms))
}

/// Creates (or truncates) `path` with owner-only permissions and writes `bytes`.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    set_private_mode(&mut options);
    options.open(path)?.write_all(bytes)
}

/// Appends `bytes` to `path`, creating it with owner-only permissions.
pub fn append_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    set_private_mode(&mut options);
    options.open(path)?.write_all(bytes)
}

#[cfg(unix)]
fn set_private_mode(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_mode(_options: &mut std::fs::OpenOptions) {}
