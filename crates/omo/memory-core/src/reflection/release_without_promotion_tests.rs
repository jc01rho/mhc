use super::*;
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    reflection: std::path::PathBuf,
    transcripts: std::path::PathBuf,
    locks: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let reflection = root.path().join("reflection");
    let transcripts = root.path().join("transcripts");
    let locks = root.path().join("locks");
    for dir in [&reflection, &transcripts, &locks] {
        std::fs::create_dir_all(dir).unwrap();
    }
    Fixture {
        _root: root,
        reflection,
        transcripts,
        locks,
    }
}

fn reservation(run_id: &str, trigger: &str) -> String {
    serde_json::json!({
        "runId": run_id,
        "request": {"trigger": trigger, "conversationIds": ["conversation-1"], "snapshots": []},
        "launcherPid": 1,
        "launcherHostname": "host",
    })
    .to_string()
}

fn release(fixture: &Fixture, run_id: &str) -> Result<ReleaseWithoutPromotionResult, ReleaseError> {
    release_reservation_without_promotion(&ReleaseWithoutPromotionOptions {
        reflection_dir: &fixture.reflection,
        transcripts_dir: &fixture.transcripts,
        locks_dir: &fixture.locks,
        run_id,
    })
}

fn failures(fixture: &Fixture) -> Option<u64> {
    TranscriptJournal::new(TranscriptJournalOptions::new(
        fixture.transcripts.join("conversation-1"),
    ))
    .get_state()
    .unwrap()
    .consecutive_failures
    .map(u64::from)
}

#[test]
fn the_matching_reservation_is_released_and_the_pending_run_is_left_queued() {
    let fixture = fixture();
    std::fs::write(
        fixture.reflection.join("active.lock"),
        reservation("reflection-run-1", "manual"),
    )
    .unwrap();
    let pending = reservation("reflection-run-2", "step-count");
    std::fs::write(fixture.reflection.join("pending.json"), &pending).unwrap();

    let result = release(&fixture, "reflection-run-1").unwrap();

    assert_eq!(
        result,
        ReleaseWithoutPromotionResult {
            released: true,
            pending_run_id: Some("reflection-run-2".to_string()),
        }
    );
    assert!(!fixture.reflection.join("active.lock").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.reflection.join("pending.json")).unwrap(),
        pending
    );
}

#[test]
fn another_runs_reservation_is_not_released() {
    let fixture = fixture();
    std::fs::write(
        fixture.reflection.join("active.lock"),
        reservation("reflection-run-9", "manual"),
    )
    .unwrap();

    let result = release(&fixture, "reflection-run-1").unwrap();

    assert!(!result.released);
    assert!(fixture.reflection.join("active.lock").exists());
}

#[test]
fn nothing_is_released_when_there_is_no_reservation() {
    let fixture = fixture();

    let result = release(&fixture, "reflection-run-1").unwrap();

    assert_eq!(
        result,
        ReleaseWithoutPromotionResult {
            released: false,
            pending_run_id: None,
        }
    );
}

#[test]
fn an_automatic_run_records_the_failure_on_its_journals() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.transcripts.join("conversation-1")).unwrap();
    std::fs::write(
        fixture.reflection.join("active.lock"),
        reservation("reflection-run-1", "step-count"),
    )
    .unwrap();

    assert!(release(&fixture, "reflection-run-1").unwrap().released);

    assert_eq!(failures(&fixture), Some(1));
}

#[test]
fn a_manual_run_leaves_its_journals_alone() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.transcripts.join("conversation-1")).unwrap();
    std::fs::write(
        fixture.reflection.join("active.lock"),
        reservation("reflection-run-1", "manual"),
    )
    .unwrap();

    assert!(release(&fixture, "reflection-run-1").unwrap().released);

    assert_eq!(failures(&fixture), None);
}

#[test]
fn a_journal_that_does_not_exist_is_skipped() {
    let fixture = fixture();
    std::fs::write(
        fixture.reflection.join("active.lock"),
        reservation("reflection-run-1", "compaction"),
    )
    .unwrap();

    assert!(release(&fixture, "reflection-run-1").unwrap().released);

    assert!(!fixture.transcripts.join("conversation-1").exists());
}

#[test]
fn a_held_scheduler_lock_is_reported_as_contention_and_nothing_changes() {
    let fixture = fixture();
    std::fs::write(
        fixture.reflection.join("active.lock"),
        reservation("reflection-run-1", "manual"),
    )
    .unwrap();
    let holder = create_lock_record(
        "reflection-scheduler",
        CreateLockRecordOptions { run_id: None },
    )
    .unwrap();

    let inner = with_lock(
        &reflection_scheduler_lock_path(&fixture.locks),
        &holder,
        &AcquireLockOptions {
            wait_timeout_ms: Some(0),
            ..Default::default()
        },
        || Ok::<_, String>(release(&fixture, "reflection-run-1")),
    )
    .unwrap();

    assert!(matches!(inner, Err(ReleaseError::Contention(_))));
    assert!(fixture.reflection.join("active.lock").exists());
}
