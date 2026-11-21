//! Port of `commands/facts-failure-cause.ts` (omo #11909): plain-words classification of facts
//! failures for the /doctor and /facts surfaces. The ledger stores a machine reason; the reader
//! needs the one action to take. A transient cause (a timeout, a busy write lock) means the remedy
//! is `/facts retry` itself. A cause only the user can fix (no model for the extractor, secret-like
//! content) names what to fix instead of telling her to "fix the cause".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use memory_core::facts::failures_schema::{
    FactsFailureReason, FactsFailureRecord, FactsFailureState,
};

use crate::worker::failure_detail::child_failure_cause;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactsFailureAction {
    Retry,
    FixFirst,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsFailureCause {
    /// Short plain-words label; `None` when the cause is not known.
    pub label: Option<String>,
    pub action: FactsFailureAction,
    /// The config setting to name when the fix is a setting.
    pub setting: Option<&'static str>,
}

fn known(
    label: &str,
    action: FactsFailureAction,
    setting: Option<&'static str>,
) -> FactsFailureCause {
    FactsFailureCause {
        label: Some(label.to_owned()),
        action,
        setting,
    }
}

fn classify_facts_failure(record: &FactsFailureRecord) -> FactsFailureCause {
    use FactsFailureAction::{FixFirst, Retry};
    match record.last_reason {
        FactsFailureReason::Other => FactsFailureCause {
            label: None,
            action: Retry,
            setting: None,
        },
        FactsFailureReason::ChildExit => FactsFailureCause {
            label: Some(
                child_failure_cause(record.last_detail.as_deref())
                    .unwrap_or_else(|| "the facts child exited".to_owned()),
            ),
            action: FixFirst,
            setting: None,
        },
        FactsFailureReason::DeadlineExceeded => known("timeout", Retry, None),
        FactsFailureReason::MemoryWriteLockExhausted => {
            known("the memory write lock stayed busy", Retry, None)
        }
        FactsFailureReason::UnknownLiveness => {
            known("the facts child stopped responding", Retry, None)
        }
        FactsFailureReason::QuickCategoryUnavailable => known(
            "no model is available for facts extraction",
            FixFirst,
            Some("memory.facts.model"),
        ),
        FactsFailureReason::InvalidExtraction => known("parse error", FixFirst, None),
        FactsFailureReason::PayloadEntryOversize => {
            known("one entry is too large to extract", FixFirst, None)
        }
        FactsFailureReason::PayloadEnvelopeOversize => {
            known("the batch is too large to extract", FixFirst, None)
        }
        FactsFailureReason::SecretLikeContent => {
            known("secret-like content in the batch", FixFirst, None)
        }
        FactsFailureReason::ParentDirty => known(
            "the memory repository has uncommitted changes",
            FixFirst,
            None,
        ),
        // mhc-only reason (no upstream row): the facts child runs under the reflection sandbox policy.
        FactsFailureReason::SandboxUnavailable => known(
            "the facts sandbox is unavailable",
            FixFirst,
            Some("memory.reflection.sandbox"),
        ),
    }
}

fn parked_of(records: &[FactsFailureRecord]) -> Vec<&FactsFailureRecord> {
    records
        .iter()
        .filter(|record| record.state == FactsFailureState::Parked)
        .collect()
}

/// Label groups over parked records, most frequent first, label order breaking ties.
fn cause_counts(records: &[FactsFailureRecord]) -> Vec<(FactsFailureCause, usize)> {
    let mut by_label: BTreeMap<String, (FactsFailureCause, usize)> = BTreeMap::new();
    for record in parked_of(records) {
        let cause = classify_facts_failure(record);
        let Some(label) = cause.label.clone() else {
            continue;
        };
        by_label.entry(label).or_insert((cause, 0)).1 += 1;
    }
    let mut counts: Vec<(FactsFailureCause, usize)> = by_label.into_values().collect();
    counts.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.label.cmp(&right.0.label))
    });
    counts
}

/// The must-fix cause covering the most parked batches: the one the to-do names.
pub fn dominant_fix_first_cause(records: &[FactsFailureRecord]) -> Option<FactsFailureCause> {
    cause_counts(records)
        .into_iter()
        .map(|(cause, _)| cause)
        .find(|cause| cause.action == FactsFailureAction::FixFirst)
}

/// The run directory of the newest parked batch whose run log still exists. The failure id is the
/// ledger's per-launch `batchId`, not the run directory name, so each `<facts>/runs/<run>/ledger.json`
/// is read for the `batchId` the record names and a `runId` that names its own directory. A pruned
/// run yields no path, never one that does not exist; an unreadable runs directory yields none.
pub fn parked_run_path(
    records: &[FactsFailureRecord],
    facts_dir: Option<&Path>,
) -> Option<PathBuf> {
    let facts_dir = facts_dir?;
    let mut newest_first = parked_of(records);
    if newest_first.is_empty() {
        return None;
    }
    newest_first.sort_by(|left, right| {
        failure_ms(&right.last_failure_at).cmp(&failure_ms(&left.last_failure_at))
    });
    let runs_dir = facts_dir.join("runs");
    let mut names: Vec<String> = std::fs::read_dir(&runs_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with(".prune-"))
        .collect();
    names.sort();
    let mut by_batch_id: BTreeMap<String, PathBuf> = BTreeMap::new();
    for name in names {
        let run_dir = runs_dir.join(&name);
        let Ok(raw) = std::fs::read_to_string(run_dir.join("ledger.json")) else {
            continue;
        };
        let Ok(ledger) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(fields) = ledger.as_object() else {
            continue;
        };
        let Some(batch_id) = fields
            .get("batchId")
            .and_then(serde_json::Value::as_str)
            .filter(|batch_id| !batch_id.is_empty())
        else {
            continue;
        };
        if fields
            .get("runId")
            .is_some_and(|run_id| run_id.as_str() != Some(name.as_str()))
        {
            continue;
        }
        by_batch_id.entry(batch_id.to_owned()).or_insert(run_dir);
    }
    newest_first.into_iter().find_map(|record| {
        by_batch_id
            .get(record.last_failure_id.trim())
            .filter(|run_dir| run_dir.exists())
            .cloned()
    })
}

fn failure_ms(at: &str) -> Option<i64> {
    memory_core::support::time::parse_rfc3339(at)
}

/// The quieter segment after the to-do: cause counts over the parked batches plus the newest
/// parked batch's run path. An unknown cause contributes no label, never a guess, but the run path
/// still renders so the reader has somewhere to look.
pub fn facts_parked_cause_summary(
    records: &[FactsFailureRecord],
    facts_dir: Option<&Path>,
) -> Option<String> {
    if parked_of(records).is_empty() {
        return None;
    }
    let mut segments = Vec::new();
    let counts = cause_counts(records);
    if !counts.is_empty() {
        let list = counts
            .iter()
            .map(|(cause, count)| {
                format!("{} x{count}", cause.label.as_deref().unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join(", ");
        let noun = if counts.len() == 1 { "cause" } else { "causes" };
        segments.push(format!("{noun}: {list}"));
    }
    if let Some(run_dir) = parked_run_path(records, facts_dir) {
        segments.push(format!("last run: {}", run_dir.display()));
    }
    (!segments.is_empty()).then(|| segments.join("; "))
}

#[cfg(test)]
#[path = "facts_failure_cause_tests.rs"]
mod tests;
