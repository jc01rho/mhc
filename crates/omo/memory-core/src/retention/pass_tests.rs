use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::*;

use crate::identity::layout::MemoryIdentityPaths;
use crate::retention::budget::RetentionCursor;
use crate::retention::policy::{RetentionError, RuntimeRetentionPolicy};
use crate::retention::reflection_health_signal::ReflectionTerminalKind;
use crate::retention::reflection_runs::RETENTION_TOMBSTONE_PREFIX;
use crate::retention::test_support::{
    JournalOptions, JournalShape, NOW, RunShape, Sentinel, age_tree, completions_dir, days_ago,
    exists, identity, iso, journal, read_json, read_names, reflection_run, runs_dir, tombstones,
    write_json,
};

fn unreflected_policy() -> RuntimeRetentionPolicy {
    RuntimeRetentionPolicy {
        discard_unreflected_journals: true,
        ..RuntimeRetentionPolicy::default()
    }
}

fn pass(paths: &MemoryIdentityPaths, policy: RuntimeRetentionPolicy) -> RuntimeRetentionReport {
    let now = || NOW;
    run_runtime_retention(&RuntimeRetentionOptions::new(RetentionContext::new(
        paths, policy, &now,
    )))
    .expect("retention pass")
}

fn mtime_ms_of(path: &Path) -> i64 {
    std::fs::metadata(path)
        .expect("metadata")
        .modified()
        .expect("modification time")
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_millis() as i64
}

fn folded_runs(paths: &MemoryIdentityPaths, count: u32, finished_at: i64) {
    for index in 1..=count {
        reflection_run(
            paths,
            &format!("reflection-run-{index}"),
            RunShape {
                finished_at: Some(finished_at),
                ..RunShape::default()
            },
        );
    }
}

fn unfolded_runs(paths: &MemoryIdentityPaths, count: u32) {
    for index in 1..=count {
        reflection_run(
            paths,
            &format!("reflection-run-{index}"),
            RunShape {
                outcome: Some("failed".to_string()),
                finished_at: Some(days_ago(60.0)),
                ..RunShape::default()
            },
        );
    }
}

fn run_ids_left(paths: &MemoryIdentityPaths) -> Vec<String> {
    let mut left: Vec<String> = read_names(&runs_dir(paths))
        .into_iter()
        .filter(|name| !name.starts_with(RETENTION_TOMBSTONE_PREFIX))
        .collect();
    left.sort();
    left
}

#[test]
fn thirty_folded_runs_and_a_budget_that_admits_twelve_persist_a_cursor_the_next_pass_resumes_and_deletes()
 {
    let id = identity();
    let paths = &id.paths;
    folded_runs(paths, 30, days_ago(10.0));
    reflection_run(
        paths,
        "reflection-run-99",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );
    let clock = Cell::new(NOW);
    let now = || clock.get();
    let tick = |_: &Path| clock.set(clock.get() + 5_000);
    let policy = RuntimeRetentionPolicy {
        pass_budget_ms: 60_000,
        ..unreflected_policy()
    };
    let mut options = RuntimeRetentionOptions::new(RetentionContext::new(paths, policy, &now));
    options.context.before_tombstone_delete = Some(&tick);

    let first = run_runtime_retention(&options).expect("first pass");

    assert_eq!(first.runs.removed.len(), 12);
    let last_name = first.runs.removed.last().expect("a removed run").clone();
    assert_eq!(
        first.cursor,
        Some(ReportCursor::Store(RetentionCursor {
            store: "runs".to_string(),
            last_name: Some(last_name.clone()),
        }))
    );
    let stored = read_json(&paths.runtime.join("retention.json"));
    assert_eq!(stored["version"], 1);
    assert_eq!(stored["cursor"]["store"], "runs");
    assert_eq!(stored["cursor"]["lastName"], last_name);

    let second = pass(paths, unreflected_policy());

    assert_eq!(second.runs.removed.len(), 18);
    assert_eq!(second.cursor, None);
    assert!(!exists(&paths.runtime.join("retention.json")));
    assert_eq!(run_ids_left(paths), vec!["reflection-run-99".to_string()]);
}

