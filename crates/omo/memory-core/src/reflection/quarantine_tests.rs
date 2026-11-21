use super::*;
use tempfile::tempdir;

fn record() -> QuarantineRecord {
    QuarantineRecord {
        version: 1,
        run_id: "reflection-run-4".to_string(),
        kind: "reflection".to_string(),
        trigger: "step-count".to_string(),
        origin: None,
        generation: "2026-11-19T23:59:00.000Z".to_string(),
        reason: QuarantineReason::StrandedNoLedger,
        quarantined_at: "2026-11-20T00:00:00.000Z".to_string(),
        evidence: vec!["prelaunch.json".to_string()],
    }
}

#[test]
fn the_sentinel_alone_is_written_without_a_reservation() {
    let dir = tempdir().unwrap();

    write_quarantine(dir.path(), &record(), None).unwrap();

    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(QUARANTINE_FILENAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(written["reason"], "stranded_no_ledger");
    assert_eq!(written["runId"], "reflection-run-4");
    assert_eq!(written["evidence"][0], "prelaunch.json");
    assert!(!dir.path().join(RESERVATION_EVIDENCE_FILENAME).exists());
}

#[test]
fn the_reservation_is_kept_beside_the_sentinel_as_evidence() {
    let dir = tempdir().unwrap();
    let reservation = serde_json::json!({"runId": "reflection-run-4", "future": {"kept": true}});

    write_quarantine(dir.path(), &record(), Some(&reservation)).unwrap();

    let evidence: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(RESERVATION_EVIDENCE_FILENAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(evidence, reservation);
    assert!(dir.path().join(QUARANTINE_FILENAME).exists());
}

#[test]
fn every_reason_serializes_to_its_receipt_name() {
    for reason in [
        QuarantineReason::LedgerUnreadable,
        QuarantineReason::PrelaunchMissingAfterDeadline,
        QuarantineReason::TerminalClaimUnrecoverable,
        QuarantineReason::StrandedNoLedger,
    ] {
        assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_str());
    }
}
