use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::support::time::{format_rfc3339_millis, parse_rfc3339};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    parse_rfc3339("2026-11-05T00:00:00.000Z").unwrap()
}

fn days_ago(days: i64) -> i64 {
    now_ms() - days * DAY_MS
}

fn iso(ms: i64) -> String {
    format_rfc3339_millis(ms)
}

fn runtime_dir(root: &Path) -> PathBuf {
    let runtime = root.join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    runtime
}

fn log_lines(runtime: &Path) -> Vec<Value> {
    std::fs::read_to_string(runtime.join(RETENTION_LOG_FILE))
        .unwrap()
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn removed(store: &str, names: &[&str]) -> Vec<(String, Vec<String>)> {
    vec![(
        store.to_string(),
        names.iter().map(|name| (*name).to_string()).collect(),
    )]
}

#[test]
fn a_log_past_its_size_limit_rotates_before_the_next_entry() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_dir(root.path());
    let old = format!(
        "{}\n",
        json!({ "v": 1, "at": iso(days_ago(3)), "total": 1, "removed": { "runs": ["reflection-run-0"] } })
    );
    std::fs::write(runtime.join(RETENTION_LOG_FILE), old.repeat(40)).unwrap();

    append_retention_log(
        &runtime,
        &RetentionLogInput {
            at: now_ms(),
            removed: removed("journals", &["s-1"]),
            cap_evicted_bytes: None,
        },
        &RetentionLogLimits {
            max_bytes: Some(1024),
            max_names_per_store: None,
        },
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(runtime.join(RETENTION_LOG_PREVIOUS_FILE)).unwrap(),
        old.repeat(40)
    );
    assert_eq!(
        log_lines(&runtime),
        vec![
            json!({ "v": 1, "at": iso(now_ms()), "total": 1, "removed": { "journals": ["s-1"] } })
        ]
    );
}

#[test]
fn an_entry_lists_at_most_the_cap_per_store_and_counts_the_rest() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_dir(root.path());
    let runs: Vec<String> = (1..=7)
        .map(|index| format!("reflection-run-{index}"))
        .collect();

    append_retention_log(
        &runtime,
        &RetentionLogInput {
            at: now_ms(),
            removed: vec![("runs".to_string(), runs.clone())],
            cap_evicted_bytes: None,
        },
        &RetentionLogLimits {
            max_bytes: None,
            max_names_per_store: Some(5),
        },
    )
    .unwrap();

    assert_eq!(
        log_lines(&runtime),
        vec![json!({
            "v": 1,
            "at": iso(now_ms()),
            "total": 7,
            "removed": { "runs": runs[..5] },
            "more": { "runs": 2 },
        })]
    );
}

#[test]
fn the_newest_entry_of_a_huge_cleanup_fits_the_readers_tail() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_dir(root.path());
    append_retention_log(
        &runtime,
        &RetentionLogInput {
            at: days_ago(1),
            removed: removed("runs", &["reflection-run-1"]),
            cap_evicted_bytes: None,
        },
        &RetentionLogLimits::default(),
    )
    .unwrap();
    let stores = [
        "runs",
        "journals",
        "sidecars",
        "cap:runs",
        "cap:journals",
        "cap:sidecars",
    ];
    let removed: Vec<(String, Vec<String>)> = stores
        .iter()
        .map(|store| {
            let names = (0..200)
                .map(|index| format!("{store}-{}-{index:03}", "x".repeat(56)))
                .collect();
            ((*store).to_string(), names)
        })
        .collect();

    append_retention_log(
        &runtime,
        &RetentionLogInput {
            at: now_ms(),
            removed,
            cap_evicted_bytes: None,
        },
        &RetentionLogLimits::default(),
    )
    .unwrap();

    assert_eq!(
        read_last_retention_log_entry(&runtime).unwrap(),
        Some(RetentionLogEntry {
            at: iso(now_ms()),
            total: 1200,
        })
    );
    let log = std::fs::read_to_string(runtime.join(RETENTION_LOG_FILE)).unwrap();
    let last = log.trim_end().split('\n').next_back().unwrap_or_default();
    assert!(last.len() <= 32 * 1024, "last line is {} bytes", last.len());
}

