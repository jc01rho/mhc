//! Port of `retention/reflection-runs.ts`: folded reflection run directories
//! (`reflection/runs/<runId>/`, design 2.1). A run is removed only when its final.json names it,
//! its outcome is in the repo (`merged` / `no_changes`), it finished and was last touched more than
//! `retention_days` ago, it is neither reserved nor an anchor (the run-id high water, the newest
//! folded run), and its finalize lock is free. The claim is a rename to a `.prune-` tombstone under
//! that lock; the delete runs after release through `remove_tree`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::budget::RetentionStore;
use super::guard::{LazyRetentionGuard, reserved_run_ids};
use super::policy::{
    RetentionContext, RetentionError, RetentionKeepReason, RetentionStoreResult, StoreTally,
    WarnFn, days_ms, idle_since, is_gone, list_names,
};
use crate::fs::remove_tree;
use crate::fs::resilient::{exists, read_to_string, rename};
use crate::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, acquire_lock,
    create_lock_record, release_lock, run_finalization_lock_path,
};
use crate::reflection::run_id_high_water::reflection_run_number;
use crate::support::random::random_uuid;
use crate::support::time::parse_rfc3339;

pub const RETENTION_TOMBSTONE_PREFIX: &str = ".prune-";
const FOLDED_OUTCOMES: [&str; 2] = ["merged", "no_changes"];
const TERMINAL_SENTINELS: [&str; 2] = ["abandoned.json", "quarantined.json"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldedRun {
    pub name: String,
    pub finished_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunClassification {
    Folded(FoldedRun),
    Keep(RetentionKeepReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Removed,
    Keep(RetentionKeepReason),
}

pub fn prune_reflection_runs(
    context: &RetentionContext<'_>,
    lazy_guard: &LazyRetentionGuard<'_>,
) -> Result<RetentionStoreResult, RetentionError> {
    let runs_dir = context.paths.reflection.join("runs");
    let mut tally = StoreTally::default();
    let mut folded: Vec<FoldedRun> = Vec::new();
    let mut crash_tombstones: Vec<PathBuf> = Vec::new();
    for name in list_names(&runs_dir)? {
        if context
            .budget
            .is_some_and(|budget| !budget.proceed(RetentionStore::Runs))
        {
            return Ok(tally.result());
        }
        let run_dir = runs_dir.join(&name);
        if name.starts_with(RETENTION_TOMBSTONE_PREFIX) {
            crash_tombstones.push(run_dir);
            continue;
        }
        match classify_reflection_run(&run_dir, &name) {
            RunClassification::Keep(reason) => tally.keep(&name, reason),
            RunClassification::Folded(run) => folded.push(run),
        }
    }
    for run_dir in &crash_tombstones {
        if context
            .budget
            .is_some_and(|budget| !budget.admit(RetentionStore::Runs))
        {
            return Ok(tally.result());
        }
        remove_tombstone(run_dir, context.warn);
    }
    let newest = folded
        .iter()
        .max_by(|left, right| {
            left.finished_at
                .cmp(&right.finished_at)
                .then_with(|| left.name.cmp(&right.name))
        })
        .map(|run| run.name.clone());
    let cutoff = context.now_ms() - days_ms(context.policy.retention_days);
    for run in &folded {
        if context
            .budget
            .is_some_and(|budget| budget.skip(RetentionStore::Runs, &run.name))
        {
            continue;
        }
        if context
            .budget
            .is_some_and(|budget| !budget.admit(RetentionStore::Runs))
        {
            break;
        }
        let is_newest = newest.as_deref() == Some(run.name.as_str());
        let recent = !is_newest
            && (run.finished_at > cutoff || !idle_since(&runs_dir.join(&run.name), cutoff)?);
        if is_newest {
            tally.keep(&run.name, RetentionKeepReason::Anchor);
        } else if recent {
            tally.keep(&run.name, RetentionKeepReason::Recent);
        } else {
            match lazy_guard.get()? {
                None => tally.keep(&run.name, RetentionKeepReason::SchedulerBusy),
                Some(guard) if guard.reserved.contains(&run.name) => {
                    tally.keep(&run.name, RetentionKeepReason::Reserved);
                }
                Some(guard) if reflection_run_number(&run.name) == guard.high_water => {
                    tally.keep(&run.name, RetentionKeepReason::Anchor);
                }
                Some(_) => match claim_and_remove(context, &runs_dir, &run.name, cutoff)? {
                    ClaimOutcome::Removed => tally.removed.push(run.name.clone()),
                    ClaimOutcome::Keep(reason) => tally.keep(&run.name, reason),
                },
            }
        }
        if let Some(budget) = context.budget {
            budget.done(RetentionStore::Runs, &run.name);
        }
    }
    Ok(tally.result())
}

pub fn classify_reflection_run(run_dir: &Path, name: &str) -> RunClassification {
    let raw = match read_to_string(&run_dir.join("final.json")) {
        Ok(raw) => raw,
        Err(error) if is_gone(&error) => {
            return RunClassification::Keep(
                if TERMINAL_SENTINELS
                    .iter()
                    .any(|file| exists(&run_dir.join(file)))
                {
                    RetentionKeepReason::UnfoldedTerminal
                } else {
                    RetentionKeepReason::NoTerminal
                },
            );
        }
        Err(_) => return RunClassification::Keep(RetentionKeepReason::Malformed),
    };
    let Ok(Value::Object(record)) = serde_json::from_str::<Value>(&raw) else {
        return RunClassification::Keep(RetentionKeepReason::Malformed);
    };
    let finished_at = record
        .get("finishedAt")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    let outcome = record.get("outcome").and_then(Value::as_str);
    let (Some(finished_at), Some(outcome)) = (finished_at, outcome) else {
        return RunClassification::Keep(RetentionKeepReason::Malformed);
    };
    if record.get("runId").and_then(Value::as_str) != Some(name) {
        return RunClassification::Keep(RetentionKeepReason::Malformed);
    }
    if FOLDED_OUTCOMES.contains(&outcome) {
        RunClassification::Folded(FoldedRun {
            name: name.to_string(),
            finished_at,
        })
    } else {
        RunClassification::Keep(RetentionKeepReason::UnfoldedTerminal)
    }
}

/// Takes the run's finalize lock without waiting, renames the run to a tombstone under it and
/// deletes the tombstone after the lock is released. `cutoff` is the idle bound the run must meet.
pub fn claim_and_remove(
    context: &RetentionContext<'_>,
    runs_dir: &Path,
    name: &str,
    cutoff: i64,
) -> Result<ClaimOutcome, RetentionError> {
    let lock_path = run_finalization_lock_path(&context.paths.locks, name)
        .map_err(|error| RetentionError::Other(error.to_string()))?;
    let record = create_lock_record(
        "runtime-retention",
        CreateLockRecordOptions {
            run_id: Some(name.to_string()),
        },
    )
    .map_err(|error| RetentionError::Other(error.to_string()))?;
    let options = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        ..AcquireLockOptions::default()
    };
    match acquire_lock(&lock_path, &record, &options) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(_)) => {
            return Ok(ClaimOutcome::Keep(RetentionKeepReason::Locked));
        }
        Err(AcquireLockError::Aborted) => return Err(RetentionError::Aborted),
        Err(AcquireLockError::Io(error)) => return Err(RetentionError::Io(error)),
        Err(error) => return Err(RetentionError::Other(error.to_string())),
    }
    let claimed = claim_under_lock(context, runs_dir, name, cutoff);
    let released = release_lock(&lock_path, &record);
    let claimed = claimed?;
    released?;
    let tombstone = match claimed {
        Ok(tombstone) => tombstone,
        Err(reason) => return Ok(ClaimOutcome::Keep(reason)),
    };
    if let Some(hook) = context.before_tombstone_delete {
        hook(&tombstone);
    }
    remove_tombstone(&tombstone, context.warn);
    Ok(ClaimOutcome::Removed)
}

