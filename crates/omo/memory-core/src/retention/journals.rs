//! Port of `retention/journals.ts`: transcript journals (`transcripts/<sessionId>/`, design 2.3),
//! classified from state.json:
//! - fully reflected (J1): `transcript.jsonl` goes after `retention_days` idle and state.json is
//!   stamped `transcript_pruned_at`, so a resumed session rebuilds it against the kept anchor;
//! - never reflected / no steps (J2): the whole journal goes after `abandoned_journal_days` idle;
//! - partly reflected (J3): `transcript.jsonl` goes after `abandoned_journal_days`, stamped like J1.
//!
//! Every class is judged from state.json, so a transcript that holds rows the state never counted
//! is kept as `state-behind-transcript`. The scan is lock-free; every predicate is re-checked under
//! the journal's own lock, files are unlinked under it, and the directory itself is removed only
//! after release (never recursively).

use std::io;
use std::path::Path;

use serde_json::{Map, Value};

use super::budget::RetentionStore;
use super::guard::LazyRetentionGuard;
use super::policy::{
    RetentionContext, RetentionError, RetentionKeepReason, RetentionStoreResult, StoreTally,
    days_ms, list_names, lstat_or_none, mtime_ms,
};
use super::reflection_health_signal::UnreflectedRetentionGate;
use super::reflection_runs::ClaimOutcome;
use crate::fs::resilient::{exists, read_to_string, remove_file, rename, write};
use crate::journal::cursor::{ReflectionTranscriptState, derive_state, initial_reflection_state};
use crate::journal::lock::{JournalLock, run_with_lock};
use crate::journal::store::{JournalError, TranscriptJournal, TranscriptJournalOptions};
use crate::support::random::random_uuid;
use crate::support::time::{format_rfc3339_millis, parse_rfc3339};

const TRANSCRIPT: &str = "transcript.jsonl";
const STATE: &str = "state.json";
const LOCK: &str = "state.lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalClass {
    Fully,
    Never,
    Partly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Class(JournalClass),
    Keep(RetentionKeepReason),
}

enum StateRead {
    Absent,
    Unreadable,
    State(Map<String, Value>),
}

enum Visit {
    Removed,
    Keep(RetentionKeepReason),
}

pub fn prune_transcript_journals(
    context: &RetentionContext<'_>,
    lazy_guard: &LazyRetentionGuard<'_>,
    gate: Option<&UnreflectedRetentionGate>,
    lock: &dyn JournalLock,
) -> Result<RetentionStoreResult, RetentionError> {
    let mut tally = StoreTally::default();
    for session_id in list_names(&context.paths.transcripts)? {
        if context
            .budget
            .is_some_and(|budget| budget.skip(RetentionStore::Journals, &session_id))
        {
            continue;
        }
        if context
            .budget
            .is_some_and(|budget| !budget.admit(RetentionStore::Journals))
        {
            break;
        }
        let outcome = match visit_journal(context, lazy_guard, gate, lock, &session_id) {
            Ok(outcome) => outcome,
            Err(error) => {
                if context.aborted() {
                    return Err(error);
                }
                Some(Visit::Keep(RetentionKeepReason::StateUnreadable))
            }
        };
        match outcome {
            Some(Visit::Removed) => tally.removed.push(session_id.clone()),
            Some(Visit::Keep(reason)) => tally.keep(&session_id, reason),
            None => {}
        }
        if let Some(budget) = context.budget {
            budget.done(RetentionStore::Journals, &session_id);
        }
    }
    Ok(tally.result())
}

fn visit_journal(
    context: &RetentionContext<'_>,
    lazy_guard: &LazyRetentionGuard<'_>,
    gate: Option<&UnreflectedRetentionGate>,
    lock: &dyn JournalLock,
    session_id: &str,
) -> Result<Option<Visit>, RetentionError> {
    let dir = context.paths.transcripts.join(session_id);
    if !lstat_or_none(&dir).is_some_and(|stats| stats.is_dir()) {
        return Ok(None);
    }
    if context.is_live(session_id) {
        return Ok(Some(Visit::Keep(RetentionKeepReason::Live)));
    }
    if exists(&dir.join(LOCK)) {
        return Ok(Some(Visit::Keep(RetentionKeepReason::Locked)));
    }
    if let Verdict::Keep(reason) = judge(context, &dir, false, gate)? {
        return Ok(Some(Visit::Keep(reason)));
    }
    let Some(guard) = lazy_guard.get()? else {
        return Ok(Some(Visit::Keep(RetentionKeepReason::SchedulerBusy)));
    };
    if guard.reserved_conversation_ids.contains(session_id) {
        return Ok(Some(Visit::Keep(RetentionKeepReason::Reserved)));
    }
    if let Some(hook) = context.before_journal_claim {
        hook(session_id);
    }
    Ok(Some(
        match remove_transcript_journal_under_lock(context, &dir, lock, gate)? {
            ClaimOutcome::Removed => Visit::Removed,
            ClaimOutcome::Keep(reason) => Visit::Keep(reason),
        },
    ))
}

