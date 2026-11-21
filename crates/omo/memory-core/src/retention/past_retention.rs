//! Port of `retention/past-retention.ts`: what the next retention pass would remove, counted
//! read-only for `/doctor` (design 6): the same classifiers the pass uses, minus the claims.
//! Anchors are approximated by the scanned high water and the newest folded run; reservations and
//! lock contention are decided by the pass itself.

use std::collections::BTreeSet;

use super::completions::{Completion, newest_run_ids, read_completion};
use super::journals::{JournalClass, Verdict, classify_journal_entry};
use super::policy::{
    RetentionContext, RetentionError, RetentionKeepReason, RuntimeRetentionPolicy, days_ms,
    idle_since, list_names, lstat_or_none, mtime_ms,
};
use super::reflection_health_signal::{UnreflectedRetentionGate, read_unreflected_retention_gate};
use super::reflection_runs::{
    RETENTION_TOMBSTONE_PREFIX, RunClassification, classify_reflection_run,
};
use super::sidecars::{is_kibitzer_encoded_session, kibitzer_sidecars_root};
use super::size::measure_tree_bytes;
use crate::fs::resilient::exists;
use crate::reflection::run_id_high_water::{
    RunIdHighWaterPaths, read_reflection_run_id_mark, reflection_run_number,
    scan_reflection_run_high_water,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimePastRetention {
    pub runs: usize,
    pub journals: usize,
    pub sidecars: usize,
    /// Journals no retention row classifies (kept, design r2 2.3 `unclassified`).
    pub unclassified_journals: usize,
}

/// Folded runs the next pass would remove, by its rules (`reflection_runs`), minus what only the
/// pass decides.
fn past_retention_run_names(context: &RetentionContext<'_>) -> Result<Vec<String>, RetentionError> {
    let cutoff = context.now_ms() - days_ms(context.policy.retention_days);
    let runs_dir = context.paths.reflection.join("runs");
    let high_water = scan_reflection_run_high_water(RunIdHighWaterPaths::from(context.paths));
    let mut folded: Vec<(String, i64)> = Vec::new();
    for name in list_names(&runs_dir)? {
        if name.starts_with(RETENTION_TOMBSTONE_PREFIX) {
            continue;
        }
        if let RunClassification::Folded(run) =
            classify_reflection_run(&runs_dir.join(&name), &name)
        {
            folded.push((name, run.finished_at));
        }
    }
    let newest = folded
        .iter()
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)))
        .map(|(name, _)| name.clone());
    let mut names = Vec::new();
    for (name, finished_at) in &folded {
        if newest.as_deref() == Some(name.as_str())
            || reflection_run_number(name) == high_water
            || *finished_at > cutoff
        {
            continue;
        }
        if idle_since(&runs_dir.join(name), cutoff)? {
            names.push(name.clone());
        }
    }
    Ok(names)
}

/// Counts what the next pass would remove from runs, journals and sidecars. `context.cancel` is
/// the doctors' measurement deadline; once it fires the count fails with the abort.
pub fn count_runtime_past_retention(
    context: &RetentionContext<'_>,
) -> Result<RuntimePastRetention, RetentionError> {
    let cutoff = context.now_ms() - days_ms(context.policy.retention_days);
    let runs = past_retention_run_names(context)?.len();

    let mut journals = 0;
    let mut unclassified_journals = 0;
    let gate = read_unreflected_retention_gate(
        &context.paths.reflection,
        context.reflection_enabled(),
        &|| true,
    )?
    .unwrap_or_else(UnreflectedRetentionGate::open);
    for session_id in list_names(&context.paths.transcripts)? {
        if context.is_live(&session_id) {
            continue;
        }
        let dir = context.paths.transcripts.join(&session_id);
        match classify_journal_entry(context, &dir, &gate)? {
            None => {}
            Some(Verdict::Class(_)) => journals += 1,
            Some(Verdict::Keep(RetentionKeepReason::Unclassified)) => unclassified_journals += 1,
            Some(Verdict::Keep(_)) => {}
        }
    }

    let mut sidecars = 0;
    let root = kibitzer_sidecars_root(&context.paths.recall);
    for name in list_names(&root)? {
        if !is_kibitzer_encoded_session(&name) {
            continue;
        }
        let path = root.join(&name);
        if lstat_or_none(&path).is_some_and(|stats| stats.is_dir()) && idle_since(&path, cutoff)? {
            sidecars += 1;
        }
    }
    Ok(RuntimePastRetention {
        runs,
        journals,
        sidecars,
        unclassified_journals,
    })
}

