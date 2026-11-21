//! Port of `retention/pass.ts`: one retention pass over an identity's runtime (design 2, section
//! 6). The reflection stores and the journal rules run only when the reservation guard could be
//! read (it also records the run-id mark first). The walk is bounded by `pass_budget_ms` and by the
//! abort signal, checked between items: a pass that stops early persists a resume cursor in
//! `runtime/retention.json` and the next pass continues there; a pass that completes deletes the
//! cursor. The size cap keeps its own resume point in the same file, which only the cap reads:
//! every pass walks the stores whatever the cap left.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use super::budget::{PassBudget, RetentionCursor, RetentionStore};
use super::cap::{
    RuntimeCapCursor, RuntimeSizeCapOptions, RuntimeSizeCapReport, run_runtime_size_cap,
};
use super::completions::prune_completions;
use super::guard::LazyRetentionGuard;
use super::journals::prune_transcript_journals;
use super::legacy::{
    LegacyRecallRunsOutcome, prune_legacy_recall_runs, prune_legacy_reflection_sessions,
};
use super::log::{RetentionLogInput, RetentionLogLimits, append_retention_log};
use super::policy::{RetentionContext, RetentionError, RetentionStoreResult, days_ms};
use super::recall_ledgers::prune_recall_ledgers;
use super::reflection_health_signal::{UnreflectedRetentionGate, read_unreflected_retention_gate};
use super::reflection_runs::prune_reflection_runs;
use super::sidecars::{PruneKibitzerSidecarsOptions, prune_kibitzer_sidecars};
use super::stamp::{
    RetentionResumePoints, clear_retention_cursor, read_retention_resume_points,
    write_retention_resume_points,
};
use crate::journal::lock::DefaultJournalLock;

#[derive(Clone, Copy)]
pub struct RuntimeRetentionOptions<'a> {
    /// The shared pass context; its `budget` is replaced by the pass's own.
    pub context: RetentionContext<'a>,
    /// Encoded sidecar directory names this process's live sessions own.
    pub owned_sidecars: Option<&'a BTreeSet<String>>,
    /// `memory.runtime.max_bytes`: when set, the size cap (design 3) runs after retention; `0`
    /// measures only.
    pub max_bytes: Option<u64>,
    /// The cap's journal floor for a caller that cannot see live sessions (the host sweep, `omo
    /// doctor --fix`): `retention_days`, so an idle but alive session keeps its journal. Unset
    /// keeps the 24 h floor.
    pub journal_floor_ms: Option<i64>,
}

