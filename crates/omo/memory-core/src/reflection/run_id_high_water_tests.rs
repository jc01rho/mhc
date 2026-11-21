use super::*;
use tempfile::tempdir;

#[test]
fn run_numbers_parse_from_every_trace_name() {
    assert_eq!(reflection_run_number("reflection-run-7"), 7);
    assert_eq!(reflection_run_number("reflection-run-7.json"), 7);
    assert_eq!(reflection_run_number("1755000000000-reflection-run-12"), 12);
    assert_eq!(reflection_run_number("reflection-run-"), 0);
    assert_eq!(reflection_run_number("reflection-run-7.json.json"), 0);
    assert_eq!(reflection_run_number("12-34-reflection-run-5"), 0);
    assert_eq!(reflection_run_number("facts-run-9"), 0);
    assert_eq!(reflection_run_number("reflection-run-9007199254740992"), 0);
}

#[test]
fn the_scan_takes_the_highest_number_any_trace_carries() {
    let root = tempdir().unwrap();
    let reflection = root.path().join("reflection");
    let sessions = root.path().join("reflection-sessions");
    let worktrees = root.path().join("worktrees");
    std::fs::create_dir_all(reflection.join("completions")).unwrap();
    std::fs::create_dir_all(reflection.join("runs").join("reflection-run-4")).unwrap();
    std::fs::create_dir_all(sessions.join("reflection-run-6")).unwrap();
    std::fs::create_dir_all(worktrees.join("1755000000000-reflection-run-5")).unwrap();
    std::fs::write(
        reflection.join("completions").join("reflection-run-3.json"),
        "{}",
    )
    .unwrap();
    std::fs::write(
        reflection.join("pending.json"),
        r#"{"runId":"reflection-run-9"}"#,
    )
    .unwrap();
    std::fs::write(reflection.join("active.lock"), "not json").unwrap();

    let paths = RunIdHighWaterPaths {
        reflection: &reflection,
        reflection_sessions: &sessions,
        worktrees: &worktrees,
    };

    assert_eq!(scan_reflection_run_high_water(paths), 9);
}

#[test]
fn the_scan_of_an_empty_identity_is_zero() {
    let root = tempdir().unwrap();
    let missing = root.path().join("missing");
    let paths = RunIdHighWaterPaths {
        reflection: &missing,
        reflection_sessions: &missing,
        worktrees: &missing,
    };

    assert_eq!(scan_reflection_run_high_water(paths), 0);
}

#[test]
fn the_durable_mark_is_raised_but_never_lowered() {
    let root = tempdir().unwrap();
    let reflection = root.path().join("reflection");

    assert_eq!(read_reflection_run_id_mark(&reflection), None);
    assert_eq!(record_reflection_run_id_mark(&reflection, 41).unwrap(), 41);
    assert_eq!(record_reflection_run_id_mark(&reflection, 12).unwrap(), 41);
    assert_eq!(read_reflection_run_id_mark(&reflection), Some(41));
    assert_eq!(
        std::fs::read_to_string(reflection.join("run-id.json")).unwrap(),
        "{\"version\":1,\"highWater\":41}\n"
    );
    let leftovers: Vec<String> = std::fs::read_dir(&reflection)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}

#[test]
fn an_unreadable_mark_counts_as_absent() {
    let root = tempdir().unwrap();
    let reflection = root.path().join("reflection");
    std::fs::create_dir_all(&reflection).unwrap();

    std::fs::write(reflection.join("run-id.json"), "[1]").unwrap();
    assert_eq!(read_reflection_run_id_mark(&reflection), None);
    std::fs::write(reflection.join("run-id.json"), r#"{"highWater":-1}"#).unwrap();
    assert_eq!(read_reflection_run_id_mark(&reflection), None);
    std::fs::write(reflection.join("run-id.json"), r#"{"highWater":3.5}"#).unwrap();
    assert_eq!(read_reflection_run_id_mark(&reflection), None);
    std::fs::write(reflection.join("run-id.json"), r#"{"highWater":3.0}"#).unwrap();
    assert_eq!(read_reflection_run_id_mark(&reflection), Some(3));
}
