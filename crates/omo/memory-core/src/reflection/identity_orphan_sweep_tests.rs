use super::*;
use tempfile::tempdir;

fn touch(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn an_identity_without_a_repo_is_skipped() {
    let root = tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("runtime")).unwrap();

    let report = sweep_identity_orphan_worktrees(
        root.path(),
        &IdentityOrphanSweepOptions {
            now_ms: 0.0,
            dry_run: true,
        },
    )
    .unwrap();

    assert_eq!(report.skipped, Some("no-repo"));
    assert!(report.removed.is_empty());
    assert!(report.kept.is_empty());
    assert!(report.failed.is_empty());
}

#[test]
fn reserved_runs_and_unsettled_run_dirs_are_live() {
    let reflection = tempdir().unwrap();
    touch(
        reflection.path(),
        "active.lock",
        r#"{"runId":"reflection-run-1"}"#,
    );
    touch(
        reflection.path(),
        "pending.json",
        r#"{"runId":"reflection-run-2"}"#,
    );
    touch(reflection.path(), "runs/reflection-run-3/ledger.json", "{}");
    touch(
        reflection.path(),
        "runs/reflection-run-4/prelaunch.json",
        "{}",
    );

    let live = collect_reflection_live_run_ids(reflection.path());

    assert_eq!(
        live.into_iter().collect::<Vec<_>>(),
        vec![
            "reflection-run-1".to_string(),
            "reflection-run-2".to_string(),
            "reflection-run-3".to_string(),
            "reflection-run-4".to_string(),
        ]
    );
}

#[test]
fn a_run_with_a_terminal_sentinel_is_not_live_whichever_sentinel_it_has() {
    let reflection = tempdir().unwrap();
    for (run, sentinel) in [
        ("reflection-run-1", "final.json"),
        ("reflection-run-2", "abandoned.json"),
        ("reflection-run-3", "quarantined.json"),
    ] {
        touch(reflection.path(), &format!("runs/{run}/ledger.json"), "{}");
        touch(reflection.path(), &format!("runs/{run}/{sentinel}"), "{}");
    }

    assert!(collect_reflection_live_run_ids(reflection.path()).is_empty());
}

#[test]
fn tombstones_and_empty_run_dirs_are_not_live() {
    let reflection = tempdir().unwrap();
    touch(
        reflection.path(),
        "runs/.prune-reflection-run-5/ledger.json",
        "{}",
    );
    std::fs::create_dir_all(reflection.path().join("runs/reflection-run-6")).unwrap();

    assert!(collect_reflection_live_run_ids(reflection.path()).is_empty());
}

#[test]
fn an_unreadable_reservation_file_names_no_run() {
    let reflection = tempdir().unwrap();
    touch(reflection.path(), "active.lock", "{not json");

    assert!(collect_reflection_live_run_ids(reflection.path()).is_empty());
}