/// Bytes held by journals never or only partly folded into memory (the input cap tiers 4/5 and
/// J2/J3 would discard): whole never-reflected journals, the transcript of partly-reflected ones.
/// Reported by both doctor surfaces when `discard_unreflected_journals` is off and the cap cannot
/// be met.
pub fn measure_unreflected_journal_bytes(
    context: &RetentionContext<'_>,
) -> Result<u64, RetentionError> {
    // Age does not matter here: every unreflected journal counts, so the classifier's idle window
    // is open.
    let far_future = || i64::MAX;
    let open_policy = RuntimeRetentionPolicy {
        retention_days: 0.0,
        abandoned_journal_days: 0.0,
        discard_unreflected_journals: true,
        ..context.policy
    };
    let open_context = context.with_policy_and_now(open_policy, &far_future);
    let open_gate = UnreflectedRetentionGate::open();
    let mut bytes = 0;
    for session_id in list_names(&context.paths.transcripts)? {
        let dir = context.paths.transcripts.join(&session_id);
        let Some(verdict) = classify_journal_entry(&open_context, &dir, &open_gate)? else {
            continue;
        };
        match verdict {
            // A transcript holding rows its state never counted is unreflected input kept in place.
            Verdict::Keep(RetentionKeepReason::StateBehindTranscript) => {
                bytes += measure_tree_bytes(&dir, None)?;
            }
            Verdict::Keep(_) => {}
            Verdict::Class(JournalClass::Never) => bytes += measure_tree_bytes(&dir, None)?,
            Verdict::Class(JournalClass::Partly) => {
                bytes += measure_tree_bytes(&dir.join("transcript.jsonl"), None)?;
            }
            Verdict::Class(JournalClass::Fully) => {}
        }
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimePastRetentionOtherStores {
    pub completions: usize,
    pub ledgers: usize,
    /// Legacy `reflection-sessions` entries, plus one for `recall/runs`.
    pub legacy: usize,
}

/// The stores [`count_runtime_past_retention`] leaves out, counted read-only by the pass's own
/// rules (`completions`, `recall_ledgers`, `legacy`) for the `--fix --dry-run` preview. The pass
/// records the scanned high water as `run-id.json` before it removes anything, so the preview
/// reads the mark as at least that high water. Live sessions are unknown here, as they are to the
/// host sweep.
pub fn count_runtime_past_retention_other_stores(
    context: &RetentionContext<'_>,
) -> Result<RuntimePastRetentionOtherStores, RetentionError> {
    let paths = context.paths;
    let high_water = scan_reflection_run_high_water(RunIdHighWaterPaths::from(paths));
    // The pass removes runs before completions, so a run it removes no longer keeps its
    // completion (`run-present`).
    let removed_runs: BTreeSet<String> = past_retention_run_names(context)?.into_iter().collect();
    let mark = read_reflection_run_id_mark(&paths.reflection)
        .unwrap_or(0)
        .max(high_water);
    let completions_cutoff = context.now_ms() - days_ms(context.policy.completions_days);
    let retention_cutoff = context.now_ms() - days_ms(context.policy.retention_days);

    let dir = paths.reflection.join("completions");
    let mut records: Vec<Completion> = Vec::new();
    for name in list_names(&dir)? {
        context.check_abort()?;
        let Some(run_id) = name.strip_suffix(".json") else {
            continue;
        };
        if let Some(record) = read_completion(&dir.join(&name), run_id) {
            records.push(record);
        }
    }
    let newest = newest_run_ids(&records);
    let mut completions = 0;
    for record in &records {
        let number = reflection_run_number(&record.run_id);
        if newest.contains(&record.run_id)
            || !record.consumed
            || record.finished_at > completions_cutoff
        {
            continue;
        }
        if !removed_runs.contains(&record.run_id)
            && exists(&paths.reflection.join("runs").join(&record.run_id))
        {
            continue;
        }
        if number == 0 || number > mark || number == high_water {
            continue;
        }
        completions += 1;
    }

    let mut ledgers = 0;
    for name in list_names(&paths.recall_ledger)? {
        context.check_abort()?;
        if !name.ends_with(".json") {
            continue;
        }
        let stats = lstat_or_none(&paths.recall_ledger.join(&name));
        if stats.is_some_and(|stats| stats.is_file() && mtime_ms(&stats) <= completions_cutoff) {
            ledgers += 1;
        }
    }

    let mut legacy = 0;
    for name in list_names(&paths.reflection_sessions)? {
        context.check_abort()?;
        if reflection_run_number(&name) != high_water
            && idle_since(&paths.reflection_sessions.join(&name), retention_cutoff)?
        {
            legacy += 1;
        }
    }
    let recall_runs = paths.recall.join("runs");
    if exists(&recall_runs) && idle_since(&recall_runs, retention_cutoff)? {
        legacy += 1;
    }
    Ok(RuntimePastRetentionOtherStores {
        completions,
        ledgers,
        legacy,
    })
}
