use super::*;
use crate::locks::lock_record::LockRecord;
use crate::receipts::receipts::{ReadMemoryReceiptsOptions, read_memory_receipts};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

const NOW: f64 = 1_793_858_400_000.0;
const HOST: &str = "reaper-host";
const RUN: &str = "reflection-run-64";
const TWO_HOURS_MS: f64 = 2.0 * 3_600_000.0;

fn host_name() -> String {
    HOST.to_string()
}

fn fixed_now() -> f64 {
    NOW
}

fn all_dead(_: u32) -> ProcessLiveness {
    ProcessLiveness::Dead
}

fn supervisor_alive(pid: u32) -> ProcessLiveness {
    if pid == 4201 {
        ProcessLiveness::Alive
    } else {
        ProcessLiveness::Dead
    }
}

fn launcher_alive(pid: u32) -> ProcessLiveness {
    if pid == 11096 {
        ProcessLiveness::Alive
    } else {
        ProcessLiveness::Dead
    }
}

fn claimant_alive(pid: u32) -> ProcessLiveness {
    if pid == 9191 {
        ProcessLiveness::Alive
    } else {
        ProcessLiveness::Dead
    }
}

fn no_start(_: u32) -> Option<String> {
    None
}

fn recorded_start(_: u32) -> Option<String> {
    Some("proc-start-epoch:1".to_string())
}

fn long_ago(_: &Path) -> Option<f64> {
    Some(NOW - TWO_HOURS_MS)
}

fn just_now(_: &Path) -> Option<f64> {
    Some(NOW - 1_000.0)
}

fn old_lock_young_run_dir(path: &Path) -> Option<f64> {
    if path.ends_with("active.lock") {
        Some(NOW - TWO_HOURS_MS)
    } else {
        Some(NOW - 1_000.0)
    }
}

static HOST_FN: fn() -> String = host_name;
static NOW_FN: fn() -> f64 = fixed_now;
static ALL_DEAD: fn(u32) -> ProcessLiveness = all_dead;
static SUPERVISOR_ALIVE: fn(u32) -> ProcessLiveness = supervisor_alive;
static LAUNCHER_ALIVE: fn(u32) -> ProcessLiveness = launcher_alive;
static CLAIMANT_ALIVE: fn(u32) -> ProcessLiveness = claimant_alive;
static NO_START: fn(u32) -> Option<String> = no_start;
static RECORDED_START: fn(u32) -> Option<String> = recorded_start;
static LONG_AGO: fn(&Path) -> Option<f64> = long_ago;
static JUST_NOW: fn(&Path) -> Option<f64> = just_now;
static OLD_LOCK_YOUNG_RUN_DIR: fn(&Path) -> Option<f64> = old_lock_young_run_dir;
static LONG_AGO_EXCEPT_RUN_8: fn(&Path) -> Option<f64> = long_ago_except_run_8;

fn probe_full<'a>(
    liveness: &'a dyn Fn(u32) -> ProcessLiveness,
    start: &'a dyn Fn(u32) -> Option<String>,
    modified: &'a dyn Fn(&Path) -> Option<f64>,
) -> StaleLockProbe<'a> {
    StaleLockProbe {
        base: ReflectionLockProbe {
            read_text: None,
            hostname: Some(&HOST_FN),
            pid_liveness: Some(liveness),
            process_start: Some(start),
        },
        now: Some(&NOW_FN),
        list_directory: None,
        modified_at: Some(modified),
    }
}

fn probe<'a>(
    liveness: &'a dyn Fn(u32) -> ProcessLiveness,
    modified: &'a dyn Fn(&Path) -> Option<f64>,
) -> StaleLockProbe<'a> {
    probe_full(liveness, &NO_START, modified)
}

struct Fixture {
    _root: TempDir,
    runtime: PathBuf,
    reflection: PathBuf,
    locks: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    let reflection = runtime.join("reflection");
    let locks = runtime.join("locks");
    for dir in [&reflection, &locks] {
        std::fs::create_dir_all(dir).unwrap();
    }
    Fixture {
        _root: root,
        runtime,
        reflection,
        locks,
    }
}

