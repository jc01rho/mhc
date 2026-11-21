//! Port of `retention/completions.ts`: consumed completion records
//! (`reflection/completions/<runId>.json`, design 2.2). They are the allocator's durable trace of a
//! finished run, so nothing is removed until `run-id.json` covers the record's number (H1 Guard 2),
//! and the high-water record itself is never removed (Guard 1).

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use super::budget::RetentionStore;
use super::guard::LazyRetentionGuard;
use super::policy::{
    RetentionContext, RetentionError, RetentionKeepReason, RetentionStoreResult, StoreTally,
    days_ms, list_names,
};
use crate::fs::resilient::{exists, read_to_string, remove_file};
use crate::reflection::run_id_high_water::{read_reflection_run_id_mark, reflection_run_number};
use crate::support::time::parse_rfc3339;

/// `readReflectionHealth` reads the latest records for its streak and last-success window.
pub const COMPLETIONS_KEEP_NEWEST: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub run_id: String,
    pub finished_at: i64,
    pub consumed: bool,
}

pub fn prune_completions(
    context: &RetentionContext<'_>,
    lazy_guard: &LazyRetentionGuard<'_>,
) -> Result<RetentionStoreResult, RetentionError> {
    let dir = context.paths.reflection.join("completions");
    let mut tally = StoreTally::default();
    let mut records: Vec<Completion> = Vec::new();
    for name in list_names(&dir)? {
        let Some(run_id) = name.strip_suffix(".json") else {
            continue;
        };
        if context
            .budget
            .is_some_and(|budget| !budget.proceed(RetentionStore::Completions))
        {
            return Ok(tally.result());
        }
        match read_completion(&dir.join(&name), run_id) {
            None => tally.keep(run_id, RetentionKeepReason::Malformed),
            Some(record) => records.push(record),
        }
    }
    let newest = newest_run_ids(&records);
    records.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    let cutoff = context.now_ms() - days_ms(context.policy.completions_days);
    for record in &records {
        if context
            .budget
            .is_some_and(|budget| budget.skip(RetentionStore::Completions, &record.run_id))
        {
            continue;
        }
        if context
            .budget
            .is_some_and(|budget| !budget.admit(RetentionStore::Completions))
        {
            break;
        }
        let number = reflection_run_number(&record.run_id);
        if newest.contains(&record.run_id) {
            tally.keep(&record.run_id, RetentionKeepReason::Newest);
        } else if !record.consumed {
            tally.keep(&record.run_id, RetentionKeepReason::PendingDelivery);
        } else if record.finished_at > cutoff {
            tally.keep(&record.run_id, RetentionKeepReason::Recent);
        } else if exists(&context.paths.reflection.join("runs").join(&record.run_id)) {
            tally.keep(&record.run_id, RetentionKeepReason::RunPresent);
        } else if let Some(reason) = keep_by_guard(context, lazy_guard, number)? {
            tally.keep(&record.run_id, reason);
        } else {
            remove_file(&dir.join(format!("{}.json", record.run_id))).or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })?;
            tally.removed.push(record.run_id.clone());
        }
        if let Some(budget) = context.budget {
            budget.done(RetentionStore::Completions, &record.run_id);
        }
    }
    Ok(tally.result())
}

/// The newest `COMPLETIONS_KEEP_NEWEST` records by `finished_at`, ties broken by run id, newest
/// first.
pub fn newest_run_ids(records: &[Completion]) -> BTreeSet<String> {
    let mut newest_first: Vec<&Completion> = records.iter().collect();
    newest_first.sort_by(|left, right| {
        right
            .finished_at
            .cmp(&left.finished_at)
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    newest_first
        .into_iter()
        .take(COMPLETIONS_KEEP_NEWEST)
        .map(|record| record.run_id.clone())
        .collect()
}

/// The mark is read only after the guard recorded it (H1 Guard 2): no mark, no removal.
fn keep_by_guard(
    context: &RetentionContext<'_>,
    lazy_guard: &LazyRetentionGuard<'_>,
    number: u64,
) -> Result<Option<RetentionKeepReason>, RetentionError> {
    let Some(guard) = lazy_guard.get()? else {
        return Ok(Some(RetentionKeepReason::SchedulerBusy));
    };
    let mark = read_reflection_run_id_mark(&context.paths.reflection);
    if mark.is_none_or(|mark| number == 0 || number > mark) {
        return Ok(Some(RetentionKeepReason::NoMark));
    }
    if number == guard.high_water {
        return Ok(Some(RetentionKeepReason::Anchor));
    }
    Ok(None)
}

pub fn read_completion(path: &Path, run_id: &str) -> Option<Completion> {
    let parsed: Value = serde_json::from_str(&read_to_string(path).ok()?).ok()?;
    let record = parsed.as_object()?;
    let finished_at = parse_rfc3339(record.get("finishedAt")?.as_str()?)?;
    if record.get("schemaVersion")?.as_i64() != Some(1)
        || record.get("runId")?.as_str() != Some(run_id)
    {
        return None;
    }
    let consumed = match record.get("delivery")?.get("status")?.as_str()? {
        "pending" => false,
        "consumed" => true,
        _ => return None,
    };
    Some(Completion {
        run_id: run_id.to_string(),
        finished_at,
        consumed,
    })
}

#[cfg(test)]
#[path = "completions_tests.rs"]
mod tests;
