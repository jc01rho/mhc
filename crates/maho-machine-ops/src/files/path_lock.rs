//! Per-canonical-path ordering for mutations. senpi's `files/mutation-queue.ts` also serves cross-copy
//! stuck-module bookkeeping through a realm registry; none of that exists in one Rust process, so this is
//! only its core: every spelling of one file shares one slot, so two mutations of that file never run at
//! once and the later write cannot silently discard the earlier one.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, PoisonError};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::cancel::CancellationToken;
use crate::errors::MachineOpError;
use crate::files::automount_trigger::automount_trigger_path;
use crate::files::bounded_realpath::{
    RESOLUTION_DEADLINE, ResolutionTimedOut, canonicalize_path,
    fold_path_for_case_insensitive_filesystem, is_missing_path_error, with_resolution_deadline,
};
use crate::files::realpath_without_open::realpath_without_open_strict_blocking;
use crate::node_path::resolve;

type Slots = HashMap<String, Arc<AsyncMutex<()>>>;

fn slots() -> &'static StdMutex<Slots> {
    static SLOTS: OnceLock<StdMutex<Slots>> = OnceLock::new();
    SLOTS.get_or_init(Default::default)
}

/// Every spelling of one file must produce one key. `canonicalize` supplies that identity, including the
/// on-disk case of an alias such as `Notes.txt` vs `notes.txt`; a path that does not exist yet keys on its
/// resolved form. On a wedged mount `canonicalize` never returns, so the resolution is bounded and falls
/// back to the open-free walker, and the key is case-folded on filesystems that ignore case so the
/// fallback and the fast path agree on the same file.
pub async fn mutation_queue_key(file_path: &Path) -> Result<String, MachineOpError> {
    let resolved = resolve(Path::new(""), file_path);
    if let Some(trigger_path) = automount_trigger_path(&resolved).await {
        return Ok(fold_path_for_case_insensitive_filesystem(&trigger_path));
    }
    let canonical =
        match with_resolution_deadline(canonicalize_path(&resolved), RESOLUTION_DEADLINE).await {
            Ok(Ok(canonical)) => canonical,
            Ok(Err(error)) if is_missing_path_error(&error) => resolved,
            Ok(Err(error)) => return Err(error.into()),
            Err(ResolutionTimedOut) => realpath_without_open_strict_blocking(resolved).await?,
        };
    Ok(fold_path_for_case_insensitive_filesystem(
        &canonical.to_string_lossy(),
    ))
}

/// The holder's side of a slot; dropping it releases the slot.
pub struct FileMutationLock {
    guard: Option<OwnedMutexGuard<()>>,
    slot: Arc<AsyncMutex<()>>,
    key: String,
}

/// Forget a slot nobody holds or waits for. The map and `slot` are the only two owners left then.
fn prune_idle_slot(key: &str, slot: &Arc<AsyncMutex<()>>) {
    let mut map = slots().lock().unwrap_or_else(PoisonError::into_inner);
    let idle = map
        .get(key)
        .is_some_and(|entry| Arc::ptr_eq(entry, slot) && Arc::strong_count(entry) == 2);
    if idle {
        map.remove(key);
    }
}

impl Drop for FileMutationLock {
    fn drop(&mut self) {
        drop(self.guard.take());
        prune_idle_slot(&self.key, &self.slot);
    }
}

async fn acquire_keyed(
    key: String,
    signal: &CancellationToken,
) -> Result<FileMutationLock, MachineOpError> {
    let slot = {
        let mut map = slots().lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(map.entry(key.clone()).or_default())
    };
    let acquired = tokio::select! {
        biased;
        () = signal.cancelled() => None,
        guard = Arc::clone(&slot).lock_owned() => Some(guard),
    };
    match acquired {
        Some(guard) => Ok(FileMutationLock {
            guard: Some(guard),
            slot,
            key,
        }),
        None => {
            prune_idle_slot(&key, &slot);
            Err(MachineOpError::aborted())
        }
    }
}

/// Wait for the slot of `file_path`'s canonical identity, or fail `aborted` when `signal` is cancelled
/// while waiting.
pub async fn acquire_file_mutation_lock(
    file_path: &Path,
    signal: &CancellationToken,
) -> Result<FileMutationLock, MachineOpError> {
    let key = mutation_queue_key(file_path).await?;
    acquire_keyed(key, signal).await
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::{Context, Waker};

    use super::*;
    use crate::errors::OpErrorCode;

    fn is_slot_registered(key: &str) -> bool {
        slots()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(key)
    }

    #[tokio::test]
    async fn a_second_holder_of_one_key_waits_until_the_first_releases() {
        let key = "path_lock::tests::wait";
        let signal = CancellationToken::new();
        let first = acquire_keyed(key.to_owned(), &signal).await.unwrap();
        let mut second = Box::pin(acquire_keyed(key.to_owned(), &signal));
        let mut context = Context::from_waker(Waker::noop());
        assert!(second.as_mut().poll(&mut context).is_pending());
        drop(first);
        let second_lock = second.await.unwrap();
        assert!(is_slot_registered(key));
        drop(second_lock);
        assert!(!is_slot_registered(key));
    }

    #[tokio::test]
    async fn different_keys_do_not_wait_for_each_other() {
        let signal = CancellationToken::new();
        let _a = acquire_keyed("path_lock::tests::a".to_owned(), &signal)
            .await
            .unwrap();
        let _b = acquire_keyed("path_lock::tests::b".to_owned(), &signal)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_cancelled_waiter_fails_aborted_and_leaves_the_holder_alone() {
        let key = "path_lock::tests::cancel";
        let holder = acquire_keyed(key.to_owned(), &CancellationToken::new())
            .await
            .unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let error = acquire_keyed(key.to_owned(), &cancelled)
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, OpErrorCode::Aborted);
        assert!(is_slot_registered(key));
        drop(holder);
        assert!(!is_slot_registered(key));
    }

    #[tokio::test]
    async fn every_spelling_of_one_existing_file_shares_one_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("dir")).unwrap();
        std::fs::write(dir.path().join("shared.txt"), "x").unwrap();
        let plain = mutation_queue_key(&dir.path().join("shared.txt"))
            .await
            .unwrap();
        let dotted = mutation_queue_key(&dir.path().join("dir/../shared.txt"))
            .await
            .unwrap();
        let other = mutation_queue_key(&dir.path().join("dir")).await.unwrap();
        assert_eq!(plain, dotted);
        assert_ne!(plain, other);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_and_its_target_share_one_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink("real.txt", dir.path().join("alias.txt")).unwrap();
        let real = mutation_queue_key(&dir.path().join("real.txt"))
            .await
            .unwrap();
        let alias = mutation_queue_key(&dir.path().join("alias.txt"))
            .await
            .unwrap();
        assert_eq!(real, alias);
    }
}