fn write_text(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn write_json(path: &Path, value: &Value) {
    write_text(path, &format!("{value:#}\n"));
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn iso(millis: f64) -> String {
    format_rfc3339_millis(millis as i64)
}

fn merged(mut base: Value, patch: Value) -> Value {
    if let (Some(base), Some(patch)) = (base.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

fn active_lock(fixture: &Fixture, patch: Value) -> Value {
    let record = merged(
        json!({
            "runId": RUN,
            "request": {"trigger": "manual", "conversationIds": ["conversation-a"], "snapshots": []},
            "reservedAt": iso(NOW - 10.0 * 60_000.0),
            "launcherPid": 11096,
            "launcherHostname": HOST,
            "launcherProcessStart": null,
        }),
        patch,
    );
    write_json(&fixture.reflection.join("active.lock"), &record);
    record
}

fn pending_run(fixture: &Fixture) {
    write_json(
        &fixture.reflection.join("pending.json"),
        &json!({
            "runId": "reflection-run-65",
            "request": {"trigger": "manual", "conversationIds": ["conversation-a"], "snapshots": []},
        }),
    );
}

fn valid_ledger(run_id: &str, patch: Value) -> Value {
    merged(
        json!({
            "version": 1,
            "runId": run_id,
            "kind": "reflection",
            "trigger": "manual",
            "startedAt": iso(NOW - 9.0 * 60_000.0),
            "hardDeadlineAt": NOW - 60_000.0,
            "terminationGraceMs": 100,
            "deadlineAt": NOW - 60_000.0 + 100.0,
            "mergePolicy": "auto",
            "pid": 4201,
            "processStart": null,
            "childPid": 4202,
            "childProcessStart": null,
            "worktreeDir": "/nonexistent/worktree",
            "worktreeBranch": format!("reflection/{run_id}"),
            "baseSha": "0".repeat(40),
            "gitFilePath": "/nonexistent/worktree/.git",
            "gitFileSnapshot": "gitdir: /nonexistent",
            "commonConfigPath": "/nonexistent/config",
            "commonConfigSnapshot": null,
        }),
        patch,
    )
}

fn run_dir_of(fixture: &Fixture, run_id: &str) -> PathBuf {
    fixture.reflection.join("runs").join(run_id)
}

fn write_ledger(fixture: &Fixture, patch: Value) {
    write_json(
        &run_dir_of(fixture, RUN).join("ledger.json"),
        &valid_ledger(RUN, patch),
    );
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.insert(name, Vec::new());
                walk(root, &path, out);
            } else {
                out.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn decide(fixture: &Fixture, probe: &StaleLockProbe<'_>) -> StaleLockDecision {
    decide_stale_reflection_lock(&fixture.reflection, probe).1
}

fn reap(action: StaleLockReapAction, reason: &str) -> StaleLockDecision {
    StaleLockDecision::Reap {
        action,
        run_id: RUN.to_string(),
        reason: reason.to_string(),
    }
}

fn stuck(reason: &str) -> StaleLockDecision {
    StaleLockDecision::Stuck {
        run_id: RUN.to_string(),
        reason: reason.to_string(),
    }
}

fn reaper<'a>(fixture: &'a Fixture, probe: StaleLockProbe<'a>) -> StaleLockReaperOptions<'a> {
    let mut options = StaleLockReaperOptions::new(&fixture.runtime);
    options.probe = probe;
    options
}

fn receipt_events(fixture: &Fixture) -> Vec<(String, String, Option<String>)> {
    read_memory_receipts(&fixture.runtime, &ReadMemoryReceiptsOptions::default())
        .unwrap()
        .receipts
        .iter()
        .filter_map(|receipt| match &receipt.input {
            MemoryReceiptInput::Run {
                run_id,
                event,
                reason,
                ..
            } => Some((run_id.clone(), event.clone(), reason.clone())),
            MemoryReceiptInput::Facts { .. } => None,
        })
        .collect()
}

const OUTCOME: &str = r#"{"version":1,"runId":"reflection-run-64","attempt":1,"finishedAt":"2026-11-05T05:55:00.000Z","childExit":{"code":0,"signal":null},"timedOut":false}"#;

#[test]
fn no_lock_and_a_live_launcher_need_nothing() {
    let fixture = fixture();
    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        StaleLockDecision::None
    );

    active_lock(&fixture, json!({"launcherPid": 4201}));
    assert_eq!(
        decide(&fixture, &probe(&SUPERVISOR_ALIVE, &LONG_AGO)),
        StaleLockDecision::None
    );
}

// rule:finalize
#[test]
fn a_dead_launcher_with_a_recorded_outcome_is_finalized_by_a_session() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_text(&run_dir_of(&fixture, RUN).join("outcome.json"), OUTCOME);

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        StaleLockDecision::Finalize {
            run_id: RUN.to_string()
        }
    );
}

#[test]
fn a_lock_owned_by_another_host_cannot_be_judged() {
    let fixture = fixture();
    active_lock(&fixture, json!({"launcherHostname": "elsewhere"}));

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        stuck("it is owned by host elsewhere, which cannot be checked from here")
    );
}

// rule:launch-interrupted
#[test]
fn a_dead_launcher_with_no_run_dir_past_the_launch_window_is_a_launch_interrupted() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        reap(
            StaleLockReapAction::LaunchInterrupted,
            "launcher pid 11096 is gone and the run never started"
        )
    );
}

// rule:launch-window
#[test]
fn a_dead_launcher_inside_the_launch_window_is_kept() {
    let fixture = fixture();
    active_lock(&fixture, json!({"reservedAt": iso(NOW - 20_000.0)}));

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        stuck("its launcher is gone but the launch window has not closed")
    );
}

// rule:processes-dead
#[test]
fn a_valid_ledger_whose_processes_are_all_gone_is_processes_dead() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_ledger(&fixture, json!({}));

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        reap(
            StaleLockReapAction::ProcessesDead,
            "its launcher, supervisor and child are all gone and it recorded no outcome"
        )
    );
}