#[test]
fn an_abort_between_items_persists_the_cursor_and_the_pass_rejects() {
    let id = identity();
    let paths = &id.paths;
    folded_runs(paths, 5, days_ago(10.0));
    reflection_run(
        paths,
        "reflection-run-99",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );
    let aborted = Cell::new(false);
    let removed = Cell::new(0_u32);
    let cancel = || aborted.get();
    let hook = |_: &Path| {
        removed.set(removed.get() + 1);
        if removed.get() == 2 {
            aborted.set(true);
        }
    };
    let now = || NOW;
    let mut options =
        RuntimeRetentionOptions::new(RetentionContext::new(paths, unreflected_policy(), &now));
    options.context.cancel = Some(&cancel);
    options.context.before_tombstone_delete = Some(&hook);

    let result = run_runtime_retention(&options);

    assert!(matches!(result, Err(RetentionError::Aborted)));
    let persisted = read_json(&paths.runtime.join("retention.json"));
    assert_eq!(persisted["cursor"]["store"], "runs");
    let last_name = persisted["cursor"]["lastName"]
        .as_str()
        .expect("a cursor name")
        .to_string();
    assert!(!exists(&runs_dir(paths).join(&last_name)));
    assert_eq!(run_ids_left(paths).len(), 4);
}

#[test]
fn a_no_op_visit_records_the_visit_takes_no_lock_and_changes_no_runtime_mtime() {
    let id = identity();
    let paths = &id.paths;
    reflection_run(
        paths,
        "reflection-run-1",
        RunShape {
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );
    journal(
        paths,
        "fresh",
        JournalShape::Fully,
        days_ago(1.0),
        JournalOptions::default(),
    );
    std::fs::create_dir_all(&paths.locks).expect("locks dir");
    age_tree(&paths.root, days_ago(8.0));
    let transcript = paths.transcripts.join("fresh");
    let before = (
        mtime_ms_of(&paths.runtime),
        mtime_ms_of(&paths.locks),
        mtime_ms_of(&paths.reflection),
        mtime_ms_of(&transcript),
    );

    let report = pass(paths, unreflected_policy());

    assert_eq!(report.runs.kept["reflection-run-1"].as_str(), "anchor");
    assert_eq!(report.journals.kept["fresh"].as_str(), "recent");
    assert_eq!(report.high_water, None);
    assert!(report.skipped.is_empty());
    assert!(read_names(&paths.locks).is_empty());
    assert!(!exists(&paths.reflection.join("run-id.json")));
    assert_eq!(
        (
            mtime_ms_of(&paths.runtime),
            mtime_ms_of(&paths.locks),
            mtime_ms_of(&paths.reflection),
            mtime_ms_of(&transcript),
        ),
        before
    );
}

#[test]
fn more_unfolded_runs_than_the_budget_covers_are_all_classified_without_a_cursor() {
    let id = identity();
    let paths = &id.paths;
    unfolded_runs(paths, 10);
    let clock = Cell::new(NOW);
    let now = || {
        clock.set(clock.get() + 400);
        clock.get()
    };
    let policy = RuntimeRetentionPolicy {
        pass_budget_ms: 1_000,
        ..RuntimeRetentionPolicy::default()
    };

    let report = run_runtime_retention(&RuntimeRetentionOptions::new(RetentionContext::new(
        paths, policy, &now,
    )))
    .expect("pass");

    assert_eq!(report.runs.kept.len(), 10);
    assert_eq!(report.cursor, None);
    assert!(!exists(&paths.runtime.join("retention.json")));
}

#[test]
fn more_unreadable_completion_records_than_the_budget_covers_are_all_classified_in_one_pass() {
    let id = identity();
    let paths = &id.paths;
    for index in 1..=10 {
        write_json(
            &completions_dir(paths).join(format!("reflection-run-{index}.json")),
            &serde_json::json!({}),
        );
    }
    let clock = Cell::new(NOW);
    let now = || {
        clock.set(clock.get() + 400);
        clock.get()
    };
    let policy = RuntimeRetentionPolicy {
        pass_budget_ms: 1_000,
        ..RuntimeRetentionPolicy::default()
    };

    let report = run_runtime_retention(&RuntimeRetentionOptions::new(RetentionContext::new(
        paths, policy, &now,
    )))
    .expect("pass");

    assert_eq!(report.completions.kept.len(), 10);
    assert_eq!(report.cursor, None);
}

#[test]
fn an_abort_while_the_runs_are_classified_keeps_the_journals_resume_point() {
    let id = identity();
    let paths = &id.paths;
    unfolded_runs(paths, 10);
    write_json(
        &paths.runtime.join("retention.json"),
        &serde_json::json!({ "version": 1, "cursor": { "store": "journals", "lastName": "s-05" } }),
    );
    let checks = Cell::new(0_u32);
    let cancel = || {
        checks.set(checks.get() + 1);
        checks.get() >= 4
    };
    let now = || NOW;
    let mut options = RuntimeRetentionOptions::new(RetentionContext::new(
        paths,
        RuntimeRetentionPolicy::default(),
        &now,
    ));
    options.context.cancel = Some(&cancel);

    let result = run_runtime_retention(&options);

    assert!(matches!(result, Err(RetentionError::Aborted)));
    let persisted = read_json(&paths.runtime.join("retention.json"));
    assert_eq!(persisted["cursor"]["store"], "journals");
    assert_eq!(persisted["cursor"]["lastName"], "s-05");
    assert_eq!(run_ids_left(paths).len(), 10);
}

#[test]
fn the_health_gate_is_not_read_when_discard_unreflected_journals_is_off() {
    let id = identity();
    let paths = &id.paths;
    write_json(
        &paths.reflection.join("park.json"),
        &serde_json::json!({ "version": 1, "streak": 3, "parkedAt": iso(days_ago(1.0)) }),
    );

    let off = pass(paths, RuntimeRetentionPolicy::default());
    let on = pass(paths, unreflected_policy());

    assert_eq!(off.unreflected_gate, None);
    let gate = on.unreflected_gate.expect("gate");
    assert!(!gate.open);
    let newest = gate.newest.expect("newest event");
    assert_eq!(newest.kind, ReflectionTerminalKind::Parked);
    assert_eq!(newest.at, iso(days_ago(1.0)));
}

#[test]
fn a_pass_resumed_at_journals_that_spends_its_budget_still_processes_the_next_journal() {
    let id = identity();
    let paths = &id.paths;
    unfolded_runs(paths, 10);
    for index in 1..=8 {
        journal(
            paths,
            &format!("s-0{index}"),
            JournalShape::Fully,
            days_ago(10.0),
            JournalOptions::default(),
        );
    }
    write_json(
        &paths.runtime.join("retention.json"),
        &serde_json::json!({ "version": 1, "cursor": { "store": "journals", "lastName": "s-05" } }),
    );
    let clock = Cell::new(NOW);
    let now = || {
        clock.set(clock.get() + 400);
        clock.get()
    };
    let policy = RuntimeRetentionPolicy {
        pass_budget_ms: 1_000,
        ..RuntimeRetentionPolicy::default()
    };

    let report = run_runtime_retention(&RuntimeRetentionOptions::new(RetentionContext::new(
        paths, policy, &now,
    )))
    .expect("pass");

    assert_eq!(
        report.cursor,
        Some(ReportCursor::Store(RetentionCursor {
            store: "journals".to_string(),
            last_name: Some("s-06".to_string()),
        }))
    );
    assert_eq!(report.journals.removed, vec!["s-06".to_string()]);
    for session_id in ["s-01", "s-02", "s-03", "s-04", "s-05"] {
        assert!(
            exists(&paths.transcripts.join(session_id).join("transcript.jsonl")),
            "{session_id} lost its transcript"
        );
    }
}

#[test]
fn forty_folded_runs_and_a_small_budget_lose_every_removable_run_within_a_bounded_number_of_passes()
{
    let id = identity();
    let paths = &id.paths;
    folded_runs(paths, 40, days_ago(10.0));
    let clock = Cell::new(NOW);
    let now = || {
        clock.set(clock.get() + 400);
        clock.get()
    };
    let policy = RuntimeRetentionPolicy {
        pass_budget_ms: 1_000,
        ..RuntimeRetentionPolicy::default()
    };
    let mut passes = 0_u32;
    loop {
        let report = run_runtime_retention(&RuntimeRetentionOptions::new(RetentionContext::new(
            paths, policy, &now,
        )))
        .expect("pass");
        passes += 1;
        if report.cursor.is_none() {
            break;
        }
        assert!(passes < 40, "the pass kept stopping without finishing");
    }

    assert_eq!(
        run_ids_left(paths),
        vec![
            "reflection-run-40".to_string(),
            "reflection-run-9".to_string()
        ]
    );
}

#[test]
fn a_folded_run_whose_rename_to_a_tombstone_fails_is_kept_as_claim_failed_and_the_next_pass_removes_it()
 {
    let id = identity();
    let paths = &id.paths;
    let dir = reflection_run(
        paths,
        "reflection-run-5",
        RunShape {
            read_only_payload: true,
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
    let refuse = |from: &Path, _to: &Path| {
        if from.ends_with("reflection-run-5") {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "EPERM: operation not permitted, rename",
            ))
        } else {
            Ok(())
        }
    };
    let now = || NOW;
    let mut options =
        RuntimeRetentionOptions::new(RetentionContext::new(paths, unreflected_policy(), &now));
    options.context.before_claim_rename = Some(&refuse);

    let first = run_runtime_retention(&options).expect("first pass");

    assert_eq!(first.runs.kept["reflection-run-5"].as_str(), "claim-failed");
    assert!(exists(&dir));
    assert!(exists(&dir.join("transcript-payload.json")));
    assert!(tombstones(&runs_dir(paths)).is_empty());

    let second = pass(paths, unreflected_policy());

    assert_eq!(second.runs.removed, vec!["reflection-run-5".to_string()]);
    assert!(!exists(&dir));
}

fn fixture_journals(paths: &MemoryIdentityPaths) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let never = journal(
        paths,
        "s-never",
        JournalShape::Never,
        days_ago(60.0),
        JournalOptions::default(),
    );
    let partly = journal(
        paths,
        "s-partly",
        JournalShape::Partly,
        days_ago(60.0),
        JournalOptions::default(),
    );
    let no_steps = journal(
        paths,
        "s-nosteps",
        JournalShape::NoSteps,
        days_ago(60.0),
        JournalOptions::default(),
    );
    let fully = journal(
        paths,
        "s-full",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );
    (never, partly, no_steps, fully)
}

