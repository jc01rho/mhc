//! Port of `retention/sidecars.ts` (moved from omo-senpi `kibitzer/observe-prune.ts`, design 2.4):
//! a kibitzer sidecar directory idle for the retention window is removed, never while a session
//! owns it. Ownership is an ordinary memory-core lock (`locks/recall-sidecar.<encoded-session>.lock`)
//! held from sidecar creation to session shutdown, so a sweep from another process sees a live
//! owner (contention) and skips, while a crashed owner is recovered only on pid/start-identity
//! proof. This process's own live sessions are skipped before any lock is touched. A directory is
//! claimed by rename under its owner lock and deleted afterwards, and a tombstone left by a crash
//! is cleared by the next sweep.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use super::policy::{BeforeClaimRename, RetentionKeepReason, WarnFn, lstat_or_none, mtime_ms};
use super::reflection_runs::ClaimOutcome;
use crate::fs::remove_tree;
use crate::fs::resilient::{read_dir_names, rename};
use crate::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, acquire_lock,
    create_lock_record, release_lock,
};
use crate::support::random::random_uuid;
use crate::support::time::now_millis;

pub const KIBITZER_SIDECAR_OWNER_LOCK_PURPOSE: &str = "recall-sidecar";
pub const KIBITZER_SIDECAR_TOMBSTONE_PREFIX: &str = ".prune-";
const SIDECARS_DIRNAME: &str = "sidecars";
const OWNER_LOCK_PREFIX: &str = "recall-sidecar.";
const OWNER_LOCK_SUFFIX: &str = ".lock";

/// A sidecar directory idle this long is removed by the next sweep, unless a session still owns it.
pub const KIBITZER_SIDECAR_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// `^[A-Za-z0-9_-]+$`: only names the sidecar writer produced are candidates.
pub fn is_kibitzer_encoded_session(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn kibitzer_sidecars_root(recall_dir: &Path) -> PathBuf {
    recall_dir.join(SIDECARS_DIRNAME)
}

pub fn kibitzer_sidecar_owner_lock_path_for(locks_dir: &Path, encoded_session: &str) -> PathBuf {
    locks_dir.join(format!(
        "{OWNER_LOCK_PREFIX}{encoded_session}{OWNER_LOCK_SUFFIX}"
    ))
}

#[derive(Clone, Copy)]
pub struct PruneKibitzerSidecarsOptions<'a> {
    pub recall_dir: &'a Path,
    pub locks_dir: &'a Path,
    pub now: Option<&'a dyn Fn() -> i64>,
    pub max_age_ms: Option<i64>,
    /// Encoded directory names of this process's live sessions: skipped before any lock is touched.
    pub owned: Option<&'a BTreeSet<String>>,
    pub warn: Option<WarnFn<'a>>,
    pub before_claim_rename: Option<BeforeClaimRename<'a>>,
    pub before_tombstone_delete: Option<&'a dyn Fn(&Path)>,
    /// Resume and budget hooks of the retention pass; absent for the session-shutdown sweep.
    pub skip: Option<&'a dyn Fn(&str) -> bool>,
    pub admit: Option<&'a dyn Fn() -> bool>,
    pub done: Option<&'a dyn Fn(&str)>,
}

impl<'a> PruneKibitzerSidecarsOptions<'a> {
    pub fn new(recall_dir: &'a Path, locks_dir: &'a Path) -> Self {
        Self {
            recall_dir,
            locks_dir,
            now: None,
            max_age_ms: None,
            owned: None,
            warn: None,
            before_claim_rename: None,
            before_tombstone_delete: None,
            skip: None,
            admit: None,
            done: None,
        }
    }

    fn now_ms(&self) -> i64 {
        self.now.map_or_else(now_millis, |now| now())
    }

