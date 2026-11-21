//! Port of `retention/guard.ts`: the reflection reservation guard every reflection store removal
//! consults.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use super::policy::{RetentionContext, RetentionError};
use crate::fs::resilient::{create_dir_all, read_to_string};
use crate::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, WithLockError,
    create_lock_record, reflection_scheduler_lock_path, with_lock,
};
use crate::reflection::run_id_high_water::{
    RunIdHighWaterPaths, record_reflection_run_id_mark, scan_reflection_run_high_water,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflectionRetentionGuard {
    /// The highest run number on disk (H1 Guard 1): the trace carrying it is never removed.
    pub high_water: u64,
    pub reserved: BTreeSet<String>,
    /// Conversations named by `active.lock`/`pending.json` (J0): their journals are never touched.
    pub reserved_conversation_ids: BTreeSet<String>,
}

/// Reads the reservation state and the run-id high water under the `reflection-scheduler` lock, the
/// lock the allocator mints under, and raises the durable `run-id.json` mark before anything is
/// removed (H1 Guard 2). Contention means a reservation is in progress: the reflection stores are
/// skipped for this pass rather than waited on.
pub fn read_reflection_retention_guard(
    context: &RetentionContext<'_>,
) -> Result<Option<ReflectionRetentionGuard>, RetentionError> {
    create_dir_all(&context.paths.locks)?;
    let record = create_lock_record(
        "reflection-scheduler",
        CreateLockRecordOptions {
            run_id: Some("runtime-retention".to_string()),
        },
    )
    .map_err(|error| RetentionError::Other(error.to_string()))?;
    let lock_path = reflection_scheduler_lock_path(&context.paths.locks);
    let options = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        ..AcquireLockOptions::default()
    };
    let guarded = with_lock(&lock_path, &record, &options, || {
        let high_water = scan_reflection_run_high_water(RunIdHighWaterPaths::from(context.paths));
        if high_water > 0 {
            record_reflection_run_id_mark(&context.paths.reflection, high_water)?;
        }
        let reservations = read_reservations(&context.paths.reflection);
        Ok::<ReflectionRetentionGuard, std::io::Error>(ReflectionRetentionGuard {
            high_water,
            reserved: reservations.run_ids,
            reserved_conversation_ids: reservations.conversation_ids,
        })
    });
    match guarded {
        Ok(guard) => Ok(Some(guard)),
        Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => Ok(None),
        Err(WithLockError::Acquire(AcquireLockError::Aborted)) => Err(RetentionError::Aborted),
        Err(WithLockError::Acquire(AcquireLockError::Io(error)))
        | Err(WithLockError::User(error)) => Err(RetentionError::Io(error)),
        Err(WithLockError::Acquire(error)) => Err(RetentionError::Other(error.to_string())),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reservations {
    pub run_ids: BTreeSet<String>,
    pub conversation_ids: BTreeSet<String>,
}

/// Run ids named by `active.lock` and `pending.json`, read raw; an unreadable file names nothing.
pub fn reserved_run_ids(reflection_dir: &Path) -> BTreeSet<String> {
    read_reservations(reflection_dir).run_ids
}

/// Run ids and conversation ids named by `active.lock` and `pending.json`, read lock-free (the
/// doctors' estimate).
pub fn read_reservations(reflection_dir: &Path) -> Reservations {
    let mut reservations = Reservations::default();
    for name in ["active.lock", "pending.json"] {
        let Some(record) = read_to_string(&reflection_dir.join(name))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        else {
            continue;
        };
        if let Some(run_id) = record.get("runId").and_then(Value::as_str) {
            reservations.run_ids.insert(run_id.to_string());
        }
        let ids = record
            .get("request")
            .and_then(|request| request.get("conversationIds"))
            .and_then(Value::as_array);
        for id in ids.into_iter().flatten().filter_map(Value::as_str) {
            reservations.conversation_ids.insert(id.to_string());
        }
    }
    reservations
}

/// The guard taken only when a lock-free scan found a candidate (R3): a visit of an identity with
/// nothing to remove takes no lock and writes nothing, so a repo-less identity's mtimes stay as
/// they were and the transient sweep still reclaims it. The first call takes the scheduler lock
/// once.
pub struct LazyRetentionGuard<'a> {
    context: RetentionContext<'a>,
    resolved: RefCell<Option<Option<ReflectionRetentionGuard>>>,
}

impl<'a> LazyRetentionGuard<'a> {
    pub fn new(context: RetentionContext<'a>) -> Self {
        Self {
            context,
            resolved: RefCell::new(None),
        }
    }

    pub fn get(&self) -> Result<Option<ReflectionRetentionGuard>, RetentionError> {
        if let Some(resolved) = self.resolved.borrow().as_ref() {
            return Ok(resolved.clone());
        }
        let guard = read_reflection_retention_guard(&self.context)?;
        *self.resolved.borrow_mut() = Some(guard.clone());
        Ok(guard)
    }

    /// The guard once it was read and held.
    pub fn value(&self) -> Option<ReflectionRetentionGuard> {
        self.resolved.borrow().clone().flatten()
    }

    /// True once the scheduler lock was contended.
    pub fn busy(&self) -> bool {
        matches!(self.resolved.borrow().as_ref(), Some(None))
    }
}
