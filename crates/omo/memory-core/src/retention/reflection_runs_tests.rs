use super::*;

use crate::identity::layout::MemoryIdentityPaths;
use crate::retention::budget::PassBudget;
use crate::retention::guard::ReflectionRetentionGuard;
use crate::retention::pass::{RuntimeRetentionOptions, run_runtime_retention};
use crate::retention::policy::RuntimeRetentionPolicy;
use crate::retention::test_support::{
    CompletionShape, NOW, RunShape, Sentinel, age_tree, completion, completions_dir, days_ago,
    identity, read_only_payload, reflection_run, reservation, runs_dir, tombstones, write_json,
};

fn prune_with(
    paths: &MemoryIdentityPaths,
    policy: RuntimeRetentionPolicy,
    now: &dyn Fn() -> i64,
) -> (RetentionStoreResult, Option<ReflectionRetentionGuard>) {
    let context = RetentionContext::new(paths, policy, now);
    let guard = LazyRetentionGuard::new(context);
    let result = prune_reflection_runs(&context, &guard).expect("prune reflection runs");
    (result, guard.value())
}

fn prune_now(
    paths: &MemoryIdentityPaths,
    policy: RuntimeRetentionPolicy,
) -> (RetentionStoreResult, Option<ReflectionRetentionGuard>) {
    let now = || NOW;
    prune_with(paths, policy, &now)
}
#[test]
fn a_folded_run_idle_past_retention_is_removed_and_its_completion_kept() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-5", RunShape::default());
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            outcome: Some("no_changes".to_string()),
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );
    completion(
        paths,
        "reflection-run-5",
        CompletionShape {
            status: Some("consumed".to_string()),
            finished_at: Some(days_ago(8.0)),
        },
    );

    let (report, _) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.removed, vec!["reflection-run-5".to_string()]);
    assert!(!exists(&runs_dir(paths).join("reflection-run-5")));
    assert!(tombstones(&runs_dir(paths)).is_empty());
    assert!(exists(
        &completions_dir(paths).join("reflection-run-5.json")
    ));
}

#[test]
fn a_folded_run_whose_file_was_touched_recently_is_kept_as_recent() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-4", RunShape::default());
    let dir = reflection_run(paths, "reflection-run-5", RunShape::default());
    age_tree(&dir.join("child-1.log"), days_ago(2.0));
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );

    let (report, _) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.removed, vec!["reflection-run-4".to_string()]);
    assert_eq!(report.kept["reflection-run-5"].as_str(), "recent");
    assert!(exists(&dir));
}

#[test]
fn a_folded_run_whose_final_json_names_another_run_is_kept_as_malformed() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-4", RunShape::default());
    let dir = reflection_run(
        paths,
        "reflection-run-5",
        RunShape {
            final_run_id: Some("reflection-run-4".to_string()),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );

    let (report, _) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.removed, vec!["reflection-run-4".to_string()]);
    assert_eq!(report.kept["reflection-run-5"].as_str(), "malformed");
    assert!(exists(&dir));
}

