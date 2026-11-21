use std::path::PathBuf;

use super::*;
use crate::identity::layout::MemoryIdentityPaths;
use crate::retention::guard::ReflectionRetentionGuard;
use crate::retention::policy::RuntimeRetentionPolicy;
use crate::retention::test_support::{NOW, age_tree, days_ago, identity, read_json, write_json};

fn pruned(paths: &MemoryIdentityPaths) -> LegacyRecallRunsOutcome {
    let now = || NOW;
    let context = RetentionContext::new(paths, RuntimeRetentionPolicy::default(), &now);
    prune_legacy_recall_runs(&context).expect("prune legacy recall runs")
}

fn sessions(
    paths: &MemoryIdentityPaths,
) -> (RetentionStoreResult, Option<ReflectionRetentionGuard>) {
    let now = || NOW;
    let context = RetentionContext::new(paths, RuntimeRetentionPolicy::default(), &now);
    let guard = LazyRetentionGuard::new(context);
    let result = prune_legacy_reflection_sessions(&context, &guard).expect("prune legacy sessions");
    (result, guard.value())
}

fn legacy_run(paths: &MemoryIdentityPaths, run_id: &str) -> PathBuf {
    let dir = paths.reflection_sessions.join(run_id);
    write_json(&dir.join("session.json"), &serde_json::json!({}));
    dir
}

#[test]
fn legacy_recall_runs_idle_past_retention_go_and_a_fresh_tree_stays() {
    let id = identity();
    let paths = &id.paths;
    let runs = paths.recall.join("runs");
    write_json(&runs.join("r1").join("out.json"), &serde_json::json!({}));
    age_tree(&runs, days_ago(8.0));

    assert_eq!(pruned(paths), LegacyRecallRunsOutcome::Removed);
    assert!(!exists(&runs));
    assert_eq!(pruned(paths), LegacyRecallRunsOutcome::Absent);
}

#[test]
fn a_fresh_legacy_recall_runs_tree_is_kept() {
    let id = identity();
    let paths = &id.paths;
    let runs = paths.recall.join("runs");
    write_json(&runs.join("r1").join("out.json"), &serde_json::json!({}));
    age_tree(&runs, days_ago(2.0));

    assert_eq!(pruned(paths), LegacyRecallRunsOutcome::Kept);
    assert!(exists(&runs));
}

#[test]
fn legacy_reflection_sessions_entries_past_retention_go_except_the_allocator_anchor() {
    let id = identity();
    let paths = &id.paths;
    let old = legacy_run(paths, "reflection-run-3");
    let anchor = legacy_run(paths, "reflection-run-9");
    age_tree(&paths.reflection_sessions, days_ago(8.0));

    let (report, guard) = sessions(paths);

    assert_eq!(report.removed, vec!["reflection-run-3".to_string()]);
    assert_eq!(report.kept["reflection-run-9"].as_str(), "anchor");
    assert!(!exists(&old));
    assert!(exists(&anchor));
    assert!(exists(&paths.reflection_sessions));
    assert_eq!(
        read_json(&paths.reflection.join("run-id.json"))["highWater"],
        9
    );
    assert_eq!(guard.expect("guard").high_water, 9);
}

#[cfg(unix)]
#[test]
fn a_legacy_reflection_session_holding_a_read_only_directory_is_removed_completely() {
    use std::os::unix::fs::PermissionsExt;

    let id = identity();
    let paths = &id.paths;
    let entry = paths.reflection_sessions.join("reflection-run-3");
    write_json(
        &entry.join("nested").join("payload.json"),
        &serde_json::json!({}),
    );
    std::fs::set_permissions(
        entry.join("nested").join("payload.json"),
        std::fs::Permissions::from_mode(0o400),
    )
    .expect("read-only payload");
    std::fs::set_permissions(entry.join("nested"), std::fs::Permissions::from_mode(0o500))
        .expect("read-only directory");
    legacy_run(paths, "reflection-run-9");
    age_tree(&paths.reflection_sessions, days_ago(8.0));

    let (report, _) = sessions(paths);

    assert_eq!(report.removed, vec!["reflection-run-3".to_string()]);
    assert!(!exists(&entry));
}

#[test]
fn a_legacy_reflection_session_younger_than_retention_is_kept() {
    let id = identity();
    let paths = &id.paths;
    let fresh = legacy_run(paths, "reflection-run-3");
    age_tree(&paths.reflection_sessions, days_ago(2.0));

    let (report, _) = sessions(paths);

    assert_eq!(report.kept["reflection-run-3"].as_str(), "recent");
    assert!(report.removed.is_empty());
    assert!(exists(&fresh));
}

#[test]
fn a_missing_legacy_runs_directory_reports_absent() {
    let id = identity();
    let paths = &id.paths;

    assert_eq!(pruned(paths), LegacyRecallRunsOutcome::Absent);
    assert_eq!(LegacyRecallRunsOutcome::Removed.as_str(), "removed");
    assert_eq!(LegacyRecallRunsOutcome::Kept.as_str(), "kept");
    assert_eq!(LegacyRecallRunsOutcome::Absent.as_str(), "absent");
}
