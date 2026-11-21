use std::collections::BTreeSet;
use std::path::Path;

use super::*;
use crate::identity::layout::MemoryIdentityPaths;
use crate::locks::{
    AcquireLockOptions, CreateLockRecordOptions, acquire_lock, create_lock_record,
    reflection_scheduler_lock_path, release_lock,
};
use crate::reflection::run_id_high_water::{
    read_reflection_run_id_mark, record_reflection_run_id_mark,
};
use crate::retention::pass::{RuntimeRetentionOptions, run_runtime_retention};
use crate::retention::policy::RuntimeRetentionPolicy;
use crate::retention::test_support::{
    CompletionShape, NOW, RunShape, age_tree, completion, completions_dir, days_ago, exists,
    identity, read_json, reflection_run, reservation, runs_dir, write_json,
};

fn guard(paths: &MemoryIdentityPaths) -> Option<ReflectionRetentionGuard> {
    let now = || NOW;
    let context = RetentionContext::new(paths, RuntimeRetentionPolicy::default(), &now);
    read_reflection_retention_guard(&context).expect("read the retention guard")
}

fn legacy_session(paths: &MemoryIdentityPaths, run_id: &str) {
    write_json(
        &paths.reflection_sessions.join(run_id).join("session.json"),
        &serde_json::json!({}),
    );
}

fn mint(paths: &MemoryIdentityPaths, run_id: &str, high_water: u64) {
    let holder = create_lock_record(
        "reflection-scheduler",
        CreateLockRecordOptions {
            run_id: Some(run_id.to_string()),
        },
    )
    .expect("mint record");
    let lock_path = reflection_scheduler_lock_path(&paths.locks);
    acquire_lock(&lock_path, &holder, &AcquireLockOptions::default())
        .expect("hold the scheduler lock");
    let recorded = record_reflection_run_id_mark(&paths.reflection, high_water);
    release_lock(&lock_path, &holder).expect("release the scheduler lock");
    recorded.expect("record the run-id mark");
}