fn claim_under_lock(
    context: &RetentionContext<'_>,
    runs_dir: &Path,
    name: &str,
    cutoff: i64,
) -> Result<Result<PathBuf, RetentionKeepReason>, RetentionError> {
    if reserved_run_ids(&context.paths.reflection).contains(name) {
        return Ok(Err(RetentionKeepReason::Reserved));
    }
    if !idle_since(&runs_dir.join(name), cutoff)? {
        return Ok(Err(RetentionKeepReason::Recent));
    }
    let from = runs_dir.join(name);
    let tombstone = runs_dir.join(format!(
        "{RETENTION_TOMBSTONE_PREFIX}{name}-{}",
        random_uuid()
    ));
    let renamed = match context.before_claim_rename {
        Some(hook) => hook(&from, &tombstone),
        None => Ok(()),
    }
    .and_then(|()| rename(&from, &tombstone));
    if let Err(error) = renamed {
        context.warn_with(
            "memory runtime retention could not claim a run directory",
            &[("runId", name.to_string()), ("error", error.to_string())],
        );
        return Ok(Err(RetentionKeepReason::ClaimFailed));
    }
    Ok(Ok(tombstone))
}

/// A tombstone is unreachable by name; a failed delete is retried by the next pass.
pub fn remove_tombstone(path: &Path, warn: Option<WarnFn<'_>>) -> bool {
    match remove_tree(path) {
        Ok(()) => true,
        Err(error) => {
            if let Some(warn) = warn {
                warn(
                    "memory runtime retention could not delete a tombstone",
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

#[cfg(test)]
#[path = "reflection_runs_tests.rs"]
mod tests;
