use super::*;

fn record(
    conversation_id: &str,
    state: FactsFailureState,
    reason: FactsFailureReason,
    failure_id: &str,
    last_failure_at: &str,
) -> FactsFailureRecord {
    FactsFailureRecord {
        conversation_id: conversation_id.to_owned(),
        end_message_id: format!("{conversation_id}-end"),
        end_snapshot_line: 3,
        state,
        streak: 5,
        first_failure_at: "2026-11-01T09:00:00.000Z".to_owned(),
        last_failure_at: last_failure_at.to_owned(),
        last_reason: reason,
        last_detail: None,
        last_failure_id: failure_id.to_owned(),
        next_eligible_at: None,
        parked_at: None,
    }
}

fn parked(
    conversation_id: &str,
    reason: FactsFailureReason,
    failure_id: &str,
) -> FactsFailureRecord {
    record(
        conversation_id,
        FactsFailureState::Parked,
        reason,
        failure_id,
        "2026-11-01T10:00:00.000Z",
    )
}

fn write_ledger(facts_dir: &Path, run: &str, ledger: serde_json::Value) -> PathBuf {
    let run_dir = facts_dir.join("runs").join(run);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("ledger.json"), ledger.to_string()).unwrap();
    run_dir
}

#[test]
fn a_transient_cause_names_no_fix() {
    let records = [parked("a", FactsFailureReason::DeadlineExceeded, "batch-1")];

    assert_eq!(dominant_fix_first_cause(&records), None);
}

#[test]
fn a_setting_fix_names_the_setting() {
    let records = [
        parked("a", FactsFailureReason::DeadlineExceeded, "batch-1"),
        parked("b", FactsFailureReason::QuickCategoryUnavailable, "batch-2"),
    ];

    let cause = dominant_fix_first_cause(&records).unwrap();

    assert_eq!(cause.setting, Some("memory.facts.model"));
    assert_eq!(cause.action, FactsFailureAction::FixFirst);
}

#[test]
fn a_child_exit_is_labelled_with_its_distilled_cause() {
    let mut exited = parked("a", FactsFailureReason::ChildExit, "batch-1");
    exited.last_detail = Some("  at x (y.js:1:2)\nTypeError: boom".to_owned());

    let cause = dominant_fix_first_cause(&[exited]).unwrap();

    assert_eq!(cause.label.as_deref(), Some("TypeError: boom"));
}

#[test]
fn the_summary_counts_causes_and_names_the_newest_surviving_run() {
    let root = tempfile::tempdir().unwrap();
    let run_dir = write_ledger(
        root.path(),
        "facts-abc-1",
        serde_json::json!({"batchId": "batch-2", "runId": "facts-abc-1"}),
    );
    let mut newest = parked("c", FactsFailureReason::InvalidExtraction, "batch-2");
    newest.last_failure_at = "2026-11-01T12:00:00.000Z".to_owned();
    let records = [
        parked("a", FactsFailureReason::DeadlineExceeded, "batch-1"),
        parked("b", FactsFailureReason::DeadlineExceeded, "batch-1"),
        newest,
    ];

    let summary = facts_parked_cause_summary(&records, Some(root.path())).unwrap();

    assert_eq!(
        summary,
        format!(
            "causes: timeout x2, parse error x1; last run: {}",
            run_dir.display()
        )
    );
}

#[test]
fn an_unknown_cause_still_shows_the_run_path() {
    let root = tempfile::tempdir().unwrap();
    let run_dir = write_ledger(
        root.path(),
        "facts-abc-1",
        serde_json::json!({"batchId": "batch-1"}),
    );
    let records = [parked("a", FactsFailureReason::Other, "batch-1")];

    assert_eq!(
        facts_parked_cause_summary(&records, Some(root.path())),
        Some(format!("last run: {}", run_dir.display()))
    );
}

#[test]
fn backoff_only_has_no_parked_summary() {
    let records = [record(
        "a",
        FactsFailureState::Backoff,
        FactsFailureReason::DeadlineExceeded,
        "batch-1",
        "2026-11-01T10:00:00.000Z",
    )];

    assert_eq!(facts_parked_cause_summary(&records, None), None);
}

#[test]
fn a_pruned_or_mismatched_run_yields_no_path() {
    let root = tempfile::tempdir().unwrap();
    write_ledger(
        root.path(),
        "facts-abc-1",
        serde_json::json!({"batchId": "batch-1", "runId": "facts-other-1"}),
    );
    write_ledger(
        root.path(),
        ".prune-facts-abc-2",
        serde_json::json!({"batchId": "batch-2"}),
    );
    let records = [
        parked("a", FactsFailureReason::DeadlineExceeded, "batch-1"),
        parked("b", FactsFailureReason::DeadlineExceeded, "batch-2"),
        parked("c", FactsFailureReason::DeadlineExceeded, "batch-3"),
    ];

    assert_eq!(parked_run_path(&records, Some(root.path())), None);
    assert_eq!(parked_run_path(&records, None), None);
}
