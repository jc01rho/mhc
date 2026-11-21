use super::*;
use tempfile::tempdir;

fn record(launcher: Option<AbandonedLauncher>) -> AbandonedRecord {
    AbandonedRecord {
        version: 1,
        run_id: "reflection-run-3".to_string(),
        abandoned_at: "2026-11-20T00:00:00.000Z".to_string(),
        reason: AbandonedReason::LaunchInterrupted,
        generation: "2026-11-19T23:59:00.000Z".to_string(),
        kind: "reflection".to_string(),
        trigger: "manual".to_string(),
        origin: None,
        launcher,
    }
}

#[test]
fn the_sentinel_is_written_with_the_camel_case_fields_other_readers_expect() {
    let dir = tempdir().unwrap();

    write_abandoned(dir.path(), &record(None)).unwrap();

    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(ABANDONED_FILENAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(written["runId"], "reflection-run-3");
    assert_eq!(written["reason"], "launch_interrupted");
    assert_eq!(written["generation"], "2026-11-19T23:59:00.000Z");
    assert!(written.get("origin").is_none());
    assert!(written.get("launcher").is_none());
}

#[test]
fn a_launcher_without_pid_keeps_a_null_process_start_and_drops_the_missing_fields() {
    let dir = tempdir().unwrap();
    let launcher = AbandonedLauncher {
        pid: None,
        hostname: None,
        process_start: None,
    };

    write_abandoned(dir.path(), &record(Some(launcher))).unwrap();

    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(ABANDONED_FILENAME)).unwrap(),
    )
    .unwrap();
    let launcher = &written["launcher"];
    assert!(launcher.get("pid").is_none());
    assert!(launcher.get("hostname").is_none());
    assert!(launcher["processStart"].is_null());
}

#[test]
fn reasons_serialize_to_the_names_the_receipts_use() {
    assert_eq!(
        AbandonedReason::LaunchInterrupted.as_str(),
        "launch_interrupted"
    );
    assert_eq!(AbandonedReason::ProcessesDead.as_str(), "processes_dead");
    assert_eq!(
        serde_json::to_value(AbandonedReason::ProcessesDead).unwrap(),
        "processes_dead"
    );
}