#[test]
fn the_shipped_default_policy_keeps_unreflected_journals_and_truncates_only_the_folded_one() {
    let id = identity();
    let paths = &id.paths;
    let (never, partly, no_steps, fully) = fixture_journals(paths);

    let report = pass(paths, RuntimeRetentionPolicy::default());

    assert!(!RuntimeRetentionPolicy::default().discard_unreflected_journals);
    assert_eq!(
        report.journals.kept["s-never"].as_str(),
        "unreflected-retention-off"
    );
    assert_eq!(
        report.journals.kept["s-nosteps"].as_str(),
        "unreflected-retention-off"
    );
    assert_eq!(
        report.journals.kept["s-partly"].as_str(),
        "unreflected-retention-off"
    );
    assert_eq!(report.journals.removed, vec!["s-full".to_string()]);
    for dir in [&never, &partly, &no_steps] {
        assert!(exists(&dir.join("transcript.jsonl")));
    }
    assert!(!exists(&fully.join("transcript.jsonl")));
}

#[test]
fn with_discard_unreflected_journals_on_the_unreflected_journals_go_on_a_healthy_identity() {
    let id = identity();
    let paths = &id.paths;
    let (never, partly, no_steps, fully) = fixture_journals(paths);

    let report = pass(paths, unreflected_policy());

    assert_eq!(
        report.journals.removed,
        vec![
            "s-full".to_string(),
            "s-never".to_string(),
            "s-nosteps".to_string(),
            "s-partly".to_string()
        ]
    );
    assert!(!exists(&never));
    assert!(!exists(&no_steps));
    assert!(!exists(&partly.join("transcript.jsonl")));
    assert!(!exists(&fully.join("transcript.jsonl")));
}