#[test]
fn the_last_entry_is_the_newest_whole_one_across_generations() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_dir(root.path());
    assert_eq!(read_last_retention_log_entry(&runtime).unwrap(), None);

    std::fs::write(
        runtime.join(RETENTION_LOG_PREVIOUS_FILE),
        format!(
            "{}\n",
            json!({ "v": 1, "at": iso(days_ago(9)), "total": 4, "removed": {} })
        ),
    )
    .unwrap();
    assert_eq!(
        read_last_retention_log_entry(&runtime).unwrap(),
        Some(RetentionLogEntry {
            at: iso(days_ago(9)),
            total: 4,
        })
    );

    let first = json!({ "v": 1, "at": iso(days_ago(2)), "total": 3, "removed": {} });
    std::fs::write(
        runtime.join(RETENTION_LOG_FILE),
        format!("{first}\n{{\"v\":1,\"at\":\"2026-11-0"),
    )
    .unwrap();
    assert_eq!(
        read_last_retention_log_entry(&runtime).unwrap(),
        Some(RetentionLogEntry {
            at: iso(days_ago(2)),
            total: 3,
        })
    );
}

#[test]
fn a_pass_that_removed_nothing_writes_no_log() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_dir(root.path());

    append_retention_log(
        &runtime,
        &RetentionLogInput {
            at: now_ms(),
            removed: vec![("runs".to_string(), Vec::new())],
            cap_evicted_bytes: None,
        },
        &RetentionLogLimits::default(),
    )
    .unwrap();

    assert!(!runtime.join(RETENTION_LOG_FILE).exists());
}

// The two log cases that run a full retention pass, on the shared retention fixtures.
mod pass_driven {
    use serde_json::json;

    use super::log_lines;
    use crate::identity::layout::MemoryIdentityPaths;
    use crate::retention::log::RETENTION_LOG_FILE;
    use crate::retention::pass::{RuntimeRetentionOptions, run_runtime_retention};
    use crate::retention::policy::{DEFAULT_RUNTIME_RETENTION, RetentionContext};
    use crate::retention::test_support::{
        NOW, RunShape, days_ago, identity, iso, now, reflection_run, runs_dir,
    };

    fn run_retention(paths: &MemoryIdentityPaths) {
        run_runtime_retention(&RuntimeRetentionOptions::new(RetentionContext::new(
            paths,
            DEFAULT_RUNTIME_RETENTION,
            &now,
        )))
        .unwrap();
    }

    fn run_finished(days: f64) -> RunShape {
        RunShape {
            finished_at: Some(days_ago(days)),
            ..RunShape::default()
        }
    }

    #[test]
    fn a_folded_run_past_the_window_is_named_by_one_log_line_with_the_pass_time_and_the_total() {
        let fixture = identity();
        let paths = &fixture.paths;
        reflection_run(paths, "reflection-run-1", run_finished(8.0));
        reflection_run(paths, "reflection-run-9", run_finished(1.0));

        run_retention(paths);

        assert!(!runs_dir(paths).join("reflection-run-1").exists());
        let lines = log_lines(&paths.runtime);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["v"], json!(1));
        assert_eq!(lines[0]["at"], json!(iso(NOW)));
        assert_eq!(lines[0]["total"], json!(1));
        assert_eq!(lines[0]["removed"]["runs"], json!(["reflection-run-1"]));
    }

    #[test]
    fn a_pass_with_nothing_past_the_window_writes_no_log_file() {
        let fixture = identity();
        let paths = &fixture.paths;
        reflection_run(paths, "reflection-run-9", run_finished(1.0));

        run_retention(paths);

        assert!(!paths.runtime.join(RETENTION_LOG_FILE).exists());
    }
}
