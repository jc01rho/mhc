use super::*;
use std::cell::RefCell;
use tempfile::tempdir;

fn dead(_: u32) -> ProcessLiveness {
    ProcessLiveness::Dead
}

fn alive(_: u32) -> ProcessLiveness {
    ProcessLiveness::Alive
}

fn seams(liveness: &dyn Fn(u32) -> ProcessLiveness) -> RunTerminalClaimSeams<'_> {
    RunTerminalClaimSeams {
        pid_liveness: Some(liveness),
        before_recovery: None,
    }
}

fn write_claim(run_dir: &Path, kind: &str, attempt: i64, pid: u32) {
    std::fs::write(
        run_terminal_claim_path(run_dir),
        serde_json::json!({
            "version": 1,
            "kind": kind,
            "runId": "run-1",
            "attempt": attempt,
            "claimantPid": pid,
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn the_first_claimant_creates_the_claim() {
    let dir = tempdir().unwrap();

    let claim = claim_run_terminal(
        dir.path(),
        "run-1",
        1,
        RunTerminalClaimKind::Abandon,
        &seams(&alive),
        4242,
    )
    .unwrap();

    assert_eq!(claim.kind, RunTerminalClaimKind::Abandon);
    assert_eq!(claim.claimant_pid, 4242);
    assert_eq!(read_run_terminal_claim(dir.path()).unwrap(), claim);
    let written: Value = serde_json::from_str(
        &std::fs::read_to_string(run_terminal_claim_path(dir.path())).unwrap(),
    )
    .unwrap();
    assert_eq!(written["kind"], "abandon");
    assert_eq!(written["runId"], "run-1");
    assert_eq!(written["claimantPid"], 4242);
}

#[test]
fn no_candidate_file_is_left_beside_the_claim() {
    let dir = tempdir().unwrap();

    claim_run_terminal(
        dir.path(),
        "run-1",
        1,
        RunTerminalClaimKind::Publish,
        &seams(&alive),
        4242,
    )
    .unwrap();

    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["terminal-claim.json".to_string()]);
}

#[test]
fn a_matching_claim_is_returned_unchanged() {
    let dir = tempdir().unwrap();
    write_claim(dir.path(), "abandon", 1, 111);

    let claim = claim_run_terminal(
        dir.path(),
        "run-1",
        1,
        RunTerminalClaimKind::Abandon,
        &seams(&dead),
        222,
    )
    .unwrap();

    assert_eq!(claim.claimant_pid, 111);
}

#[test]
fn a_claim_by_a_live_publisher_wins_over_an_abandon_request() {
    let dir = tempdir().unwrap();
    write_claim(dir.path(), "publish", 1, 111);

    let claim = claim_run_terminal(
        dir.path(),
        "run-1",
        1,
        RunTerminalClaimKind::Abandon,
        &seams(&alive),
        222,
    )
    .unwrap();

    assert_eq!(claim.kind, RunTerminalClaimKind::Publish);
    assert_eq!(claim.claimant_pid, 111);
    assert!(!run_terminal_claim_matches(
        &claim,
        "run-1",
        1,
        Some(RunTerminalClaimKind::Abandon)
    ));
}

#[test]
fn a_publish_claim_by_a_dead_process_is_replaced() {
    let dir = tempdir().unwrap();
    write_claim(dir.path(), "publish", 1, 111);
    let reasons = RefCell::new(Vec::new());
    let record = |reason: RecoveryReason| reasons.borrow_mut().push(reason);
    let hooked = RunTerminalClaimSeams {
        pid_liveness: Some(&dead),
        before_recovery: Some(&record),
    };

    let claim = claim_run_terminal(
        dir.path(),
        "run-1",
        1,
        RunTerminalClaimKind::Abandon,
        &hooked,
        222,
    )
    .unwrap();

    assert_eq!(claim.kind, RunTerminalClaimKind::Abandon);
    assert_eq!(claim.claimant_pid, 222);
    assert_eq!(reasons.into_inner(), vec![RecoveryReason::DeadPublish]);
    assert_eq!(read_run_terminal_claim(dir.path()).unwrap(), claim);
}

#[test]
fn an_abandon_claim_is_never_taken_over_even_when_its_claimant_is_dead() {
    let dir = tempdir().unwrap();
    write_claim(dir.path(), "abandon", 2, 111);

    let claim = claim_run_terminal(
        dir.path(),
        "run-1",
        1,
        RunTerminalClaimKind::Publish,
        &seams(&dead),
        222,
    )
    .unwrap();

    assert_eq!(claim.kind, RunTerminalClaimKind::Abandon);
    assert_eq!(claim.attempt, 2);
}

#[test]
fn an_unreadable_claim_authorized_nothing_and_is_discarded() {
    let dir = tempdir().unwrap();
    std::fs::write(run_terminal_claim_path(dir.path()), "").unwrap();
    let reasons = RefCell::new(Vec::new());
    let record = |reason: RecoveryReason| reasons.borrow_mut().push(reason);
    let hooked = RunTerminalClaimSeams {
        pid_liveness: Some(&alive),
        before_recovery: Some(&record),
    };

    let claim = claim_run_terminal(
        dir.path(),
        "run-1",
        1,
        RunTerminalClaimKind::Abandon,
        &hooked,
        222,
    )
    .unwrap();

    assert_eq!(claim.claimant_pid, 222);
    assert_eq!(reasons.into_inner(), vec![RecoveryReason::Invalid]);
}

#[test]
fn a_claim_that_is_a_directory_cannot_be_recovered() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(run_terminal_claim_path(dir.path())).unwrap();

    let error = claim_run_terminal(
        dir.path(),
        "run-1",
        1,
        RunTerminalClaimKind::Abandon,
        &seams(&alive),
        222,
    )
    .unwrap_err();

    assert!(matches!(error, TerminalClaimError::Unrecoverable(_)));
}

#[test]
fn a_terminal_sentinel_settles_the_run() {
    for name in ["final.json", "abandoned.json", "quarantined.json"] {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(name), "{}").unwrap();

        assert_eq!(
            read_run_terminal_publication(dir.path(), Some(1)).unwrap(),
            Some(RunTerminalPublication::Settled)
        );
    }
}

#[test]
fn an_outcome_for_this_attempt_finalizes_and_one_for_another_attempt_does_not() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("outcome.json"), r#"{"attempt":2}"#).unwrap();

    assert_eq!(
        read_run_terminal_publication(dir.path(), Some(2)).unwrap(),
        Some(RunTerminalPublication::Finalize)
    );
    assert_eq!(
        read_run_terminal_publication(dir.path(), Some(1)).unwrap(),
        None
    );
}

#[test]
fn a_publication_in_progress_vetoes_an_abandon() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("publishing.json"),
        r#"{"version":1,"attempt":1}"#,
    )
    .unwrap();

    assert_eq!(
        read_run_terminal_publication(dir.path(), Some(1)).unwrap(),
        Some(RunTerminalPublication::Veto)
    );
}

#[test]
fn nothing_recorded_means_a_claim_may_be_attempted() {
    let dir = tempdir().unwrap();

    assert_eq!(
        read_run_terminal_publication(dir.path(), Some(1)).unwrap(),
        None
    );
}

#[test]
fn a_claim_is_valid_only_with_a_positive_integer_claimant() {
    let dir = tempdir().unwrap();
    for (claimant, valid) in [(0, false), (-3, false), (7, true)] {
        std::fs::write(
            run_terminal_claim_path(dir.path()),
            serde_json::json!({
                "version": 1,
                "kind": "publish",
                "runId": "run-1",
                "attempt": 1,
                "claimantPid": claimant,
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(read_run_terminal_claim(dir.path()).is_ok(), valid);
    }
}
