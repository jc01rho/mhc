//! Port of `retention/legacy.ts`: stores no writer touches any more (design 2.6): `recall/runs/`
//! and `reflection-sessions/<runId>/` (still scanned by the run-id allocator). The
//! reflection-sessions directory itself stays because the sandbox grant expects it.

use super::budget::RetentionStore;
use super::guard::LazyRetentionGuard;
use super::policy::{
    RetentionContext, RetentionError, RetentionKeepReason, RetentionStoreResult, StoreTally,
    days_ms, idle_since, list_names,
};
use crate::fs::remove_tree;
use crate::fs::resilient::exists;
use crate::reflection::run_id_high_water::reflection_run_number;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRecallRunsOutcome {
    Absent,
    Kept,
    Removed,
}

impl LegacyRecallRunsOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Kept => "kept",
            Self::Removed => "removed",
        }
    }
}

pub fn prune_legacy_recall_runs(
    context: &RetentionContext<'_>,
) -> Result<LegacyRecallRunsOutcome, RetentionError> {
    let dir = context.paths.recall.join("runs");
    if !exists(&dir) {
        return Ok(LegacyRecallRunsOutcome::Absent);
    }
    if context
        .budget
        .is_some_and(|budget| budget.skip(RetentionStore::RecallRuns, "runs"))
    {
        return Ok(LegacyRecallRunsOutcome::Kept);
    }
    if context
        .budget
        .is_some_and(|budget| !budget.admit(RetentionStore::RecallRuns))
    {
        return Ok(LegacyRecallRunsOutcome::Kept);
    }
    let cutoff = context.now_ms() - days_ms(context.policy.retention_days);
    if !idle_since(&dir, cutoff)? {
        return Ok(LegacyRecallRunsOutcome::Kept);
    }
    remove_tree(&dir)?;
    if let Some(budget) = context.budget {
        budget.done(RetentionStore::RecallRuns, "runs");
    }
    Ok(LegacyRecallRunsOutcome::Removed)
}

pub fn prune_legacy_reflection_sessions(
    context: &RetentionContext<'_>,
    lazy_guard: &LazyRetentionGuard<'_>,
) -> Result<RetentionStoreResult, RetentionError> {
    let mut tally = StoreTally::default();
    let cutoff = context.now_ms() - days_ms(context.policy.retention_days);
    for name in list_names(&context.paths.reflection_sessions)? {
        if context
            .budget
            .is_some_and(|budget| budget.skip(RetentionStore::ReflectionSessions, &name))
        {
            continue;
        }
        if context
            .budget
            .is_some_and(|budget| !budget.admit(RetentionStore::ReflectionSessions))
        {
            break;
        }
        let path = context.paths.reflection_sessions.join(&name);
        if !idle_since(&path, cutoff)? {
            tally.keep(&name, RetentionKeepReason::Recent);
        } else {
            match lazy_guard.get()? {
                None => tally.keep(&name, RetentionKeepReason::SchedulerBusy),
                Some(guard) if reflection_run_number(&name) == guard.high_water => {
                    tally.keep(&name, RetentionKeepReason::Anchor);
                }
                Some(_) => match remove_tree(&path) {
                    Ok(()) => tally.removed.push(name.clone()),
                    Err(error) => {
                        context.warn_with(
                            "memory runtime retention could not remove a legacy reflection session",
                            &[
                                ("path", path.display().to_string()),
                                ("error", error.to_string()),
                            ],
                        );
                        tally.keep(&name, RetentionKeepReason::ChangedDuringRemoval);
                    }
                },
            }
        }
        if let Some(budget) = context.budget {
            budget.done(RetentionStore::ReflectionSessions, &name);
        }
    }
    Ok(tally.result())
}
