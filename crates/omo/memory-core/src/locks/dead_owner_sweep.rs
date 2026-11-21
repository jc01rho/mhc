//! Port of `locks/dead-owner-sweep.ts`: reclaims every `*.lock` in a directory whose recorded
//! owner's pid is gone on this host (the same proof every contender applies; never age). A lock
//! nobody contends for again is otherwise kept forever. Reclaim goes through the acquire path with
//! no wait, so it follows the same race-safe recovery protocol every contender uses, and the lock is
//! released at once. One lock that cannot be read or reclaimed is reported through `on_failure` and
//! skipped; the sweep never fails as a whole.

use std::io::ErrorKind;
use std::path::Path;

use crate::support::host::hostname;

use super::acquire::{
    AcquireLockError, AcquireLockOptions, acquire_lock, is_lock_owner_proven_dead, release_lock,
};
use super::lock_record::{
    CreateLockRecordOptions, LockRecord, create_lock_record, parse_lock_record,
};
use super::process_identity::{ProcessLiveness, get_pid_liveness};

const LOCK_FILE_SUFFIX: &str = ".lock";

/// A sweep checks only owners whose pid is already gone; a live pid would cost a start-identity
/// probe (a process spawn) per lock on every sweep. A recycled pid is still caught by the next
/// contender.
fn proven_dead_owner(lock_path: &Path) -> std::io::Result<Option<LockRecord>> {
    let raw = match std::fs::read_to_string(lock_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(owner) = parse_lock_record(&raw) else {
        return Ok(None);
    };
    if owner.hostname != hostname() || get_pid_liveness(owner.pid) != ProcessLiveness::Dead {
        return Ok(None);
    }
    Ok(is_lock_owner_proven_dead(&owner).then_some(owner))
}

fn reclaim_if_dead_owner(
    lock_path: &Path,
    on_reclaimed: &mut impl FnMut(&Path, &LockRecord),
) -> Result<bool, String> {
    let is_file = std::fs::symlink_metadata(lock_path).is_ok_and(|status| status.is_file());
    let previous_owner = if is_file {
        proven_dead_owner(lock_path).map_err(|error| error.to_string())?
    } else {
        None
    };
    let Some(previous_owner) = previous_owner else {
        return Ok(false);
    };
    let sweeper = create_lock_record("dead-owner-sweep", CreateLockRecordOptions { run_id: None })
        .map_err(|error| error.to_string())?;
    let no_wait = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        ..Default::default()
    };
    match acquire_lock(lock_path, &sweeper, &no_wait) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(_)) => return Ok(false),
        Err(error) => return Err(error.to_string()),
    }
    release_lock(lock_path, &sweeper).map_err(|error| error.to_string())?;
    on_reclaimed(lock_path, &previous_owner);
    Ok(true)
}

/// Returns how many locks were reclaimed. `on_failure` hears of each lock (or the directory) that
/// could not be inspected or reclaimed; `on_reclaimed` hears the proven-dead owner of each reclaimed lock.
pub fn sweep_dead_owner_locks(
    lock_directory: &Path,
    mut on_failure: impl FnMut(&Path, &str),
    mut on_reclaimed: impl FnMut(&Path, &LockRecord),
) -> usize {
    let entries = match std::fs::read_dir(lock_directory) {
        Ok(entries) => entries,
        Err(error) => {
            if error.kind() != ErrorKind::NotFound {
                on_failure(lock_directory, &error.to_string());
            }
            return 0;
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(LOCK_FILE_SUFFIX))
        .collect();
    names.sort();
    let mut reclaimed = 0;
    for name in names {
        let lock_path = lock_directory.join(&name);
        match reclaim_if_dead_owner(&lock_path, &mut on_reclaimed) {
            Ok(true) => reclaimed += 1,
            Ok(false) => {}
            Err(message) => on_failure(&lock_path, &message),
        }
    }
    reclaimed
}

#[cfg(test)]
#[path = "dead_owner_sweep_tests.rs"]
mod tests;