// rule:ledger-unreadable
#[test]
fn an_unreadable_ledger_whose_recorded_processes_are_gone_is_ledger_unreadable() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_text(&run_dir_of(&fixture, RUN).join("ledger.json"), "{not json");

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        reap(
            StaleLockReapAction::LedgerUnreadable,
            "its ledger cannot be read and its recorded processes are gone"
        )
    );
}

// rule:run-process-alive
#[test]
fn a_live_supervisor_with_a_matching_start_keeps_the_lock_and_an_unverifiable_one_too() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_ledger(&fixture, json!({"processStart": "proc-start-epoch:1"}));

    assert_eq!(
        decide(
            &fixture,
            &probe_full(&SUPERVISOR_ALIVE, &RECORDED_START, &LONG_AGO)
        ),
        stuck("its supervisor pid 4201 is still running")
    );

    write_ledger(&fixture, json!({}));
    assert_eq!(
        decide(&fixture, &probe(&SUPERVISOR_ALIVE, &LONG_AGO)),
        stuck("its supervisor pid 4201 could not be checked")
    );
}

// rule:run-launching
#[test]
fn a_ledger_still_launching_before_its_hard_deadline_is_kept() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    let mut ledger = valid_ledger(RUN, json!({"launching": true}));
    let fields = ledger.as_object_mut().unwrap();
    fields.remove("pid");
    fields.remove("processStart");
    fields.remove("childPid");
    fields.remove("childProcessStart");
    fields.insert("hardDeadlineAt".to_string(), json!(NOW + 60_000.0));
    fields.insert("deadlineAt".to_string(), json!(NOW + 60_100.0));
    write_json(&run_dir_of(&fixture, RUN).join("ledger.json"), &ledger);

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        stuck(STILL_LAUNCHING)
    );
}

// rule:terminal-recorded
#[test]
fn a_run_that_recorded_a_terminal_state_only_needs_its_release() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_json(
        &run_dir_of(&fixture, RUN).join("final.json"),
        &json!({"outcome": "failed"}),
    );

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        reap(
            StaleLockReapAction::TerminalRecorded,
            "the run already recorded its terminal state"
        )
    );
}

// rule:final-folded
#[test]
fn a_folded_outcome_can_only_be_settled_by_a_session() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    for outcome in ["merged", "no_changes"] {
        write_json(
            &run_dir_of(&fixture, RUN).join("final.json"),
            &json!({"outcome": outcome}),
        );

        assert_eq!(
            decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
            stuck(
                "it finished with a folded outcome that only a session can settle; open a session"
            )
        );
    }
}

// rule:release-settled
#[test]
fn a_sentinel_for_this_generation_leaves_only_the_release_missing() {
    let fixture = fixture();
    let record = active_lock(&fixture, json!({}));
    write_json(
        &run_dir_of(&fixture, RUN).join("abandoned.json"),
        &json!({"runId": RUN, "generation": record["reservedAt"]}),
    );

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        reap(
            StaleLockReapAction::ReleaseSettled,
            "it was already settled and only the release is missing"
        )
    );
    assert!(is_settled_under_dead_launcher(
        &fixture.reflection,
        &probe(&ALL_DEAD, &LONG_AGO)
    ));
}

// rule:settled-other-generation
#[test]
fn a_sentinel_from_another_generation_is_kept() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_json(
        &run_dir_of(&fixture, RUN).join("quarantined.json"),
        &json!({"runId": RUN, "generation": iso(NOW - 3_600_000.0)}),
    );

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        stuck("its run dir holds a terminal record from another generation")
    );
    assert!(!is_settled_under_dead_launcher(
        &fixture.reflection,
        &probe(&ALL_DEAD, &LONG_AGO)
    ));
}

// rule:legacy-lock
#[test]
fn a_reservation_without_launcher_identity_is_left_to_a_session() {
    let fixture = fixture();
    write_json(
        &fixture.reflection.join("active.lock"),
        &json!({
            "runId": RUN,
            "request": {"trigger": "manual", "conversationIds": ["conversation-a"], "snapshots": []},
        }),
    );

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        stuck("its launcher identity is missing; a session reconciles it")
    );
}

// rule:lock-torn
#[test]
fn an_old_torn_lock_whose_launcher_is_gone_is_reapable() {
    let fixture = fixture();
    write_text(
        &fixture.reflection.join("active.lock"),
        r#"{"runId": "reflection-run-64", "launcherPid": 11096, "launcherHostname": "reaper-host", "req"#,
    );

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        StaleLockDecision::Reap {
            action: StaleLockReapAction::LockTorn,
            run_id: "unknown".to_string(),
            reason: "active.lock is not valid JSON".to_string(),
        }
    );
}

// rule:torn-recent
#[test]
fn a_recent_torn_lock_is_kept() {
    let fixture = fixture();
    write_text(&fixture.reflection.join("active.lock"), "{\"runId\":");

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &JUST_NOW)),
        StaleLockDecision::Stuck {
            run_id: "unknown".to_string(),
            reason: "active.lock is not valid JSON and it is recent".to_string(),
        }
    );
}