fn completion_with(paths: &MemoryIdentityPaths, run_id: &str, outcome: &str, at: i64) {
    write_json(
        &completions_dir(paths).join(format!("{run_id}.json")),
        &serde_json::json!({
            "schemaVersion": 1,
            "runId": run_id,
            "identity": "agent-test",
            "category": "quick",
            "conversationIds": [],
            "trigger": "manual",
            "outcome": outcome,
            "startedAt": iso(at - 60_000),
            "finishedAt": iso(at),
            "delivery": { "status": "consumed" },
        }),
    );
}

struct GateShape {
    label: &'static str,
    open: bool,
    reflection_enabled: Option<bool>,
    write: fn(&MemoryIdentityPaths),
}

fn newest_failed_two_days_ago(paths: &MemoryIdentityPaths) {
    completion_with(paths, "reflection-run-1", "failed", days_ago(2.0));
}

fn newest_failed_twenty_days_ago(paths: &MemoryIdentityPaths) {
    completion_with(paths, "reflection-run-1", "failed", days_ago(20.0));
}

fn only_abandoned_and_quarantined_runs(paths: &MemoryIdentityPaths) {
    reflection_run(
        paths,
        "reflection-run-1",
        RunShape {
            sentinel: Some(Sentinel::Abandoned),
            finished_at: Some(days_ago(20.0)),
            ..RunShape::default()
        },
    );
    reflection_run(
        paths,
        "reflection-run-2",
        RunShape {
            sentinel: Some(Sentinel::Quarantined),
            finished_at: Some(days_ago(20.0)),
            ..RunShape::default()
        },
    );
}

