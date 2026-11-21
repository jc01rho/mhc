use super::*;
use crate::identity::layout::MemoryIdentityPaths;
use crate::retention::policy::RuntimeRetentionPolicy;
use crate::retention::test_support::{
    CompletionShape, NOW, RunShape, completion, completions_dir, days_ago, identity, read_json,
    reflection_run, run_id_mark,
};

fn prune_now(paths: &MemoryIdentityPaths) -> RetentionStoreResult {
    let now = || NOW;
    let context = RetentionContext::new(paths, RuntimeRetentionPolicy::default(), &now);
    let guard = LazyRetentionGuard::new(context);
    prune_completions(&context, &guard).expect("prune completions")
}

#[test]
fn consumed_pending_and_run_backed_completions_keep_only_the_consumed_runless_one() {
    let id = identity();
    let paths = &id.paths;
    run_id_mark(paths, 200);
    completion(
        paths,
        "reflection-run-1",
        CompletionShape {
            status: Some("consumed".to_string()),
            finished_at: Some(days_ago(31.0)),
        },
    );
    completion(
        paths,
        "reflection-run-2",
        CompletionShape {
            status: Some("pending".to_string()),
            finished_at: Some(days_ago(90.0)),
        },
    );
    completion(
        paths,
        "reflection-run-3",
        CompletionShape {
            status: Some("consumed".to_string()),
            finished_at: Some(days_ago(31.0)),
        },
    );
    reflection_run(
        paths,
        "reflection-run-3",
        RunShape {
            outcome: Some("failed".to_string()),
            finished_at: Some(days_ago(31.0)),
            ..RunShape::default()
        },
    );
    for index in 100..150 {
        completion(
            paths,
            &format!("reflection-run-{index}"),
            CompletionShape {
                status: Some("consumed".to_string()),
                finished_at: Some(days_ago(1.0)),
            },
        );
    }

    let report = prune_now(paths);

    assert_eq!(report.removed, vec!["reflection-run-1".to_string()]);
    assert_eq!(report.kept["reflection-run-2"].as_str(), "pending-delivery");
    assert_eq!(report.kept["reflection-run-3"].as_str(), "run-present");
    assert!(!exists(
        &completions_dir(paths).join("reflection-run-1.json")
    ));
}

#[test]
fn an_old_consumed_completion_without_a_mark_makes_the_pass_record_the_mark_first() {
    let id = identity();
    let paths = &id.paths;
    completion(
        paths,
        "reflection-run-1",
        CompletionShape {
            status: Some("consumed".to_string()),
            finished_at: Some(days_ago(31.0)),
        },
    );
    completion(
        paths,
        "reflection-run-60",
        CompletionShape {
            status: Some("consumed".to_string()),
            finished_at: Some(days_ago(31.0)),
        },
    );
    for index in 100..150 {
        completion(
            paths,
            &format!("reflection-run-{index}"),
            CompletionShape {
                status: Some("consumed".to_string()),
                finished_at: Some(days_ago(1.0)),
            },
        );
    }

    let report = prune_now(paths);

    let mark = read_json(&paths.reflection.join("run-id.json"));
    assert_eq!(mark["highWater"], 149);
    assert_eq!(
        report.removed,
        vec![
            "reflection-run-1".to_string(),
            "reflection-run-60".to_string()
        ]
    );
}

#[test]
fn a_completion_outside_the_newest_fifty_window_is_kept_as_newest_only_inside_it() {
    let id = identity();
    let paths = &id.paths;
    run_id_mark(paths, 400);
    for index in 100..151 {
        completion(
            paths,
            &format!("reflection-run-{index}"),
            CompletionShape {
                status: Some("consumed".to_string()),
                finished_at: Some(days_ago(1.0)),
            },
        );
    }

    let report = prune_now(paths);

    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert_eq!(report.kept["reflection-run-100"].as_str(), "recent");
    assert_eq!(report.kept["reflection-run-150"].as_str(), "newest");
    assert_eq!(report.kept.len(), 51);
}