fn judge(
    context: &RetentionContext<'_>,
    dir: &Path,
    holding_lock: bool,
    gate: Option<&UnreflectedRetentionGate>,
) -> Result<Verdict, RetentionError> {
    let state = match read_state(&dir.join(STATE))? {
        StateRead::Unreadable => return Ok(Verdict::Keep(RetentionKeepReason::StateUnreadable)),
        StateRead::Absent => None,
        StateRead::State(state) => Some(state),
    };
    let kind = match classify(state.as_ref()) {
        Verdict::Keep(reason) => return Ok(Verdict::Keep(reason)),
        Verdict::Class(kind) => kind,
    };
    if kind != JournalClass::Never && !exists(&dir.join(TRANSCRIPT)) {
        return Ok(Verdict::Keep(RetentionKeepReason::Pruned));
    }
    if kind != JournalClass::Fully && !context.policy.discard_unreflected_journals {
        return Ok(Verdict::Keep(RetentionKeepReason::UnreflectedRetentionOff));
    }
    if kind != JournalClass::Fully && !gate.is_some_and(|gate| gate.open) {
        return Ok(Verdict::Keep(RetentionKeepReason::ReflectionUnhealthy));
    }
    let days = if kind == JournalClass::Fully {
        context.policy.retention_days
    } else {
        context.policy.abandoned_journal_days
    };
    let cutoff = context.now_ms() - days_ms(days);
    let newest = newest_file_mtime(dir, holding_lock)?;
    if newest.is_none_or(|newest| newest > cutoff) {
        return Ok(Verdict::Keep(RetentionKeepReason::Recent));
    }
    let started_at = state
        .as_ref()
        .and_then(|state| state.get("last_reflection_started_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    if started_at.is_some_and(|started_at| started_at > cutoff) {
        return Ok(Verdict::Keep(RetentionKeepReason::Recent));
    }
    if let Some(state) = &state
        && !transcript_matches_state(dir, state)?
    {
        return Ok(Verdict::Keep(RetentionKeepReason::StateBehindTranscript));
    }
    Ok(Verdict::Class(kind))
}

/// The state's counts describe exactly the transcript on disk (REVIEW-6 M1): every byte is a
/// journal row, and the completed steps and unreflected bytes derived from those rows equal what
/// state.json recorded. A row appended after the last state write moves at least one of them, so
/// the class read from the state no longer holds and nothing is removed.
fn transcript_matches_state(
    dir: &Path,
    state: &Map<String, Value>,
) -> Result<bool, RetentionError> {
    let Some(size) = lstat_or_none(&dir.join(TRANSCRIPT)).map(|stats| stats.len()) else {
        return Ok(true);
    };
    let entries =
        TranscriptJournal::new(TranscriptJournalOptions::new(dir)).read_entries_snapshot()?;
    let row_bytes: u64 = entries
        .iter()
        .map(|entry| serde_json::to_vec(entry).map_or(0, |json| json.len() as u64 + 1))
        .sum();
    if row_bytes != size {
        return Ok(false);
    }
    let anchor = state
        .get("reflected_through_message_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let seed = ReflectionTranscriptState {
        reflected_through_message_id: anchor,
        reflected_completed_steps: count(state.get("reflected_completed_steps")).unwrap_or(0)
            as usize,
        ..initial_reflection_state()
    };
    let derived = derive_state(&seed, &entries);
    if Some(derived.total_completed_steps as u64) != count(state.get("total_completed_steps")) {
        return Ok(false);
    }
    Ok(match state.get("unreflected_bytes") {
        None => true,
        Some(recorded) => recorded.as_u64() == derived.unreflected_bytes.map(|bytes| bytes as u64),
    })
}

fn classify(state: Option<&Map<String, Value>>) -> Verdict {
    let Some(state) = state else {
        return Verdict::Class(JournalClass::Never);
    };
    let (Some(total), Some(reflected), Some(since)) = (
        count(state.get("total_completed_steps")),
        count(state.get("reflected_completed_steps")),
        count(state.get("steps_since_last_successful_reflection")),
    ) else {
        return Verdict::Keep(RetentionKeepReason::Unclassified);
    };
    let anchor = state
        .get("reflected_through_message_id")
        .and_then(Value::as_str)
        .is_some_and(|anchor| !anchor.is_empty());
    if reflected == 0 && !anchor {
        return Verdict::Class(JournalClass::Never);
    }
    if total > 0 && since == 0 && anchor {
        return if state.get("pending_compaction") == Some(&Value::Bool(true)) {
            Verdict::Keep(RetentionKeepReason::PendingCompaction)
        } else {
            Verdict::Class(JournalClass::Fully)
        };
    }
    if reflected > 0 && since > 0 && anchor {
        return Verdict::Class(JournalClass::Partly);
    }
    Verdict::Keep(RetentionKeepReason::Unclassified)
}

/// An absent count is 0 (older states omit it); a present one must be a non-negative integer.
fn count(value: Option<&Value>) -> Option<u64> {
    let Some(value) = value else {
        return Some(0);
    };
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0 && number.fract() == 0.0)
            .map(|number| number as u64)
    })
}