fn parked_after_a_success_thirty_days_ago(paths: &MemoryIdentityPaths) {
    completion_with(paths, "reflection-run-1", "merged", days_ago(30.0));
    write_json(
        &paths.reflection.join("park.json"),
        &serde_json::json!({ "version": 1, "streak": 3, "parkedAt": iso(days_ago(29.0)) }),
    );
}

fn newest_merged_three_days_ago(paths: &MemoryIdentityPaths) {
    completion_with(paths, "reflection-run-1", "failed", days_ago(9.0));
    completion_with(paths, "reflection-run-2", "timed_out", days_ago(8.0));
    completion_with(paths, "reflection-run-3", "merged", days_ago(3.0));
}

fn no_terminal_event(_paths: &MemoryIdentityPaths) {}

fn merged_ten_days_ago_and_a_quarantine_written_today(paths: &MemoryIdentityPaths) {
    completion_with(paths, "reflection-run-1", "merged", days_ago(10.0));
    reflection_run(
        paths,
        "reflection-run-2",
        RunShape {
            sentinel: Some(Sentinel::Quarantined),
            finished_at: Some(days_ago(30.0)),
            sentinel_written_at: Some(days_ago(0.0)),
            ..RunShape::default()
        },
    );
}

fn reflection_disabled_with_a_failure(paths: &MemoryIdentityPaths) {
    completion_with(paths, "reflection-run-1", "failed", days_ago(2.0));
}