impl<'a> RuntimeRetentionOptions<'a> {
    pub fn new(context: RetentionContext<'a>) -> Self {
        Self {
            context,
            owned_sidecars: None,
            max_bytes: None,
            journal_floor_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportCursor {
    Store(RetentionCursor),
    Cap(RuntimeCapCursor),
}

impl ReportCursor {
    pub fn store(&self) -> &str {
        match self {
            Self::Store(cursor) => &cursor.store,
            Self::Cap(_) => "cap",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRetentionReport {
    /// `None` when the reflection-scheduler lock was busy and the reflection stores were skipped.
    pub high_water: Option<u64>,
    pub skipped: Vec<String>,
    pub runs: RetentionStoreResult,
    pub completions: RetentionStoreResult,
    pub reflection_sessions: RetentionStoreResult,
    pub journals: RetentionStoreResult,
    pub sidecars: RetentionStoreResult,
    pub ledgers: RetentionStoreResult,
    pub recall_runs: LegacyRecallRunsOutcome,
    /// Read only when `discard_unreflected_journals` is on; `None` when off or when the pass
    /// stopped while reading it.
    pub unreflected_gate: Option<UnreflectedRetentionGate>,
    /// Set when the pass stopped early (budget or abort), the store walk's or else the cap's;
    /// persisted for the next pass.
    pub cursor: Option<ReportCursor>,
    /// Present when `max_bytes` was set and retention finished inside the budget.
    pub cap: Option<RuntimeSizeCapReport>,
}

pub fn run_runtime_retention(
    options: &RuntimeRetentionOptions<'_>,
) -> Result<RuntimeRetentionReport, RetentionError> {
    let base = &options.context;
    base.check_abort()?;
    let paths = base.paths;
    let stored = read_retention_resume_points(&paths.runtime);
    let budget = PassBudget::new(
        base.now,
        base.policy.pass_budget_ms,
        base.cancel,
        stored.cursor.clone(),
    );
    let context = base.with_budget(&budget);
    let guard = LazyRetentionGuard::new(context);
    let runs = prune_reflection_runs(&context, &guard)?;
    let completions = prune_completions(&context, &guard)?;
    let reflection_sessions = prune_legacy_reflection_sessions(&context, &guard)?;
    let unreflected_gate = if base.policy.discard_unreflected_journals {
        read_unreflected_retention_gate(&paths.reflection, base.reflection_enabled(), &|| {
            budget.proceed(RetentionStore::Journals)
        })?
    } else {
        None
    };
    let journals = prune_transcript_journals(
        &context,
        &guard,
        unreflected_gate.as_ref(),
        &DefaultJournalLock,
    )?;
    let sidecar_skip = |name: &str| budget.skip(RetentionStore::Sidecars, name);
    let sidecar_admit = || budget.admit(RetentionStore::Sidecars);
    let sidecar_done = |name: &str| budget.done(RetentionStore::Sidecars, name);
    let mut sidecar_options = PruneKibitzerSidecarsOptions::new(&paths.recall, &paths.locks);
    sidecar_options.now = Some(base.now);
    sidecar_options.max_age_ms = Some(days_ms(base.policy.retention_days));
    sidecar_options.owned = options.owned_sidecars;
    sidecar_options.warn = base.warn;
    sidecar_options.before_claim_rename = base.before_claim_rename;
    sidecar_options.before_tombstone_delete = base.before_tombstone_delete;
    sidecar_options.skip = Some(&sidecar_skip);
    sidecar_options.admit = Some(&sidecar_admit);
    sidecar_options.done = Some(&sidecar_done);
    let sidecar_result = prune_kibitzer_sidecars(&sidecar_options);
    let mut sidecar_removed = sidecar_result.pruned.clone();
    sidecar_removed.sort();
    let sidecars = RetentionStoreResult {
        removed: sidecar_removed,
        kept: sidecar_result.kept_reasons.clone(),
    };
    let ledgers = prune_recall_ledgers(&context)?;
    let recall_runs = prune_legacy_recall_runs(&context)?;
    // The cap (design 3) runs last, inside what is left of the same budget, and resumes its own
    // tier walk from its own resume point, which the store walk never reads (#12538 review r4 M).
    // A pass that stops before the cap keeps that resume point; a pass without a cap drops it.
    let mut cap_resume: Option<RuntimeCapCursor> = if options.max_bytes.is_none() {
        None
    } else {
        stored.cap.clone()
    };
    let mut cap: Option<RuntimeSizeCapReport> = None;
    if let Some(max_bytes) = options.max_bytes
        && budget.stopped().is_none()
    {
        let remaining_ms = budget.remaining_ms();
        let deadline = Instant::now() + Duration::from_millis(remaining_ms);
        let measure_cancel = || base.aborted() || Instant::now() >= deadline;
        let mut cap_options = RuntimeSizeCapOptions::new(*base, max_bytes);
        cap_options.owned_sidecars = options.owned_sidecars;
        cap_options.journal_floor_ms = options.journal_floor_ms;
        cap_options.budget_ms = Some(remaining_ms);
        cap_options.measure_cancel = Some(&measure_cancel);
        cap_options.resume = cap_resume.as_ref();
        let report = run_runtime_size_cap(&cap_options)?;
        cap_resume = report.cursor.clone();
        cap = Some(report);
    }
    log_removals(
        base,
        &[
            ("runs", &runs.removed),
            ("completions", &completions.removed),
            ("reflectionSessions", &reflection_sessions.removed),
            ("journals", &journals.removed),
            ("sidecars", &sidecars.removed),
            ("ledgers", &ledgers.removed),
        ],
        recall_runs,
        cap.as_ref(),
    );
    // A cap that could not even measure hands its resume point back unchanged; marked stalled, it
    // waits for the next interval instead of re-running retention at every bind (#12538 review r5
    // L1).
    let cap_stalled = cap.as_ref().is_some_and(|cap| {
        cap.skipped
            .iter()
            .any(|reason| reason == "measure-over-budget")
    });
    let stopped = budget.stopped();
    if stopped.is_some() || cap_resume.is_some() {
        write_retention_resume_points(
            &paths.runtime,
            &RetentionResumePoints {
                cursor: stopped.clone(),
                cap_stalled: cap_resume.is_some() && cap_stalled,
                cap: cap_resume,
            },
        )?;
        if budget.aborted() {
            base.check_abort()?;
        }
    } else if stored.cursor.is_some() || stored.cap.is_some() {
        clear_retention_cursor(&paths.runtime)?;
    }
    let cursor = stopped.map(ReportCursor::Store).or_else(|| {
        cap.as_ref()
            .and_then(|cap| cap.cursor.clone())
            .map(ReportCursor::Cap)
    });
    Ok(RuntimeRetentionReport {
        high_water: guard.value().map(|guard| guard.high_water),
        skipped: if guard.busy() {
            vec!["reflection-scheduler-busy".to_string()]
        } else {
            Vec::new()
        },
        runs,
        completions,
        reflection_sessions,
        journals,
        sidecars,
        ledgers,
        recall_runs,
        unreflected_gate,
        cursor,
        cap,
    })
}

fn log_removals(
    context: &RetentionContext<'_>,
    stores: &[(&str, &Vec<String>)],
    recall_runs: LegacyRecallRunsOutcome,
    cap: Option<&RuntimeSizeCapReport>,
) {
    let mut removed: Vec<(String, Vec<String>)> = stores
        .iter()
        .map(|(store, names)| ((*store).to_string(), (*names).clone()))
        .collect();
    if recall_runs == LegacyRecallRunsOutcome::Removed {
        removed.push(("legacy".to_string(), vec!["recall/runs".to_string()]));
    }
    for eviction in cap.map(|cap| cap.evicted.as_slice()).unwrap_or_default() {
        let key = format!("cap:{}", eviction.tier.as_str());
        match removed.iter_mut().find(|(store, _)| *store == key) {
            Some((_, names)) => names.push(eviction.name.clone()),
            None => removed.push((key, vec![eviction.name.clone()])),
        }
    }
    let input = RetentionLogInput {
        at: context.now_ms(),
        removed,
        cap_evicted_bytes: cap.map(|cap| cap.evicted.iter().map(|entry| entry.bytes).sum::<u64>()),
    };
    // The removals already happened; a log that cannot be written must not fail the pass.
    if let Err(error) = append_retention_log(
        &context.paths.runtime,
        &input,
        &RetentionLogLimits::default(),
    ) {
        context.warn_with(
            "memory runtime retention could not write its removal log",
            &[("error", error.to_string())],
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionCapSummary {
    pub cap_bytes: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub evicted: usize,
    pub evicted_bytes: u64,
    pub over_cap_bytes: u64,
    pub cap_kept_high_water: Vec<String>,
}

/// Bytes-free summary for one log line: removed counts per store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionSummary {
    pub removed: usize,
    pub runs: usize,
    pub completions: usize,
    pub reflection_sessions: usize,
    pub journals: usize,
    pub sidecars: usize,
    pub ledgers: usize,
    pub recall_runs: LegacyRecallRunsOutcome,
    pub skipped: Vec<String>,
    pub interrupted_at: Option<String>,
    pub cap: Option<RetentionCapSummary>,
}

pub fn summarize_runtime_retention(report: &RuntimeRetentionReport) -> RetentionSummary {
    let stores = [
        &report.runs,
        &report.completions,
        &report.reflection_sessions,
        &report.journals,
        &report.sidecars,
        &report.ledgers,
    ];
    let removed = stores
        .iter()
        .map(|store| store.removed.len())
        .sum::<usize>()
        + usize::from(report.recall_runs == LegacyRecallRunsOutcome::Removed)
        + report.cap.as_ref().map_or(0, |cap| cap.evicted.len());
    RetentionSummary {
        removed,
        runs: report.runs.removed.len(),
        completions: report.completions.removed.len(),
        reflection_sessions: report.reflection_sessions.removed.len(),
        journals: report.journals.removed.len(),
        sidecars: report.sidecars.removed.len(),
        ledgers: report.ledgers.removed.len(),
        recall_runs: report.recall_runs,
        skipped: report.skipped.clone(),
        interrupted_at: report
            .cursor
            .as_ref()
            .map(|cursor| cursor.store().to_string()),
        cap: report.cap.as_ref().map(|cap| RetentionCapSummary {
            cap_bytes: cap.max_bytes,
            bytes_before: cap.before_bytes,
            bytes_after: cap.after_bytes,
            evicted: cap.evicted.len(),
            evicted_bytes: cap.evicted.iter().map(|entry| entry.bytes).sum(),
            over_cap_bytes: cap.exceeded_bytes,
            cap_kept_high_water: cap
                .kept
                .iter()
                .filter(|(_, reason)| reason.as_str() == "high-water")
                .map(|(name, _)| name.clone())
                .collect(),
        }),
    }
}

#[cfg(test)]
#[path = "pass_tests.rs"]
mod tests;
