//! Port of `reflection/release-without-promotion.ts` (omo #12517).
//!
//! Releases `run_id`'s reservation as a failure WITHOUT promoting `pending.json` (DESIGN.r2 4.1b, H3).
//! A completion would promote the pending run under the caller's pid; from the CLI that pid exits
//! at once and the promoted run would itself be reaped as an interrupted launch. This applies the
//! same failure bookkeeping as a `failed` completion, never writes `pending.json`, and unlinks
//! `active.lock`. The next session promotes the pending run under its own pid
//! (`ReflectionReservationStore::promote_pending`). Lock contention is its own error, because the
//! scheduler lock is taken with a zero wait.
//!
//! mhc has no reflection park file (omo `ReflectionParkFile`), so there is no park transition to apply.

use std::fmt;
use std::path::Path;

use crate::journal::store::{TranscriptJournal, TranscriptJournalOptions};
use crate::locks::acquire::{
    AcquireLockError, AcquireLockOptions, LockContentionError, WithLockError, with_lock,
};
use crate::locks::domains::reflection_scheduler_lock_path;
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};

use super::machine::ReflectionTrigger;
use super::reservation::read_run;

pub struct ReleaseWithoutPromotionOptions<'a> {
    pub reflection_dir: &'a Path,
    pub transcripts_dir: &'a Path,
    pub locks_dir: &'a Path,
    pub run_id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseWithoutPromotionResult {
    pub released: bool,
    pub pending_run_id: Option<String>,
}

#[derive(Debug)]
pub enum ReleaseError {
    Contention(Box<LockContentionError>),
    Failed(String),
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contention(error) => error.fmt(f),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ReleaseError {}

fn failed(error: impl fmt::Display) -> ReleaseError {
    ReleaseError::Failed(error.to_string())
}

fn read_pending_run_id(reflection_dir: &Path) -> Result<Option<String>, ReleaseError> {
    let raw = match std::fs::read_to_string(reflection_dir.join("pending.json")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failed(error)),
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(None);
    };
    Ok(parsed
        .get("runId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

pub fn release_reservation_without_promotion(
    options: &ReleaseWithoutPromotionOptions<'_>,
) -> Result<ReleaseWithoutPromotionResult, ReleaseError> {
    let record = create_lock_record(
        "reflection-scheduler",
        CreateLockRecordOptions {
            run_id: Some(options.run_id.to_string()),
        },
    )
    .map_err(failed)?;
    let acquire = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        ..Default::default()
    };
    with_lock(
        &reflection_scheduler_lock_path(options.locks_dir),
        &record,
        &acquire,
        || release_under_lock(options),
    )
    .map_err(|error| match error {
        WithLockError::Acquire(AcquireLockError::Contention(contention)) => {
            ReleaseError::Contention(contention)
        }
        WithLockError::Acquire(error) => failed(error),
        WithLockError::User(error) => error,
    })
}

fn release_under_lock(
    options: &ReleaseWithoutPromotionOptions<'_>,
) -> Result<ReleaseWithoutPromotionResult, ReleaseError> {
    let active_path = options.reflection_dir.join("active.lock");
    let not_released = ReleaseWithoutPromotionResult {
        released: false,
        pending_run_id: None,
    };
    let Some(active) = read_run(&active_path).map_err(failed)? else {
        return Ok(not_released);
    };
    if active.run_id != options.run_id {
        return Ok(not_released);
    }
    if !matches!(
        active.request.trigger,
        ReflectionTrigger::Manual | ReflectionTrigger::Dream
    ) {
        for conversation_id in &active.request.conversation_ids {
            let journal_dir = options.transcripts_dir.join(conversation_id);
            if !journal_dir.is_dir() {
                continue;
            }
            TranscriptJournal::new(TranscriptJournalOptions::new(journal_dir))
                .record_reflection_failure()
                .map_err(failed)?;
        }
    }
    let pending_run_id = read_pending_run_id(options.reflection_dir)?;
    match std::fs::remove_file(&active_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(error)),
    }
    Ok(ReleaseWithoutPromotionResult {
        released: true,
        pending_run_id,
    })
}

#[cfg(test)]
#[path = "release_without_promotion_tests.rs"]
mod tests;