#[test]
fn the_reflection_health_gate_decides_whether_unreflected_input_goes() {
    let shapes = [
        GateShape {
            label: "(i) newest failed 2 d ago, no success",
            open: false,
            reflection_enabled: None,
            write: newest_failed_two_days_ago,
        },
        GateShape {
            label: "(ii) newest failed 20 d ago, no success",
            open: false,
            reflection_enabled: None,
            write: newest_failed_twenty_days_ago,
        },
        GateShape {
            label: "(iii) only abandoned and quarantined run dirs",
            open: false,
            reflection_enabled: None,
            write: only_abandoned_and_quarantined_runs,
        },
        GateShape {
            label: "(iv) parked, newest completion a success 30 d ago",
            open: false,
            reflection_enabled: None,
            write: parked_after_a_success_thirty_days_ago,
        },
        GateShape {
            label: "(v) newest merged 3 d ago after older failures",
            open: true,
            reflection_enabled: None,
            write: newest_merged_three_days_ago,
        },
        GateShape {
            label: "(vi) no terminal event at all",
            open: true,
            reflection_enabled: None,
            write: no_terminal_event,
        },
        GateShape {
            label: "(vii) reflection disabled, newest failed",
            open: true,
            reflection_enabled: Some(false),
            write: reflection_disabled_with_a_failure,
        },
        GateShape {
            label: "(viii) merged 10 d ago, quarantined today for a 30-day-old generation",
            open: true,
            reflection_enabled: None,
            write: merged_ten_days_ago_and_a_quarantine_written_today,
        },
    ];

    for shape in shapes {
        let id = identity();
        let paths = &id.paths;
        (shape.write)(paths);
        let never = journal(
            paths,
            "s-never",
            JournalShape::Never,
            days_ago(15.0),
            JournalOptions::default(),
        );
        let partly = journal(
            paths,
            "s-partly",
            JournalShape::Partly,
            days_ago(15.0),
            JournalOptions::default(),
        );
        let now = || NOW;
        let mut context = RetentionContext::new(paths, unreflected_policy(), &now);
        context.reflection_enabled = shape.reflection_enabled;

        let report = run_runtime_retention(&RuntimeRetentionOptions::new(context))
            .unwrap_or_else(|error| panic!("{}: {error}", shape.label));

        if shape.open {
            assert_eq!(
                report.journals.removed,
                vec!["s-never".to_string(), "s-partly".to_string()],
                "{}",
                shape.label
            );
            assert!(!exists(&never), "{}", shape.label);
            assert!(!exists(&partly.join("transcript.jsonl")), "{}", shape.label);
            assert!(
                report.unreflected_gate.as_ref().expect("a gate").open,
                "{}",
                shape.label
            );
        } else {
            assert_eq!(
                report.journals.kept["s-never"].as_str(),
                "reflection-unhealthy",
                "{}",
                shape.label
            );
            assert_eq!(
                report.journals.kept["s-partly"].as_str(),
                "reflection-unhealthy",
                "{}",
                shape.label
            );
            assert!(exists(&never.join("transcript.jsonl")), "{}", shape.label);
            assert!(exists(&partly.join("transcript.jsonl")), "{}", shape.label);
            let gate = report.unreflected_gate.expect("a gate");
            assert!(!gate.open, "{}", shape.label);
            let newest = gate.newest.expect("the deciding event");
            assert!(
                matches!(
                    newest.kind,
                    ReflectionTerminalKind::Failed
                        | ReflectionTerminalKind::Abandoned
                        | ReflectionTerminalKind::Quarantined
                        | ReflectionTerminalKind::Parked
                ),
                "{}",
                shape.label
            );
        }
    }
}

#[test]
fn a_closed_gate_still_truncates_a_fully_reflected_journal() {
    let id = identity();
    let paths = &id.paths;
    completion_with(paths, "reflection-run-1", "failed", days_ago(2.0));
    let fully = journal(
        paths,
        "s-full",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );

    let report = pass(paths, unreflected_policy());

    assert_eq!(report.journals.removed, vec!["s-full".to_string()]);
    assert!(!exists(&fully.join("transcript.jsonl")));
}