// rule:torn-other-host rule:torn-host-unknown rule:torn-launcher-alive
#[test]
fn a_torn_lock_is_kept_when_its_launcher_may_still_be_alive_or_is_not_ours() {
    let fixture = fixture();
    let cases = [
        (
            r#"{"launcherPid": 11096, "launcherHostname": "elsewhere", "x"#,
            "its launcher is on host elsewhere",
        ),
        (
            r#"{"launcherPid": 11096, "runId": "x"#,
            "its launcher host is unknown",
        ),
        (
            r#"{"launcherPid": 4201, "launcherHostname": "reaper-host", "x"#,
            "its launcher pid 4201 may still be alive",
        ),
    ];
    for (bytes, blocker) in cases {
        write_text(&fixture.reflection.join("active.lock"), bytes);

        assert_eq!(
            decide(&fixture, &probe(&SUPERVISOR_ALIVE, &LONG_AGO)),
            StaleLockDecision::Stuck {
                run_id: "unknown".to_string(),
                reason: format!("active.lock is not valid JSON and {blocker}"),
            }
        );
    }
}

// rule:torn-young-run-dir rule:torn-launching-run rule:torn-live-run
#[test]
fn a_torn_lock_is_kept_while_a_run_beside_it_may_be_live() {
    let fixture = fixture();
    write_text(
        &fixture.reflection.join("active.lock"),
        r#"{"launcherPid": 11096, "launcherHostname": "reaper-host", "x"#,
    );
    write_text(
        &fixture
            .reflection
            .join("runs/reflection-run-1/prelaunch.json"),
        "{}",
    );

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &OLD_LOCK_YOUNG_RUN_DIR)),
        StaleLockDecision::Stuck {
            run_id: "unknown".to_string(),
            reason: "active.lock is not valid JSON and run reflection-run-1 is a young run dir"
                .to_string(),
        }
    );

    let run_dir = fixture.reflection.join("runs/reflection-run-1");
    write_json(
        &run_dir.join("ledger.json"),
        &valid_ledger(
            "reflection-run-1",
            json!({"launching": true, "hardDeadlineAt": NOW + 60_000.0, "deadlineAt": NOW + 60_100.0}),
        ),
    );
    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        StaleLockDecision::Stuck {
            run_id: "unknown".to_string(),
            reason: "active.lock is not valid JSON and run reflection-run-1 is still launching"
                .to_string(),
        }
    );

    write_json(
        &run_dir.join("ledger.json"),
        &valid_ledger("reflection-run-1", json!({})),
    );
    assert_eq!(
        decide(&fixture, &probe(&SUPERVISOR_ALIVE, &LONG_AGO)),
        StaleLockDecision::Stuck {
            run_id: "unknown".to_string(),
            reason: "active.lock is not valid JSON and run reflection-run-1 may still be live"
                .to_string(),
        }
    );
}

#[test]
fn a_json_lock_that_is_not_a_reservation_is_torn() {
    let fixture = fixture();
    write_json(
        &fixture.reflection.join("active.lock"),
        &json!({"unrelated": true}),
    );

    assert_eq!(
        decide(&fixture, &probe(&ALL_DEAD, &LONG_AGO)),
        StaleLockDecision::Reap {
            action: StaleLockReapAction::LockTorn,
            run_id: "unknown".to_string(),
            reason: "active.lock is not a reservation".to_string(),
        }
    );
}

fn stranded_dir(fixture: &Fixture) -> PathBuf {
    run_dir_of(fixture, "reflection-run-7")
}

fn keep(reason: &str) -> StrandedRunDecision {
    StrandedRunDecision::Keep {
        reason: reason.to_string(),
    }
}

fn stranded_reap(action: StrandedRunAction, reason: &str) -> StrandedRunDecision {
    StrandedRunDecision::Reap {
        action,
        reason: reason.to_string(),
    }
}

// rule:stranded-young rule:stranded-no-ledger
#[test]
fn a_ledgerless_run_dir_is_reaped_only_after_the_launch_grace_window() {
    let fixture = fixture();
    let dir = stranded_dir(&fixture);
    write_text(
        &dir.join("prelaunch.json"),
        r#"{"runId":"reflection-run-7"}"#,
    );

    assert_eq!(
        decide_stranded_run(&dir, &probe(&ALL_DEAD, &JUST_NOW)),
        keep("it is inside the launch grace window")
    );
    assert_eq!(
        decide_stranded_run(&dir, &probe(&ALL_DEAD, &LONG_AGO)),
        stranded_reap(StrandedRunAction::NoLedger, "it never got a ledger")
    );
}

// rule:stranded-prelaunch-unreadable rule:stranded-prelaunch-mismatch
#[test]
fn a_prelaunch_file_that_is_unreadable_or_names_another_run_keeps_the_run() {
    let fixture = fixture();
    let dir = stranded_dir(&fixture);

    write_text(&dir.join("prelaunch.json"), "{not json");
    assert_eq!(
        decide_stranded_run(&dir, &probe(&ALL_DEAD, &LONG_AGO)),
        keep("its prelaunch.json cannot be read")
    );

    write_text(
        &dir.join("prelaunch.json"),
        r#"{"runId":"reflection-run-8"}"#,
    );
    assert_eq!(
        decide_stranded_run(&dir, &probe(&ALL_DEAD, &LONG_AGO)),
        keep("its prelaunch.json names another run")
    );
}