    fn warn_with(&self, message: &str, fields: &[(&str, String)]) {
        if let Some(warn) = self.warn {
            warn(message, fields);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneKibitzerSidecarsResult {
    /// Encoded directory names removed.
    pub pruned: Vec<String>,
    /// Encoded directory names inspected and left in place.
    pub kept: Vec<String>,
    pub kept_reasons: BTreeMap<String, RetentionKeepReason>,
}

enum SidecarVisit {
    Removed,
    Keep(RetentionKeepReason),
    Ignored,
}

/// Removes every sidecar directory idle for `max_age_ms`, except one owned by a live session: this
/// process's own sessions by name, any process's by its owner lock (contention means live; a dead
/// owner is recovered only on proof). The idle check is repeated under the lock so a session that
/// came alive during the scan keeps its directory. Anything that is not a directory is ignored.
pub fn prune_kibitzer_sidecars(
    options: &PruneKibitzerSidecarsOptions<'_>,
) -> PruneKibitzerSidecarsResult {
    let max_age_ms = options.max_age_ms.unwrap_or(KIBITZER_SIDECAR_RETENTION_MS);
    let root = kibitzer_sidecars_root(options.recall_dir);
    let names = match read_dir_names(&root) {
        Ok(names) => names,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                options.warn_with(
                    "kibitzer sidecar sweep listing failed",
                    &[
                        ("path", root.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
            Vec::new()
        }
    };
    let mut pruned = Vec::new();
    let mut kept_reasons = BTreeMap::new();
    for name in &names {
        let path = root.join(name);
        let is_tombstone = name.starts_with(KIBITZER_SIDECAR_TOMBSTONE_PREFIX);
        if !is_tombstone {
            if options.skip.is_some_and(|skip| skip(name)) {
                continue;
            }
            if options.admit.is_some_and(|admit| !admit()) {
                break;
            }
        }
        match visit_sidecar(options, name, &path, max_age_ms) {
            SidecarVisit::Removed => pruned.push(name.clone()),
            SidecarVisit::Keep(reason) => {
                kept_reasons.insert(name.clone(), reason);
            }
            SidecarVisit::Ignored => {}
        }
        if !is_tombstone && let Some(done) = options.done {
            done(name);
        }
    }
    PruneKibitzerSidecarsResult {
        pruned,
        kept: kept_reasons.keys().cloned().collect(),
        kept_reasons,
    }
}

fn visit_sidecar(
    options: &PruneKibitzerSidecarsOptions<'_>,
    name: &str,
    path: &Path,
    max_age_ms: i64,
) -> SidecarVisit {
    if name.starts_with(KIBITZER_SIDECAR_TOMBSTONE_PREFIX) {
        remove(path, options.warn);
        return SidecarVisit::Ignored;
    }
    if options.owned.is_some_and(|owned| owned.contains(name)) {
        return SidecarVisit::Keep(RetentionKeepReason::Owned);
    }
    if !is_kibitzer_encoded_session(name) {
        return SidecarVisit::Ignored;
    }
    if !lstat_or_none(path).is_some_and(|stats| stats.is_dir()) {
        return SidecarVisit::Ignored;
    }
    if !idle_for(path, options.now_ms(), max_age_ms) {
        return SidecarVisit::Keep(RetentionKeepReason::Recent);
    }
    let lock_path = kibitzer_sidecar_owner_lock_path_for(options.locks_dir, name);
    match claim_kibitzer_sidecar(path, &lock_path, options.now_ms(), max_age_ms, options) {
        ClaimOutcome::Removed => SidecarVisit::Removed,
        ClaimOutcome::Keep(reason) => SidecarVisit::Keep(reason),
    }
}

/// True when neither the directory nor anything directly inside it changed within `max_age_ms`.
fn idle_for(dir: &Path, at: i64, max_age_ms: i64) -> bool {
    shallow_newest_mtime(dir).is_some_and(|newest| at - newest >= max_age_ms)
}

fn shallow_newest_mtime(dir: &Path) -> Option<i64> {
    let mut newest = mtime_ms(&lstat_or_none(dir)?);
    for name in read_dir_names(dir).unwrap_or_default() {
        if let Some(stats) = lstat_or_none(&dir.join(name)) {
            newest = newest.max(mtime_ms(&stats));
        }
    }
    Some(newest)
}

/// Takes the directory's owner lock without waiting (a live owner keeps it, a dead one is
/// recovered on proof), renames the directory to a tombstone under it, and deletes the tombstone
/// only after the lock is released, so a slow delete never holds a session's owner lock.
pub fn claim_kibitzer_sidecar(
    dir: &Path,
    lock_path: &Path,
    at: i64,
    max_age_ms: i64,
    options: &PruneKibitzerSidecarsOptions<'_>,
) -> ClaimOutcome {
    let record = match create_lock_record(
        KIBITZER_SIDECAR_OWNER_LOCK_PURPOSE,
        CreateLockRecordOptions::default(),
    ) {
        Ok(record) => record,
        Err(error) => {
            warn_lock_failure(options, lock_path, &error.to_string());
            return ClaimOutcome::Keep(RetentionKeepReason::Locked);
        }
    };
    let acquire_options = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        ..AcquireLockOptions::default()
    };
    match acquire_lock(lock_path, &record, &acquire_options) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(_)) => {
            return ClaimOutcome::Keep(RetentionKeepReason::Locked);
        }
        Err(error) => {
            warn_lock_failure(options, lock_path, &error.to_string());
            return ClaimOutcome::Keep(RetentionKeepReason::Locked);
        }
    }
    let claimed = claim_under_lock(dir, at, max_age_ms, options);
    if let Err(error) = release_lock(lock_path, &record) {
        options.warn_with(
            "kibitzer sidecar sweep lock release failed",
            &[
                ("lockPath", lock_path.display().to_string()),
                ("error", error.to_string()),
            ],
        );
    }
    let tombstone = match claimed {
        Ok(tombstone) => tombstone,
        Err(reason) => return ClaimOutcome::Keep(reason),
    };
    if let Some(hook) = options.before_tombstone_delete {
        hook(&tombstone);
    }
    remove(&tombstone, options.warn);
    ClaimOutcome::Removed
}

fn warn_lock_failure(options: &PruneKibitzerSidecarsOptions<'_>, lock_path: &Path, error: &str) {
    options.warn_with(
        "kibitzer sidecar sweep lock failed",
        &[
            ("lockPath", lock_path.display().to_string()),
            ("error", error.to_string()),
        ],
    );
}

fn claim_under_lock(
    dir: &Path,
    at: i64,
    max_age_ms: i64,
    options: &PruneKibitzerSidecarsOptions<'_>,
) -> Result<PathBuf, RetentionKeepReason> {
    if !idle_for(dir, at, max_age_ms) {
        return Err(RetentionKeepReason::Recent);
    }
    let base_name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tombstone = dir.with_file_name(format!(
        "{KIBITZER_SIDECAR_TOMBSTONE_PREFIX}{base_name}-{}",
        random_uuid()
    ));
    let renamed = match options.before_claim_rename {
        Some(hook) => hook(dir, &tombstone),
        None => Ok(()),
    }
    .and_then(|()| rename(dir, &tombstone));
    match renamed {
        Ok(()) => Ok(tombstone),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(RetentionKeepReason::ChangedDuringRemoval)
        }
        Err(error) => {
            options.warn_with(
                "kibitzer sidecar sweep claim failed",
                &[
                    ("path", dir.display().to_string()),
                    ("error", error.to_string()),
                ],
            );
            Err(RetentionKeepReason::ClaimFailed)
        }
    }
}

fn remove(path: &Path, warn: Option<WarnFn<'_>>) -> bool {
    match remove_tree(path) {
        Ok(()) => true,
        Err(error) => {
            if let Some(warn) = warn {
                warn(
                    "kibitzer sidecar sweep removal failed",
                    &[
                        ("path", path.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
            false
        }
    }
}
