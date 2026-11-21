use super::*;
use std::cell::RefCell;
use tempfile::tempdir;

fn owner(pid: u32, host: &str) -> LockRecord {
    let mut record =
        create_lock_record("dead-owner-test", CreateLockRecordOptions { run_id: None }).unwrap();
    record.pid = pid;
    record.hostname = host.to_string();
    record
}

fn write_lock(dir: &Path, name: &str, record: &LockRecord) {
    std::fs::write(
        dir.join(name),
        format!("{}\n", serde_json::to_string(record).unwrap()),
    )
    .unwrap();
}

const GONE_PID: u32 = 2_000_000_000;

#[test]
fn a_lock_whose_owner_is_gone_on_this_host_is_reclaimed_and_released() {
    let dir = tempdir().unwrap();
    write_lock(dir.path(), "gone.lock", &owner(GONE_PID, &hostname()));
    let reclaimed = RefCell::new(Vec::new());
    let failures = RefCell::new(Vec::new());

    let count = sweep_dead_owner_locks(
        dir.path(),
        |path, message| {
            failures
                .borrow_mut()
                .push((path.to_path_buf(), message.to_string()))
        },
        |path, previous| {
            reclaimed
                .borrow_mut()
                .push((path.to_path_buf(), previous.pid))
        },
    );

    assert_eq!(count, 1);
    assert!(!dir.path().join("gone.lock").exists());
    assert_eq!(
        reclaimed.into_inner(),
        vec![(dir.path().join("gone.lock"), GONE_PID)]
    );
    assert!(failures.into_inner().is_empty());
}

#[test]
fn a_lock_held_by_a_live_process_is_kept() {
    let dir = tempdir().unwrap();
    write_lock(
        dir.path(),
        "live.lock",
        &owner(std::process::id(), &hostname()),
    );

    let count = sweep_dead_owner_locks(dir.path(), |_, _| {}, |_, _| {});

    assert_eq!(count, 0);
    assert!(dir.path().join("live.lock").exists());
}

#[test]
fn a_lock_owned_by_another_host_cannot_be_judged_and_is_kept() {
    let dir = tempdir().unwrap();
    write_lock(
        dir.path(),
        "remote.lock",
        &owner(GONE_PID, "another-host.invalid"),
    );

    let count = sweep_dead_owner_locks(dir.path(), |_, _| {}, |_, _| {});

    assert_eq!(count, 0);
    assert!(dir.path().join("remote.lock").exists());
}

#[test]
fn an_unparseable_lock_and_a_non_lock_file_are_left_alone() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("torn.lock"), "{\"pid\":").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "keep").unwrap();

    let count = sweep_dead_owner_locks(dir.path(), |_, _| {}, |_, _| {});

    assert_eq!(count, 0);
    assert!(dir.path().join("torn.lock").exists());
    assert!(dir.path().join("notes.txt").exists());
}

#[test]
fn a_missing_directory_sweeps_nothing_and_reports_no_failure() {
    let dir = tempdir().unwrap();
    let failures = RefCell::new(0);

    let count = sweep_dead_owner_locks(
        &dir.path().join("absent"),
        |_, _| *failures.borrow_mut() += 1,
        |_, _| {},
    );

    assert_eq!(count, 0);
    assert_eq!(failures.into_inner(), 0);
}

#[test]
fn one_reclaimed_lock_does_not_stop_the_sweep_of_the_others() {
    let dir = tempdir().unwrap();
    write_lock(dir.path(), "a.lock", &owner(GONE_PID, &hostname()));
    write_lock(dir.path(), "b.lock", &owner(GONE_PID, &hostname()));

    let count = sweep_dead_owner_locks(dir.path(), |_, _| {}, |_, _| {});

    assert_eq!(count, 2);
}