// rule:stranded-processes-dead rule:stranded-ledger-unreadable rule:stranded-process-alive rule:stranded-launching
#[test]
fn a_stranded_run_with_a_ledger_is_judged_by_its_recorded_processes() {
    let fixture = fixture();
    let dir = stranded_dir(&fixture);
    let ledger = |patch: Value| valid_ledger("reflection-run-7", patch);

    write_json(&dir.join("ledger.json"), &ledger(json!({})));
    assert_eq!(
        decide_stranded_run(&dir, &probe(&ALL_DEAD, &LONG_AGO)),
        stranded_reap(
            StrandedRunAction::ProcessesDead,
            "its supervisor and child are gone and it recorded no outcome"
        )
    );
    assert_eq!(
        decide_stranded_run(&dir, &probe(&SUPERVISOR_ALIVE, &LONG_AGO)),
        keep("its supervisor pid 4201 could not be checked")
    );

    write_text(&dir.join("ledger.json"), "{not json");
    assert_eq!(
        decide_stranded_run(&dir, &probe(&ALL_DEAD, &LONG_AGO)),
        stranded_reap(
            StrandedRunAction::LedgerUnreadable,
            "its ledger cannot be read and its recorded processes are gone"
        )
    );

    let mut launching = ledger(json!({"launching": true}));
    let fields = launching.as_object_mut().unwrap();
    for key in ["pid", "processStart", "childPid", "childProcessStart"] {
        fields.remove(key);
    }
    fields.insert("hardDeadlineAt".to_string(), json!(NOW + 60_000.0));
    fields.insert("deadlineAt".to_string(), json!(NOW + 60_100.0));
    write_json(&dir.join("ledger.json"), &launching);
    assert_eq!(
        decide_stranded_run(&dir, &probe(&ALL_DEAD, &LONG_AGO)),
        keep(STILL_LAUNCHING)
    );
}

#[test]
fn case_a_a_launch_that_never_started_is_abandoned_released_and_receipted() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Reaped);
    assert!(matches!(
        report.decision,
        StaleLockDecision::Reap {
            action: StaleLockReapAction::LaunchInterrupted,
            ..
        }
    ));
    assert_eq!(report.after, ReflectionLockVerdict::None);
    assert!(!fixture.reflection.join("active.lock").exists());
    let abandoned = read_json(&run_dir_of(&fixture, RUN).join("abandoned.json"));
    assert_eq!(abandoned["reason"], "launch_interrupted");
    assert_eq!(abandoned["runId"], RUN);
    assert_eq!(abandoned["kind"], "reflection");
    assert_eq!(abandoned["trigger"], "manual");
    assert_eq!(abandoned["launcher"]["pid"], 11096);
    assert_eq!(
        receipt_events(&fixture),
        vec![(
            RUN.to_string(),
            "abandoned".to_string(),
            Some("launch_interrupted".to_string())
        )]
    );
}

#[test]
fn case_b_dead_processes_are_abandoned_and_the_pending_run_stays_byte_identical() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    pending_run(&fixture);
    let pending_before = std::fs::read(fixture.reflection.join("pending.json")).unwrap();
    write_ledger(&fixture, json!({}));
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Reaped);
    assert_eq!(report.pending_kept.as_deref(), Some("reflection-run-65"));
    let run_dir = run_dir_of(&fixture, RUN);
    let abandoned = read_json(&run_dir.join("abandoned.json"));
    assert_eq!(abandoned["reason"], "processes_dead");
    assert_eq!(
        abandoned["generation"],
        valid_ledger(RUN, json!({}))["startedAt"]
    );
    let claim = read_json(&run_dir.join("terminal-claim.json"));
    assert_eq!(claim["kind"], "abandon");
    assert_eq!(claim["runId"], RUN);
    assert_eq!(
        receipt_events(&fixture),
        vec![(
            RUN.to_string(),
            "abandoned".to_string(),
            Some("processes_dead".to_string())
        )]
    );
    assert!(!fixture.reflection.join("active.lock").exists());
    assert_eq!(
        std::fs::read(fixture.reflection.join("pending.json")).unwrap(),
        pending_before
    );
}

#[test]
fn an_unreadable_ledger_is_quarantined_with_the_reservation_kept_as_evidence() {
    let fixture = fixture();
    let record = active_lock(&fixture, json!({}));
    write_text(&run_dir_of(&fixture, RUN).join("ledger.json"), "{not json");
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Reaped);
    let run_dir = run_dir_of(&fixture, RUN);
    let quarantined = read_json(&run_dir.join("quarantined.json"));
    assert_eq!(quarantined["reason"], "ledger_unreadable");
    assert_eq!(
        quarantined["evidence"],
        json!([
            "ledger.json",
            "terminal-claim.json",
            "reservation.quarantined.json"
        ])
    );
    assert_eq!(
        read_json(&run_dir.join("reservation.quarantined.json")),
        record
    );
    assert_eq!(
        receipt_events(&fixture),
        vec![(
            RUN.to_string(),
            "quarantined".to_string(),
            Some("ledger_unreadable".to_string())
        )]
    );
}