#[test]
fn a_pass_that_recorded_high_water_forty_keeps_forty_one_recorded_by_a_mint() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(
        paths,
        "reflection-run-1",
        RunShape {
            finished_at: Some(days_ago(10.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-40",
        RunShape {
            finished_at: Some(days_ago(9.0)),
            ..RunShape::default()
        },
    );
    legacy_session(paths, "reflection-run-2");
    age_tree(&paths.reflection_sessions, days_ago(8.0));

    let mint_hook = |_path: &Path| mint(paths, "reflection-run-41", 41);
    let now = || NOW;
    let mut first_options = RuntimeRetentionOptions::new(RetentionContext::new(
        paths,
        RuntimeRetentionPolicy::default(),
        &now,
    ));
    first_options.context.before_tombstone_delete = Some(&mint_hook);
    let first = run_runtime_retention(&first_options).expect("first pass");

    assert_eq!(first.high_water, Some(40));
    assert_eq!(first.runs.removed, vec!["reflection-run-1".to_string()]);
    assert_eq!(
        first.reflection_sessions.removed,
        vec!["reflection-run-2".to_string()]
    );
    assert_eq!(
        read_json(&paths.reflection.join("run-id.json"))["highWater"],
        41
    );

    reflection_run(
        paths,
        "reflection-run-3",
        RunShape {
            finished_at: Some(days_ago(10.0)),
            ..RunShape::default()
        },
    );
    let second = run_runtime_retention(&RuntimeRetentionOptions::new(RetentionContext::new(
        paths,
        RuntimeRetentionPolicy::default(),
        &now,
    )))
    .expect("second pass");

    assert_eq!(second.high_water, Some(40));
    assert_eq!(second.runs.removed, vec!["reflection-run-3".to_string()]);
    assert_eq!(
        read_json(&paths.reflection.join("run-id.json"))["highWater"],
        41
    );
}

#[test]
fn a_mint_holding_the_scheduler_lock_keeps_every_reflection_store_as_scheduler_busy() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(
        paths,
        "reflection-run-1",
        RunShape {
            finished_at: Some(days_ago(10.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-5",
        RunShape {
            finished_at: Some(days_ago(9.0)),
            ..RunShape::default()
        },
    );
    for index in 100..151 {
        completion(
            paths,
            &format!("reflection-run-{index}"),
            CompletionShape {
                status: Some("consumed".to_string()),
                finished_at: Some(days_ago(31.0 + (150 - index) as f64)),
            },
        );
    }
    legacy_session(paths, "reflection-run-3");
    age_tree(&paths.reflection_sessions, days_ago(8.0));
    std::fs::create_dir_all(&paths.locks).expect("locks dir");

    let holder = create_lock_record(
        "reflection-scheduler",
        CreateLockRecordOptions {
            run_id: Some("reflection-run-151".to_string()),
        },
    )
    .expect("mint record");
    let lock_path = reflection_scheduler_lock_path(&paths.locks);
    acquire_lock(&lock_path, &holder, &AcquireLockOptions::default())
        .expect("hold the scheduler lock");

    let now = || NOW;
    let report = run_runtime_retention(&RuntimeRetentionOptions::new(RetentionContext::new(
        paths,
        RuntimeRetentionPolicy::default(),
        &now,
    )))
    .expect("pass with a held scheduler lock");

    assert_eq!(
        report.skipped,
        vec!["reflection-scheduler-busy".to_string()]
    );
    assert_eq!(report.high_water, None);
    assert_eq!(
        report.runs.kept["reflection-run-1"].as_str(),
        "scheduler-busy"
    );
    assert_eq!(
        report.completions.kept["reflection-run-100"].as_str(),
        "scheduler-busy"
    );
    assert_eq!(
        report.reflection_sessions.kept["reflection-run-3"].as_str(),
        "scheduler-busy"
    );
    assert!(report.runs.removed.is_empty());
    assert!(report.completions.removed.is_empty());
    assert!(report.reflection_sessions.removed.is_empty());
    assert!(exists(&runs_dir(paths).join("reflection-run-1")));
    assert!(exists(
        &completions_dir(paths).join("reflection-run-100.json")
    ));
    assert!(exists(&paths.reflection_sessions.join("reflection-run-3")));
    assert!(!exists(&paths.reflection.join("run-id.json")));

    release_lock(&lock_path, &holder).expect("release the scheduler lock");
}

#[test]
fn the_guard_reports_the_reserved_run_ids_and_conversations_of_both_reservation_files() {
    let id = identity();
    let paths = &id.paths;
    reservation(paths, "active.lock", "reflection-run-3");
    write_json(
        &paths.reflection.join("pending.json"),
        &serde_json::json!({
            "runId": "reflection-run-4",
            "reservedAt": "2026-11-04T00:00:00.000Z",
            "request": { "trigger": "manual", "conversationIds": ["X"] },
        }),
    );

    let read = guard(paths).expect("guard");

    assert_eq!(
        read.reserved,
        BTreeSet::from([
            "reflection-run-3".to_string(),
            "reflection-run-4".to_string()
        ])
    );
    assert_eq!(
        read.reserved_conversation_ids,
        BTreeSet::from(["X".to_string()])
    );
    assert_eq!(read.high_water, 4);
}

#[test]
fn a_lazy_guard_reads_once_and_reports_the_busy_scheduler_lock() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-2", RunShape::default());

    let now = || NOW;
    let context = RetentionContext::new(paths, RuntimeRetentionPolicy::default(), &now);
    let lazy = LazyRetentionGuard::new(context);
    assert_eq!(lazy.value(), None);
    let first = lazy.get().expect("first read").expect("guard");
    assert_eq!(first.high_water, 2);
    assert_eq!(lazy.value(), Some(first));
    assert!(!lazy.busy());

    std::fs::create_dir_all(&paths.locks).expect("locks dir");
    let holder = create_lock_record(
        "reflection-scheduler",
        CreateLockRecordOptions {
            run_id: Some("reflection-run-9".to_string()),
        },
    )
    .expect("mint record");
    let lock_path = reflection_scheduler_lock_path(&paths.locks);
    acquire_lock(&lock_path, &holder, &AcquireLockOptions::default()).expect("hold the lock");
    let busy_context = RetentionContext::new(paths, RuntimeRetentionPolicy::default(), &now);
    let busy = LazyRetentionGuard::new(busy_context);
    assert!(busy.get().expect("busy read").is_none());
    assert!(busy.busy());
    assert_eq!(busy.value(), None);
    release_lock(&lock_path, &holder).expect("release the lock");
}

#[test]
fn the_guard_records_the_run_id_mark_of_the_highest_run_on_disk() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-7", RunShape::default());
    assert_eq!(read_reflection_run_id_mark(&paths.reflection), None);

    let read = guard(paths).expect("guard");

    assert_eq!(read.high_water, 7);
    assert_eq!(read_reflection_run_id_mark(&paths.reflection), Some(7));
}