fn read_state(path: &Path) -> Result<StateRead, RetentionError> {
    let raw = match read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(StateRead::Absent),
        Err(error) => return Err(RetentionError::Io(error)),
    };
    Ok(match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(state)) => StateRead::State(state),
        _ => StateRead::Unreadable,
    })
}

/// Newest mtime over the directory and its files. Under the lock the lock file is this pass's own
/// and creating it moved the directory mtime, so only the other files are judged.
fn newest_file_mtime(dir: &Path, holding_lock: bool) -> io::Result<Option<i64>> {
    let mut newest = if holding_lock {
        0
    } else {
        match lstat_or_none(dir) {
            Some(stats) => mtime_ms(&stats),
            None => return Ok(None),
        }
    };
    for name in list_names(dir)? {
        if holding_lock && name == LOCK {
            continue;
        }
        if let Some(stats) = lstat_or_none(&dir.join(&name)) {
            newest = newest.max(mtime_ms(&stats));
        }
    }
    Ok(Some(newest))
}

/// Re-judges the journal under its own lock, unlinks its files under it, and removes the emptied
/// directory only after the lock is released.
pub fn remove_transcript_journal_under_lock(
    context: &RetentionContext<'_>,
    dir: &Path,
    lock: &dyn JournalLock,
    gate: Option<&UnreflectedRetentionGate>,
) -> Result<ClaimOutcome, RetentionError> {
    let locked = run_with_lock(
        lock,
        &dir.join(LOCK),
        || Ok(unlink_under_lock(context, dir, gate)),
        context.cancel,
    );
    let verdict = match locked {
        Ok(inner) => inner?,
        Err(JournalError::LockTimeout(_)) => {
            return Ok(ClaimOutcome::Keep(RetentionKeepReason::Locked));
        }
        Err(error) => return Err(error.into()),
    };
    let kind = match verdict {
        Verdict::Keep(reason) => return Ok(ClaimOutcome::Keep(reason)),
        Verdict::Class(kind) => kind,
    };
    if kind != JournalClass::Never {
        return Ok(ClaimOutcome::Removed);
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => Ok(ClaimOutcome::Removed),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ClaimOutcome::Removed),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists
            ) =>
        {
            Ok(ClaimOutcome::Keep(
                RetentionKeepReason::ChangedDuringRemoval,
            ))
        }
        Err(error) => Err(RetentionError::Io(error)),
    }
}

fn unlink_under_lock(
    context: &RetentionContext<'_>,
    dir: &Path,
    gate: Option<&UnreflectedRetentionGate>,
) -> Result<Verdict, RetentionError> {
    let current = judge(context, dir, true, gate)?;
    let Verdict::Class(kind) = current else {
        return Ok(current);
    };
    if kind == JournalClass::Never {
        unlink_if_present(&dir.join(STATE))?;
        unlink_if_present(&dir.join(TRANSCRIPT))?;
    } else {
        stamp_pruned(&dir.join(STATE), context.now_ms())?;
        unlink_if_present(&dir.join(TRANSCRIPT))?;
    }
    Ok(current)
}

fn unlink_if_present(path: &Path) -> io::Result<()> {
    match remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn stamp_pruned(state_path: &Path, now: i64) -> Result<(), RetentionError> {
    let StateRead::State(state) = read_state(state_path)? else {
        return Err(RetentionError::Other(format!(
            "journal state vanished under its lock: {}",
            state_path.display()
        )));
    };
    let mut stamped: Map<String, Value> = state
        .into_iter()
        .filter(|(key, _)| key != "reflected_through_byte_offset")
        .collect();
    stamped.insert(
        "transcript_pruned_at".to_string(),
        Value::String(format_rfc3339_millis(now)),
    );
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(stamped))
            .map_err(|error| RetentionError::Other(error.to_string()))?
    );
    let temporary = state_path.with_file_name(format!("{STATE}.tmp-{}", random_uuid()));
    let written = write(&temporary, body.as_bytes()).and_then(|()| rename(&temporary, state_path));
    if let Err(error) = written {
        let _ = remove_file(&temporary);
        return Err(RetentionError::Io(error));
    }
    Ok(())
}

/// One `transcripts/` entry for the cap and the doctors' scans, tolerant like the pass's visit: a
/// name that is not a directory is skipped (`None`), and an entry whose judgement fails (a
/// state.json that is a directory, an I/O error) is kept as `state-unreadable` so the scan goes on.
/// An abort propagates.
pub fn classify_journal_entry(
    context: &RetentionContext<'_>,
    dir: &Path,
    gate: &UnreflectedRetentionGate,
) -> Result<Option<Verdict>, RetentionError> {
    context.check_abort()?;
    if !lstat_or_none(dir).is_some_and(|stats| stats.is_dir()) {
        return Ok(None);
    }
    match judge(context, dir, false, Some(gate)) {
        Ok(verdict) => Ok(Some(verdict)),
        Err(error) => {
            if context.aborted() {
                Err(error)
            } else {
                Ok(Some(Verdict::Keep(RetentionKeepReason::StateUnreadable)))
            }
        }
    }
}

#[cfg(test)]
#[path = "journals_tests.rs"]
mod tests;