#[test]
fn a_torn_lock_is_moved_aside_byte_for_byte_and_the_run_id_high_water_survives() {
    let fixture = fixture();
    let torn = r#"{"runId": "reflection-run-64", "launcherPid": 11096, "launcherHostname": "reaper-host", "req"#;
    write_text(&fixture.reflection.join("active.lock"), torn);
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Reaped);
    assert_eq!(report.after, ReflectionLockVerdict::None);
    assert!(!fixture.reflection.join("active.lock").exists());
    let moved: Vec<String> = std::fs::read_dir(&fixture.reflection)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("active.lock.torn-"))
        .collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.reflection.join(&moved[0])).unwrap(),
        torn
    );
    assert_eq!(
        crate::reflection::run_id_high_water::read_reflection_run_id_mark(&fixture.reflection),
        Some(64)
    );
    assert_eq!(
        receipt_events(&fixture),
        vec![(
            "reflection-run-64".to_string(),
            "abandoned".to_string(),
            Some("lock_torn".to_string())
        )]
    );
}

#[test]
fn a_dry_run_reports_what_it_would_do_and_writes_nothing() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    pending_run(&fixture);
    write_ledger(&fixture, json!({}));
    write_text(&fixture.reflection.join("runs/.prune-old/leftover"), "x");
    let before = tree(&fixture.runtime);
    let mut options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));
    options.dry_run = true;

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::WouldReap);
    assert_eq!(report.pending_kept.as_deref(), Some("reflection-run-65"));
    assert!(report.dry_run);
    assert_eq!(
        report.tombstones,
        vec![TombstoneReport {
            name: ".prune-old".to_string(),
            removed: false,
            error: None,
        }]
    );
    assert_eq!(tree(&fixture.runtime), before);
    let lines = format_stale_lock_fix_lines("agent-a", &report, None);
    assert_eq!(
        lines[0],
        "WOULD FIX agent-a active.lock run reflection-run-64 (processes-dead)"
    );
    assert_eq!(
        lines[1],
        "INFO agent-a pending reflection-run-65 stays queued for the next session"
    );
}

#[test]
fn a_session_holding_the_finalize_lock_makes_the_cli_keep_the_run_and_write_nothing() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_ledger(&fixture, json!({}));
    let before = tree(&fixture.reflection);
    let held = create_lock_record(
        "reflection-finalize",
        CreateLockRecordOptions {
            run_id: Some(RUN.to_string()),
        },
    )
    .unwrap();
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = with_lock(
        &run_finalization_lock_path(&fixture.locks, RUN).unwrap(),
        &held,
        &AcquireLockOptions {
            wait_timeout_ms: Some(0),
            ..Default::default()
        },
        || Ok::<_, String>(run_stale_lock_reaper(&options)),
    )
    .unwrap()
    .unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Kept);
    assert_eq!(
        report.kept_reason.as_deref(),
        Some("a session is finalizing it")
    );
    assert_eq!(tree(&fixture.reflection), before);
}

#[test]
fn a_run_publishing_its_outcome_is_never_claimed_or_released() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_ledger(&fixture, json!({"attempt": 1}));
    write_json(
        &run_dir_of(&fixture, RUN).join("publishing.json"),
        &json!({"version": 1, "runId": RUN, "attempt": 1, "finishedAt": iso(NOW)}),
    );
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Kept);
    assert_eq!(
        report.kept_reason.as_deref(),
        Some("the run is already publishing its outcome")
    );
    let run_dir = run_dir_of(&fixture, RUN);
    assert!(!run_dir.join("terminal-claim.json").exists());
    assert!(!run_dir.join("abandoned.json").exists());
    assert!(fixture.reflection.join("active.lock").exists());
}

#[test]
fn a_live_publisher_that_already_claimed_the_run_keeps_it() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_ledger(&fixture, json!({}));
    write_json(
        &run_dir_of(&fixture, RUN).join("terminal-claim.json"),
        &json!({"version": 1, "kind": "publish", "runId": RUN, "attempt": 1, "claimantPid": 9191}),
    );
    let options = reaper(&fixture, probe(&CLAIMANT_ALIVE, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Kept);
    assert_eq!(
        report.kept_reason.as_deref(),
        Some("another process claimed the run")
    );
    assert!(!run_dir_of(&fixture, RUN).join("abandoned.json").exists());
}

#[test]
fn an_earlier_settle_that_stopped_before_releasing_only_needs_the_release() {
    let fixture = fixture();
    let record = active_lock(&fixture, json!({}));
    pending_run(&fixture);
    let pending_before = std::fs::read(fixture.reflection.join("pending.json")).unwrap();
    write_ledger(&fixture, json!({}));
    write_json(
        &run_dir_of(&fixture, RUN).join("abandoned.json"),
        &json!({"runId": RUN, "generation": record["reservedAt"], "reason": "processes_dead"}),
    );
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Reaped);
    assert!(matches!(
        report.decision,
        StaleLockDecision::Reap {
            action: StaleLockReapAction::ReleaseSettled,
            ..
        }
    ));
    assert!(!fixture.reflection.join("active.lock").exists());
    assert_eq!(
        std::fs::read(fixture.reflection.join("pending.json")).unwrap(),
        pending_before
    );
    assert!(receipt_events(&fixture).is_empty());
}