#[test]
fn failed_abandoned_and_quarantined_runs_sixty_days_old_are_kept_as_unfolded_terminal() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-4", RunShape::default());
    reflection_run(
        paths,
        "reflection-run-1",
        RunShape {
            outcome: Some("failed".to_string()),
            finished_at: Some(days_ago(60.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-2",
        RunShape {
            sentinel: Some(Sentinel::Abandoned),
            finished_at: Some(days_ago(60.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-3",
        RunShape {
            sentinel: Some(Sentinel::Quarantined),
            finished_at: Some(days_ago(60.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );

    let (report, _) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.removed, vec!["reflection-run-4".to_string()]);
    for run_id in ["reflection-run-1", "reflection-run-2", "reflection-run-3"] {
        assert_eq!(report.kept[run_id].as_str(), "unfolded-terminal");
        assert!(exists(&runs_dir(paths).join(run_id)));
    }
}

#[test]
fn a_run_dir_with_a_ledger_and_no_sentinel_is_kept_for_the_reaper() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-4", RunShape::default());
    reflection_run(
        paths,
        "reflection-run-1",
        RunShape {
            sentinel: Some(Sentinel::None),
            finished_at: Some(days_ago(60.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );

    let (report, _) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.removed, vec!["reflection-run-4".to_string()]);
    assert_eq!(report.kept["reflection-run-1"].as_str(), "no-terminal");
    assert!(exists(&runs_dir(paths).join("reflection-run-1")));
}

#[test]
fn folded_runs_that_are_the_pending_and_the_active_reservation_are_kept_as_reserved() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-2", RunShape::default());
    reflection_run(paths, "reflection-run-3", RunShape::default());
    reflection_run(paths, "reflection-run-4", RunShape::default());
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );
    reservation(paths, "active.lock", "reflection-run-3");
    reservation(paths, "pending.json", "reflection-run-4");

    let (report, _) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.removed, vec!["reflection-run-2".to_string()]);
    assert_eq!(report.kept["reflection-run-3"].as_str(), "reserved");
    assert_eq!(report.kept["reflection-run-4"].as_str(), "reserved");
}

#[test]
fn the_highest_numbered_and_the_newest_finished_run_are_both_kept_as_anchors() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(
        paths,
        "reflection-run-12",
        RunShape {
            finished_at: Some(days_ago(20.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-7",
        RunShape {
            finished_at: Some(days_ago(9.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-3",
        RunShape {
            finished_at: Some(days_ago(30.0)),
            ..RunShape::default()
        },
    );

    let (report, guard) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.kept["reflection-run-12"].as_str(), "anchor");
    assert_eq!(report.kept["reflection-run-7"].as_str(), "anchor");
    assert_eq!(report.removed, vec!["reflection-run-3".to_string()]);
    assert_eq!(guard.expect("guard").high_water, 12);
}

#[test]
fn a_folded_run_whose_finalize_lock_is_held_live_is_kept_as_locked_without_waiting() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(paths, "reflection-run-4", RunShape::default());
    reflection_run(paths, "reflection-run-5", RunShape::default());
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );
    std::fs::create_dir_all(&paths.locks).expect("locks dir");
    let lock_path =
        run_finalization_lock_path(&paths.locks, "reflection-run-5").expect("lock path");
    let holder = create_lock_record(
        "reflection-finalize",
        CreateLockRecordOptions {
            run_id: Some("reflection-run-5".to_string()),
        },
    )
    .expect("lock record");
    acquire_lock(&lock_path, &holder, &AcquireLockOptions::default()).expect("hold the lock");

    let (report, _) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.removed, vec!["reflection-run-4".to_string()]);
    assert_eq!(report.kept["reflection-run-5"].as_str(), "locked");
    assert!(exists(&runs_dir(paths).join("reflection-run-5")));

    release_lock(&lock_path, &holder).expect("release the lock");
}

#[test]
fn a_tombstone_left_by_a_crashed_pass_is_removed_by_the_next_pass() {
    let id = identity();
    let paths = &id.paths;
    let tombstone = runs_dir(paths).join(".prune-reflection-run-2-0f0f");
    std::fs::create_dir_all(&tombstone).expect("tombstone dir");
    write_json(
        &tombstone.join("final.json"),
        &serde_json::json!({ "runId": "reflection-run-2", "outcome": "merged" }),
    );
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );

    let aborted = || true;
    let now = || NOW;
    let mut cancelled = RuntimeRetentionOptions::new(RetentionContext::new(
        paths,
        RuntimeRetentionPolicy::default(),
        &now,
    ));
    cancelled.context.cancel = Some(&aborted);
    assert!(matches!(
        run_runtime_retention(&cancelled),
        Err(RetentionError::Aborted)
    ));
    assert!(exists(&tombstone));

    prune_now(paths, RuntimeRetentionPolicy::default());
    assert!(!exists(&tombstone));
}

#[test]
fn four_crash_tombstones_and_a_spent_budget_admit_tombstone_removal_like_any_removal() {
    use std::cell::Cell;

    let id = identity();
    let paths = &id.paths;
    for suffix in ["a1", "b2", "c3", "d4"] {
        let tombstone = runs_dir(paths).join(format!(".prune-reflection-run-2-{suffix}"));
        std::fs::create_dir_all(&tombstone).expect("tombstone dir");
        write_json(
            &tombstone.join("final.json"),
            &serde_json::json!({ "runId": "reflection-run-2", "outcome": "merged" }),
        );
    }
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );

    let clock = Cell::new(NOW);
    let now = || {
        clock.set(clock.get() + 600);
        clock.get()
    };
    let policy = RuntimeRetentionPolicy {
        pass_budget_ms: 1_000,
        ..RuntimeRetentionPolicy::default()
    };
    let budget = PassBudget::new(&now, policy.pass_budget_ms, None, None);
    let base = RetentionContext::new(paths, policy, &now);
    let context = base.with_budget(&budget);
    let guard = LazyRetentionGuard::new(context);
    prune_reflection_runs(&context, &guard).expect("bounded prune");

    let left = tombstones(&runs_dir(paths)).len();
    assert!(left > 0, "the budget must stop before every tombstone");
    assert!(left < 4, "the budget must admit at least one tombstone");
}

#[test]
fn read_only_payload_files_in_a_folded_run_and_in_a_crash_tombstone_are_removed_completely() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(
        paths,
        "reflection-run-5",
        RunShape {
            read_only_payload: true,
            ..RunShape::default()
        },
    );
    let tombstone = runs_dir(paths).join(".prune-reflection-run-4-abcd");
    std::fs::create_dir_all(&tombstone).expect("tombstone dir");
    read_only_payload(&tombstone);
    reflection_run(
        paths,
        "reflection-run-9",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );

    let (report, _) = prune_now(paths, RuntimeRetentionPolicy::default());

    assert_eq!(report.removed, vec!["reflection-run-5".to_string()]);
    assert!(!exists(&runs_dir(paths).join("reflection-run-5")));
    assert!(!exists(&tombstone));
    assert!(tombstones(&runs_dir(paths)).is_empty());
}
