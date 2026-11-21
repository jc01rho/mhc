use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;

use super::*;
use crate::identity::layout::MemoryIdentityPaths;
use crate::locks::{
    AcquireLockOptions, CreateLockRecordOptions, acquire_lock, create_lock_record,
    is_lock_owner_proven_dead, release_lock,
};
use crate::retention::policy::days_ms;
use crate::retention::test_support::{
    NOW, age_tree, days_ago, encoded_session, exists, exited_pid, identity, sidecar, tombstones,
    write_json,
};

fn owner_lock_path(locks_dir: &Path, session_id: &str) -> PathBuf {
    kibitzer_sidecar_owner_lock_path_for(locks_dir, &encoded_session(session_id))
}

fn write_dead_owner_lock(lock_path: &Path) {
    let mut record = create_lock_record(
        KIBITZER_SIDECAR_OWNER_LOCK_PURPOSE,
        CreateLockRecordOptions::default(),
    )
    .expect("dead owner record");
    record.pid = exited_pid();
    record.process_start = "unavailable".to_string();
    assert!(is_lock_owner_proven_dead(&record));
    write_json(
        lock_path,
        &serde_json::to_value(&record).expect("lock record json"),
    );
}

fn sweep<'a>(
    paths: &'a MemoryIdentityPaths,
    now: &'a dyn Fn() -> i64,
    configure: impl FnOnce(&mut PruneKibitzerSidecarsOptions<'a>),
) -> PruneKibitzerSidecarsResult {
    let mut options = PruneKibitzerSidecarsOptions::new(&paths.recall, &paths.locks);
    options.now = Some(now);
    options.max_age_ms = Some(days_ms(7.0));
    configure(&mut options);
    prune_kibitzer_sidecars(&options)
}

#[test]
fn idle_sidecars_with_a_dead_owner_an_owned_one_and_a_live_locked_one_keep_only_the_live_two() {
    let id = identity();
    let paths = &id.paths;
    let dead = sidecar(paths, "dead-owner", days_ago(8.0));
    let owned = sidecar(paths, "owned", days_ago(8.0));
    let live = sidecar(paths, "live-owner", days_ago(8.0));
    std::fs::create_dir_all(&paths.locks).expect("locks dir");

    let dead_lock = owner_lock_path(&paths.locks, "dead-owner");
    write_dead_owner_lock(&dead_lock);
    let live_lock = owner_lock_path(&paths.locks, "live-owner");
    let holder = create_lock_record(
        KIBITZER_SIDECAR_OWNER_LOCK_PURPOSE,
        CreateLockRecordOptions::default(),
    )
    .expect("live record");
    acquire_lock(&live_lock, &holder, &AcquireLockOptions::default()).expect("hold the live lock");

    let owned_names = BTreeSet::from([encoded_session("owned")]);
    let dead_lock_free_at_delete = RefCell::new(Vec::new());
    let before_delete = |tombstone: &Path| {
        if !tombstone.to_string_lossy().contains("sidecars") {
            return;
        }
        let probe = create_lock_record(
            KIBITZER_SIDECAR_OWNER_LOCK_PURPOSE,
            CreateLockRecordOptions::default(),
        )
        .expect("probe record");
        let free = acquire_lock(&dead_lock, &probe, &AcquireLockOptions::default()).is_ok();
        if free {
            release_lock(&dead_lock, &probe).expect("release the probe");
        }
        dead_lock_free_at_delete.borrow_mut().push(free);
    };
    let now = || NOW;
    let result = sweep(paths, &now, |options| {
        options.owned = Some(&owned_names);
        options.before_tombstone_delete = Some(&before_delete);
    });

    assert_eq!(*dead_lock_free_at_delete.borrow(), vec![true]);
    assert_eq!(result.pruned, vec![encoded_session("dead-owner")]);
    assert_eq!(
        result.kept_reasons[&encoded_session("owned")].as_str(),
        "owned"
    );
    assert_eq!(
        result.kept_reasons[&encoded_session("live-owner")].as_str(),
        "locked"
    );
    assert!(!exists(&dead));
    assert!(exists(&owned));
    assert!(exists(&live));

    release_lock(&live_lock, &holder).expect("release the live lock");
}

#[test]
fn a_sidecar_whose_rename_to_a_tombstone_fails_is_kept_as_claim_failed() {
    let id = identity();
    let paths = &id.paths;
    let dir = sidecar(paths, "stale", days_ago(8.0));

    let refuse = |from: &Path, _to: &Path| {
        if from.ends_with(encoded_session("stale")) {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "EPERM: operation not permitted, rename",
            ))
        } else {
            Ok(())
        }
    };
    let now = || NOW;
    let result = sweep(paths, &now, |options| {
        options.before_claim_rename = Some(&refuse);
    });

    assert_eq!(
        result.kept_reasons[&encoded_session("stale")].as_str(),
        "claim-failed"
    );
    assert!(exists(&dir));
    assert!(tombstones(&kibitzer_sidecars_root(&paths.recall)).is_empty());
}

#[test]
fn a_sidecar_touched_inside_the_window_and_a_file_that_is_not_a_directory_are_left_alone() {
    let id = identity();
    let paths = &id.paths;
    let fresh = sidecar(paths, "fresh", days_ago(2.0));
    let root = kibitzer_sidecars_root(&paths.recall);
    std::fs::create_dir_all(&root).expect("sidecar root");
    std::fs::write(root.join("not-a-directory"), "x\n").expect("stray file");
    age_tree(&root.join("not-a-directory"), days_ago(30.0));

    let now = || NOW;
    let result = sweep(paths, &now, |_| {});

    assert!(result.pruned.is_empty());
    assert_eq!(
        result.kept_reasons[&encoded_session("fresh")].as_str(),
        "recent"
    );
    assert!(exists(&fresh));
    assert!(exists(&root.join("not-a-directory")));
}

#[test]
fn a_tombstone_left_by_a_crashed_sweep_is_deleted_without_a_lock() {
    let id = identity();
    let paths = &id.paths;
    let root = kibitzer_sidecars_root(&paths.recall);
    std::fs::create_dir_all(&root).expect("sidecar root");
    let tombstone = root.join(format!(
        "{KIBITZER_SIDECAR_TOMBSTONE_PREFIX}{}-abcd",
        encoded_session("gone")
    ));
    std::fs::create_dir_all(&tombstone).expect("tombstone dir");
    std::fs::write(tombstone.join("wakes.ndjson"), "{}\n").expect("wakes");

    let now = || NOW;
    let result = sweep(paths, &now, |_| {});

    assert!(result.pruned.is_empty());
    assert!(!exists(&tombstone));
    assert!(tombstones(&root).is_empty());
}