#[test]
fn a_dead_holder_with_an_outcome_is_left_for_a_session_to_finalize() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_text(&run_dir_of(&fixture, RUN).join("outcome.json"), OUTCOME);
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Kept);
    assert_eq!(
        report.kept_reason.as_deref(),
        Some("it recorded its outcome; open a session to finalize it")
    );
    assert!(fixture.reflection.join("active.lock").exists());
    let lines = format_stale_lock_fix_lines("agent-a", &report, None);
    assert_eq!(
        lines,
        vec![
            "KEPT agent-a active.lock run reflection-run-64 (it recorded its outcome; open a session to finalize it)"
                .to_string()
        ]
    );
}

#[test]
fn an_in_session_settle_runs_instead_of_the_minimal_writers() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_ledger(&fixture, json!({}));
    let calls = AtomicUsize::new(0);
    let settle = || -> Result<(), String> {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let mut options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));
    options.settle = Some(&settle);

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Settled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(fixture.reflection.join("active.lock").exists());
    assert!(report.stranded.is_empty());
    assert!(!run_dir_of(&fixture, RUN).join("abandoned.json").exists());
}

#[test]
fn a_session_that_already_tried_the_run_declines_to_settle_it_again() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_ledger(&fixture, json!({}));
    let settle = || -> Result<(), String> { panic!("must not settle") };
    let decline = |_: &str| false;
    let mut options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));
    options.settle = Some(&settle);
    options.should_settle = Some(&decline);

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::SettleDeclined);
}

#[test]
fn session_start_settles_even_when_the_lock_needs_nothing() {
    let fixture = fixture();
    let calls = AtomicUsize::new(0);
    let settle = || -> Result<(), String> {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let mut options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));
    options.settle = Some(&settle);
    options.settle_always = true;

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failing_settle_is_reported_to_the_caller() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    write_ledger(&fixture, json!({}));
    let settle = || -> Result<(), String> { Err("reconcile failed".to_string()) };
    let mut options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));
    options.settle = Some(&settle);

    assert_eq!(
        run_stale_lock_reaper(&options).unwrap_err(),
        "reconcile failed"
    );
}

#[test]
fn stranded_runs_are_settled_by_the_cli_and_the_reserved_run_is_left_alone() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    let stranded = run_dir_of(&fixture, "reflection-run-7");
    write_json(
        &stranded.join("ledger.json"),
        &valid_ledger("reflection-run-7", json!({})),
    );
    let young = run_dir_of(&fixture, "reflection-run-8");
    write_text(
        &young.join("prelaunch.json"),
        r#"{"runId":"reflection-run-8"}"#,
    );
    write_json(
        &run_dir_of(&fixture, RUN).join("ledger.json"),
        &valid_ledger(RUN, json!({})),
    );
    let options = reaper(&fixture, probe(&LAUNCHER_ALIVE, &LONG_AGO_EXCEPT_RUN_8));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::None);
    let by_run: BTreeMap<&str, (&StrandedRunDecision, bool)> = report
        .stranded
        .iter()
        .map(|entry| (entry.run_id.as_str(), (&entry.decision, entry.applied)))
        .collect();
    assert_eq!(by_run.len(), 2);
    assert!(matches!(
        by_run["reflection-run-7"],
        (
            StrandedRunDecision::Reap {
                action: StrandedRunAction::ProcessesDead,
                ..
            },
            true
        )
    ));
    assert!(matches!(
        by_run["reflection-run-8"],
        (StrandedRunDecision::Keep { .. }, false)
    ));
    assert!(stranded.join("abandoned.json").exists());
    assert!(!young.join("quarantined.json").exists());
    assert!(!run_dir_of(&fixture, RUN).join("abandoned.json").exists());
    let lines = format_stale_lock_fix_lines("agent-a", &report, None);
    assert!(lines.contains(&"FIXED agent-a run reflection-run-7 (processes-dead)".to_string()));
}

fn long_ago_except_run_8(path: &Path) -> Option<f64> {
    if path.to_string_lossy().contains("reflection-run-8") {
        Some(NOW - 1_000.0)
    } else {
        Some(NOW - TWO_HOURS_MS)
    }
}

#[test]
fn a_run_dir_that_never_got_a_ledger_is_quarantined_by_the_shared_writer() {
    let fixture = fixture();
    let stranded = run_dir_of(&fixture, "reflection-run-9");
    write_text(
        &stranded.join("prelaunch.json"),
        r#"{"runId":"reflection-run-9"}"#,
    );

    let settled = quarantine_stranded_run(&QuarantineStrandedRunOptions {
        runtime_dir: &fixture.runtime,
        run_dir: &stranded,
        run_id: "reflection-run-9",
        action: StrandedRunAction::NoLedger,
        now: Some(&NOW_FN),
        append_receipt: None,
    })
    .unwrap();

    assert!(settled);
    let quarantined = read_json(&stranded.join("quarantined.json"));
    assert_eq!(quarantined["reason"], "stranded_no_ledger");
    assert_eq!(quarantined["quarantinedAt"], iso(NOW));
    assert_eq!(
        receipt_events(&fixture),
        vec![(
            "reflection-run-9".to_string(),
            "quarantined".to_string(),
            Some("stranded_no_ledger".to_string())
        )]
    );

    let again = quarantine_stranded_run(&QuarantineStrandedRunOptions {
        runtime_dir: &fixture.runtime,
        run_dir: &stranded,
        run_id: "reflection-run-9",
        action: StrandedRunAction::NoLedger,
        now: Some(&NOW_FN),
        append_receipt: None,
    })
    .unwrap();
    assert!(
        !again,
        "a run that already has a sentinel is not settled twice"
    );
}

#[test]
fn tombstones_left_by_an_interrupted_prune_are_removed() {
    let fixture = fixture();
    write_text(
        &fixture.reflection.join("runs/.prune-a/leftover.json"),
        "{}",
    );
    write_text(
        &fixture.reflection.join("runs/.prune-b/leftover.json"),
        "{}",
    );
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(
        report
            .tombstones
            .iter()
            .map(|tombstone| (tombstone.name.as_str(), tombstone.removed))
            .collect::<Vec<_>>(),
        vec![(".prune-a", true), (".prune-b", true)]
    );
    assert!(!fixture.reflection.join("runs/.prune-a").exists());
    let lines = format_stale_lock_fix_lines("agent-a", &report, None);
    assert!(lines.contains(&"FIXED agent-a removed tombstone .prune-a".to_string()));
}

#[test]
fn a_receipt_that_cannot_be_written_is_reported_as_a_warning_not_a_failure() {
    let fixture = fixture();
    active_lock(&fixture, json!({}));
    let append =
        |_: &Path, _: &MemoryReceiptInput| -> Result<(), String> { Err("disk full".to_string()) };
    let mut options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));
    options.append_receipt = Some(&append);

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Reaped);
    assert_eq!(
        report.warnings,
        vec![format!("receipt for {RUN} not written: disk full")]
    );
    let lines = format_stale_lock_fix_lines("agent-a", &report, None);
    assert_eq!(
        lines.last().map(String::as_str),
        Some("WARN agent-a receipt for reflection-run-64 not written: disk full")
    );
}

#[test]
fn locks_whose_owner_is_gone_are_reclaimed_and_the_rest_reported() {
    let fixture = fixture();
    let owner = |pid: u32| {
        let mut record: LockRecord =
            create_lock_record("dead-owner-test", CreateLockRecordOptions { run_id: None })
                .unwrap();
        record.pid = pid;
        format!("{}\n", serde_json::to_string(&record).unwrap())
    };
    write_text(&fixture.locks.join("gone.lock"), &owner(2_000_000_000));
    write_text(&fixture.locks.join("live.lock"), &owner(std::process::id()));

    let report = reclaim_dead_owner_locks(&fixture.locks);

    assert_eq!(report.reclaimed, vec!["gone.lock".to_string()]);
    assert_eq!(
        report.kept,
        vec![(
            "live.lock".to_string(),
            "owner is alive or not provably dead".to_string()
        )]
    );
    assert!(!fixture.locks.join("gone.lock").exists());
    let lines =
        format_stale_lock_fix_lines("agent-a", &stale_report_without_findings(), Some(&report));
    assert_eq!(
        lines,
        vec!["INFO agent-a locks reclaimed 1, kept 1".to_string()]
    );
}

fn stale_report_without_findings() -> StaleLockReaperReport {
    StaleLockReaperReport {
        before: ReflectionLockVerdict::None,
        decision: StaleLockDecision::None,
        action: StaleLockReaperAction::None,
        after: ReflectionLockVerdict::None,
        stranded: Vec::new(),
        tombstones: Vec::new(),
        warnings: Vec::new(),
        dry_run: false,
        pending_kept: None,
        kept_reason: None,
    }
}

#[test]
fn a_stuck_holder_is_reported_with_its_reason() {
    let fixture = fixture();
    active_lock(&fixture, json!({"launcherHostname": "elsewhere"}));
    let options = reaper(&fixture, probe(&ALL_DEAD, &LONG_AGO));

    let report = run_stale_lock_reaper(&options).unwrap();

    assert_eq!(report.action, StaleLockReaperAction::Kept);
    assert_eq!(
        format_stale_lock_fix_lines("agent-a", &report, None),
        vec![
            "KEPT agent-a active.lock run reflection-run-64 (it is owned by host elsewhere, which cannot be checked from here)"
                .to_string()
        ]
    );
}

#[test]
fn every_reap_action_has_its_own_stable_name() {
    let names: Vec<&str> = [
        StaleLockReapAction::LaunchInterrupted,
        StaleLockReapAction::ProcessesDead,
        StaleLockReapAction::LedgerUnreadable,
        StaleLockReapAction::LockTorn,
        StaleLockReapAction::TerminalRecorded,
        StaleLockReapAction::ReleaseSettled,
    ]
    .iter()
    .map(|action| action.as_str())
    .collect();
    assert_eq!(
        names,
        vec![
            "launch-interrupted",
            "processes-dead",
            "ledger-unreadable",
            "lock-torn",
            "terminal-recorded",
            "release-settled"
        ]
    );
}
