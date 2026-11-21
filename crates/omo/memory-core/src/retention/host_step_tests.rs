use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{
    ForeignMemoryRuntimeSettings, RetentionHostSweepStepOptions, create_retention_host_sweep_step,
    resolve_foreign_identity_discard_unreflected,
};
use crate::identity::layout::{MemoryIdentityPaths, build_identity_paths};
use crate::journal::{TranscriptJournal, TranscriptJournalOptions, TranscriptProjection};
use crate::locks::{
    AcquireLockOptions, CreateLockRecordOptions, acquire_lock, create_lock_record, release_lock,
};
use crate::reflection::host_sweep::{
    HOST_SWEEP_LOCK_FILE, HOST_SWEEP_STATE_FILE, HostSweepOptions, HostSweepResult, HostSweepState,
    HostSweepStep, read_host_sweep_state, sweep_host_identities,
};
use crate::retention::RETENTION_LOG_FILE;
use crate::support::time::format_rfc3339_millis;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 1_793_836_800_000;

fn fixed_now() -> i64 {
    NOW
}

fn fixed_clock() -> u64 {
    u64::try_from(NOW).unwrap()
}

static NOW_FN: fn() -> i64 = fixed_now;
static CLOCK_FN: fn() -> u64 = fixed_clock;

fn days_ago(days: i64) -> i64 {
    NOW - days * DAY_MS
}

fn iso(ms: i64) -> String {
    format_rfc3339_millis(ms)
}

fn settings(value: Value) -> ForeignMemoryRuntimeSettings {
    ForeignMemoryRuntimeSettings::from_json(&value)
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{value:#}\n")).unwrap();
}

fn age_tree(path: &Path, at: i64) {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            age_tree(&entry.unwrap().path(), at);
        }
    }
    let seconds = at.div_euclid(1000);
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(seconds, 0)).unwrap();
}

fn reflection_run(paths: &MemoryIdentityPaths, run_id: &str, finished_at: i64) {
    let dir = paths.reflection.join("runs").join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    write_json(&dir.join("ledger.json"), &json!({ "runId": run_id }));
    std::fs::write(dir.join("child-1.log"), "child output\n").unwrap();
    write_json(
        &dir.join("final.json"),
        &json!({ "runId": run_id, "outcome": "merged", "finishedAt": iso(finished_at) }),
    );
    age_tree(&dir, finished_at);
}

fn completion(paths: &MemoryIdentityPaths, run_id: &str, outcome: &str, finished_at: i64) {
    let path = paths
        .reflection
        .join("completions")
        .join(format!("{run_id}.json"));
    write_json(
        &path,
        &json!({
            "schemaVersion": 1,
            "runId": run_id,
            "identity": "agent-test",
            "category": "quick",
            "conversationIds": ["conversation-a"],
            "trigger": "manual",
            "outcome": outcome,
            "startedAt": iso(finished_at - 60_000),
            "finishedAt": iso(finished_at),
            "durationMs": 60_000,
            "consecutiveFailures": 0,
            "delivery": { "status": "consumed" },
        }),
    );
    age_tree(&path, finished_at);
}

fn never_journal(paths: &MemoryIdentityPaths, session_id: &str, idle_since: i64) -> PathBuf {
    let dir = paths.transcripts.join(session_id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut options = TranscriptJournalOptions::new(dir.as_path());
    options.now = Some(Box::new(move || iso(idle_since)));
    let store = TranscriptJournal::new(options);
    store
        .reconcile(&[
            TranscriptProjection::User {
                message_id: "u1".to_string(),
                text: "first".to_string(),
            },
            TranscriptProjection::Assistant {
                message_id: "a1".to_string(),
                text_blocks: vec!["first answer".to_string()],
                reasoning_blocks: Vec::new(),
                tool_calls: Vec::new(),
            },
        ])
        .unwrap();
    age_tree(&dir, idle_since);
    dir
}

fn paths_of(memory_root: &Path, name: &str) -> MemoryIdentityPaths {
    build_identity_paths(memory_root, name)
}

fn run_present(memory_root: &Path, name: &str, run_id: &str) -> bool {
    paths_of(memory_root, name)
        .reflection
        .join("runs")
        .join(run_id)
        .exists()
}

fn journal_present(memory_root: &Path, name: &str, session_id: &str) -> bool {
    paths_of(memory_root, name)
        .transcripts
        .join(session_id)
        .join("transcript.jsonl")
        .exists()
}

fn swept(result: HostSweepResult) -> (Vec<String>, bool, Vec<String>) {
    match result {
        HostSweepResult::Swept {
            visited,
            completed,
            lines,
        } => (visited, completed, lines),
        HostSweepResult::Busy => panic!("expected a sweep, got busy"),
    }
}

fn sweep<'a>(
    memory_root: &'a Path,
    steps: Vec<HostSweepStep<'a>>,
    max_identities: Option<usize>,
    dry_run: bool,
) -> HostSweepResult {
    let options = HostSweepOptions {
        steps,
        max_identities,
        now: Some(&CLOCK_FN),
        dry_run,
        ..HostSweepOptions::new(memory_root)
    };
    sweep_host_identities(options).unwrap()
}

fn step<'a>(options: RetentionHostSweepStepOptions<'a>) -> Vec<HostSweepStep<'a>> {
    vec![create_retention_host_sweep_step(options)]
}

const NAMES: [&str; 30] = [
    "id-01", "id-02", "id-03", "id-04", "id-05", "id-06", "id-07", "id-08", "id-09", "id-10",
    "id-11", "id-12", "id-13", "id-14", "id-15", "id-16", "id-17", "id-18", "id-19", "id-20",
    "id-21", "id-22", "id-23", "id-24", "id-25", "id-26", "id-27", "id-28", "id-29", "id-30",
];

const REPO_LESS: [&str; 3] = ["id-03", "id-07", "id-11"];

fn names() -> Vec<String> {
    NAMES.iter().map(|name| (*name).to_string()).collect()
}

fn host_settings() -> ForeignMemoryRuntimeSettings {
    settings(json!({
        "runtime": { "discard_unreflected_journals": true },
        "agents": { "id-14": { "runtime": { "discard_unreflected_journals": false } } },
    }))
}

fn temp_root() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let memory_root = root.path().to_path_buf();
    (root, memory_root)
}

fn lone_identity(memory_root: &Path, name: &str) -> MemoryIdentityPaths {
    let paths = paths_of(memory_root, name);
    std::fs::create_dir_all(&paths.repo).unwrap();
    std::fs::create_dir_all(&paths.runtime).unwrap();
    paths
}

fn host() -> (tempfile::TempDir, PathBuf) {
    let (root, memory_root) = temp_root();
    for name in NAMES {
        let paths = paths_of(&memory_root, name);
        std::fs::create_dir_all(&paths.runtime).unwrap();
        if !REPO_LESS.contains(&name) {
            std::fs::create_dir_all(&paths.repo).unwrap();
        }
    }
    for name in ["id-05", "id-20", "id-07"] {
        let paths = paths_of(&memory_root, name);
        reflection_run(&paths, "reflection-run-1", days_ago(8));
        reflection_run(&paths, "reflection-run-9", days_ago(1));
    }
    let failing = paths_of(&memory_root, "id-12");
    completion(&failing, "reflection-run-1", "failed", days_ago(2));
    never_journal(&failing, "s-never", days_ago(15));
    never_journal(&paths_of(&memory_root, "id-14"), "s-never", days_ago(60));
    never_journal(&paths_of(&memory_root, "id-15"), "s-never", days_ago(60));
    (root, memory_root)
}

#[test]
fn given_30_identities_and_a_bounded_driver_of_25_when_it_runs_twice_then_it_visits_25_then_5_in_sorted_order_prunes_repo_less_and_repo_identities_alike_and_honours_the_gate_and_the_per_identity_switch()
 {
    let (_root, memory_root) = host();

    let first = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            settings: Some(host_settings()),
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        Some(25),
        false,
    ));
    assert_eq!(first.0, names()[..25].to_vec());
    assert!(!first.1);
    assert_eq!(
        serde_json::from_str::<Value>(
            &std::fs::read_to_string(memory_root.join(HOST_SWEEP_STATE_FILE)).unwrap()
        )
        .unwrap(),
        json!({ "version": 1, "cursor": "id-25" })
    );
    assert!(!run_present(&memory_root, "id-05", "reflection-run-1"));
    assert!(!run_present(&memory_root, "id-07", "reflection-run-1"));
    assert!(!run_present(&memory_root, "id-20", "reflection-run-1"));
    assert!(run_present(&memory_root, "id-20", "reflection-run-9"));
    assert!(journal_present(&memory_root, "id-12", "s-never"));
    assert!(journal_present(&memory_root, "id-14", "s-never"));
    assert!(!journal_present(&memory_root, "id-15", "s-never"));
    assert!(
        first
            .2
            .iter()
            .any(|line| line.starts_with("FIXED id-20 retention removed 1"))
    );

    let second = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            settings: Some(host_settings()),
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        Some(25),
        false,
    ));
    assert_eq!(second.0, names()[25..].to_vec());
    assert!(second.1);
    assert_eq!(
        read_host_sweep_state(&memory_root),
        HostSweepState {
            cursor: None,
            completed_at: Some(iso(NOW)),
        }
    );
}

#[test]
fn given_another_driver_holds_runtime_sweep_lock_when_a_second_driver_starts_then_it_returns_busy_and_removes_nothing()
 {
    let (_root, memory_root) = host();
    let lock_path = memory_root.join(HOST_SWEEP_LOCK_FILE);
    let holder = create_lock_record("runtime-sweep", CreateLockRecordOptions::default()).unwrap();
    acquire_lock(&lock_path, &holder, &AcquireLockOptions::default()).unwrap();
    let result = sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            settings: Some(host_settings()),
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        Some(25),
        false,
    );
    release_lock(&lock_path, &holder).unwrap();

    assert_eq!(result, HostSweepResult::Busy);
    assert!(run_present(&memory_root, "id-05", "reflection-run-1"));
}

#[test]
fn given_dry_run_when_the_step_runs_then_it_lists_what_each_identitys_pass_would_remove_and_changes_nothing()
 {
    let (_root, memory_root) = host();

    let result = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            settings: Some(host_settings()),
            now: Some(&NOW_FN),
            dry_run: true,
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        true,
    ));

    assert_eq!(result.0.len(), 30);
    assert_eq!(
        result.2,
        vec![
            "WOULD FIX id-05 retention would remove 1 (runs 1, completions 0, journals 0, sidecars 0, ledgers 0, legacy 0)".to_string(),
            "WOULD FIX id-07 retention would remove 1 (runs 1, completions 0, journals 0, sidecars 0, ledgers 0, legacy 0)".to_string(),
            "WOULD FIX id-15 retention would remove 1 (runs 0, completions 0, journals 1, sidecars 0, ledgers 0, legacy 0)".to_string(),
            "WOULD FIX id-20 retention would remove 1 (runs 1, completions 0, journals 0, sidecars 0, ledgers 0, legacy 0)".to_string(),
        ]
    );
    for name in ["id-05", "id-07", "id-20"] {
        let paths = paths_of(&memory_root, name);
        assert!(run_present(&memory_root, name, "reflection-run-1"));
        assert!(!paths.runtime.join(RETENTION_LOG_FILE).exists());
        assert!(!paths.reflection.join("run-id.json").exists());
    }
    assert!(journal_present(&memory_root, "id-15", "s-never"));
}

#[test]
fn given_an_identity_with_something_past_retention_in_every_store_when_dry_run_then_fix_run_then_the_preview_names_as_many_items_as_the_pass_removes()
 {
    let (_root, memory_root) = temp_root();
    let paths = lone_identity(&memory_root, "id-mix");
    reflection_run(&paths, "reflection-run-60", days_ago(8));
    reflection_run(&paths, "reflection-run-61", days_ago(1));
    for index in 1..=51 {
        completion(
            &paths,
            &format!("reflection-run-{index}"),
            "merged",
            days_ago(90 - index),
        );
    }
    let ledger = paths.recall_ledger.join("s-old.json");
    write_json(&ledger, &json!({ "surfaced": [] }));
    age_tree(&ledger, days_ago(60));
    let session = paths.reflection_sessions.join("reflection-run-0");
    std::fs::create_dir_all(&session).unwrap();
    age_tree(&session, days_ago(20));
    let recall_runs = paths.recall.join("runs").join("r-1");
    std::fs::create_dir_all(&recall_runs).unwrap();
    age_tree(&paths.recall.join("runs"), days_ago(20));

    let dry = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            now: Some(&NOW_FN),
            dry_run: true,
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        true,
    ));
    let fixed = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        false,
    ));

    assert_eq!(
        dry.2,
        vec![
            "WOULD FIX id-mix retention would remove 5 (runs 1, completions 1, journals 0, sidecars 0, ledgers 1, legacy 2)".to_string()
        ]
    );
    assert!(fixed.2[0].starts_with("FIXED id-mix retention removed 5 "));
}

#[test]
fn given_a_folded_run_past_retention_whose_consumed_completion_is_outside_the_newest_50_when_dry_run_then_fix_run_then_the_preview_also_counts_the_completion_the_pass_removes_once_the_run_is_gone()
 {
    let (_root, memory_root) = temp_root();
    let paths = lone_identity(&memory_root, "id-rp");
    reflection_run(&paths, "reflection-run-1", days_ago(40));
    reflection_run(&paths, "reflection-run-60", days_ago(1));
    for index in 1..=51 {
        completion(
            &paths,
            &format!("reflection-run-{index}"),
            "merged",
            days_ago(90 - index),
        );
    }

    let dry = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            now: Some(&NOW_FN),
            dry_run: true,
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        true,
    ));
    let fixed = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        false,
    ));

    assert_eq!(
        dry.2,
        vec![
            "WOULD FIX id-rp retention would remove 2 (runs 1, completions 1, journals 0, sidecars 0, ledgers 0, legacy 0)".to_string()
        ]
    );
    assert!(fixed.2[0].starts_with("FIXED id-rp retention removed 2 "));
}

#[test]
fn given_dry_run_and_an_identity_over_its_size_cap_when_the_step_runs_then_it_also_says_what_the_cap_would_evict()
 {
    let (_root, memory_root) = temp_root();
    let paths = lone_identity(&memory_root, "id-big");
    reflection_run(&paths, "reflection-run-1", days_ago(8));
    reflection_run(&paths, "reflection-run-9", days_ago(1));

    let result = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            settings: Some(settings(json!({ "runtime": { "max_bytes": 1 } }))),
            now: Some(&NOW_FN),
            dry_run: true,
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        true,
    ));

    assert_eq!(
        result.2[0],
        "WOULD FIX id-big retention would remove 1 (runs 1, completions 0, journals 0, sidecars 0, ledgers 0, legacy 0)"
    );
    assert_eq!(result.2.len(), 2);
    assert!(
        result.2[1].starts_with("WOULD FIX id-big size cap: cap exceeded by "),
        "{}",
        result.2[1]
    );
    assert!(run_present(&memory_root, "id-big", "reflection-run-1"));
}

#[test]
fn given_global_retention_days_30_and_an_agent_override_of_3_when_the_host_sweep_step_visits_then_each_identity_keeps_the_windows_its_global_layer_sets()
 {
    let (_root, memory_root) = temp_root();
    let wide = lone_identity(&memory_root, "id-wide");
    let narrow = lone_identity(&memory_root, "id-narrow");
    for paths in [&wide, &narrow] {
        reflection_run(paths, "reflection-run-1", days_ago(10));
        reflection_run(paths, "reflection-run-9", days_ago(1));
    }

    swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            settings: Some(settings(json!({
                "runtime": { "retention_days": 30 },
                "agents": { "id-narrow": { "runtime": { "retention_days": 3 } } },
            }))),
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        false,
    ));

    assert!(
        wide.reflection
            .join("runs")
            .join("reflection-run-1")
            .exists()
    );
    assert!(
        !narrow
            .reflection
            .join("runs")
            .join("reflection-run-1")
            .exists()
    );
}

#[test]
fn given_an_identity_this_process_hosts_when_the_host_sweep_step_visits_it_then_the_step_skips_it_and_leaves_its_own_passs_decisions_alone()
 {
    let (_root, memory_root) = temp_root();
    let hosted = lone_identity(&memory_root, "id-hosted");
    let foreign = lone_identity(&memory_root, "id-foreign");
    for paths in [&hosted, &foreign] {
        reflection_run(paths, "reflection-run-1", days_ago(8));
        reflection_run(paths, "reflection-run-9", days_ago(1));
    }
    let is_hosted = |name: &str| name == "id-hosted";

    let result = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            is_hosted_identity: Some(&is_hosted),
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        false,
    ));

    assert!(result.0.iter().any(|name| name == "id-hosted"));
    assert!(
        hosted
            .reflection
            .join("runs")
            .join("reflection-run-1")
            .exists()
    );
    assert!(
        !foreign
            .reflection
            .join("runs")
            .join("reflection-run-1")
            .exists()
    );
}

#[test]
fn given_a_global_max_bytes_and_an_agent_override_of_0_when_the_host_sweep_step_visits_then_the_over_cap_identity_loses_its_oldest_folded_runs_and_the_unbounded_one_keeps_all()
 {
    let (_root, memory_root) = temp_root();
    let capped = lone_identity(&memory_root, "id-capped");
    let unbounded = lone_identity(&memory_root, "id-unbounded");
    for paths in [&capped, &unbounded] {
        for index in [1, 2, 3] {
            reflection_run(
                paths,
                &format!("reflection-run-{index}"),
                days_ago(4 - index),
            );
        }
    }

    let result = swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            settings: Some(settings(json!({
                "runtime": { "max_bytes": 1 },
                "agents": { "id-unbounded": { "runtime": { "max_bytes": 0 } } },
            }))),
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        false,
    ));

    let present = |paths: &MemoryIdentityPaths| -> Vec<bool> {
        [1, 2, 3]
            .into_iter()
            .map(|index| {
                paths
                    .reflection
                    .join("runs")
                    .join(format!("reflection-run-{index}"))
                    .exists()
            })
            .collect()
    };
    assert_eq!(present(&capped), vec![false, false, true]);
    assert_eq!(present(&unbounded), vec![true, true, true]);
    assert!(
        result
            .2
            .iter()
            .any(|line| line.starts_with("FIXED id-capped retention removed 2"))
    );
}

#[test]
fn given_max_bytes_forcing_the_journal_tiers_the_switch_on_and_retention_days_7_when_the_host_sweep_step_visits_then_the_caps_journal_floor_is_retention_days_so_only_the_journal_idle_past_it_goes()
 {
    let (_root, memory_root) = temp_root();
    let paths = lone_identity(&memory_root, "id-floor");
    let old = never_journal(&paths, "j-old", days_ago(10));
    let young = never_journal(&paths, "j-young", days_ago(3));

    swept(sweep(
        &memory_root,
        step(RetentionHostSweepStepOptions {
            settings: Some(settings(json!({
                "runtime": {
                    "max_bytes": 1,
                    "retention_days": 7,
                    "abandoned_journal_days": 14,
                    "discard_unreflected_journals": true,
                },
            }))),
            now: Some(&NOW_FN),
            ..RetentionHostSweepStepOptions::default()
        }),
        None,
        false,
    ));

    assert!(!old.exists());
    assert!(young.exists());
}

#[test]
fn given_global_settings_when_the_foreign_identity_switch_resolves_then_only_global_on_plus_no_override_off_counts_as_on()
 {
    let host_settings = host_settings();

    assert!(!resolve_foreign_identity_discard_unreflected(
        Some(&host_settings),
        "id-14"
    ));
    assert!(resolve_foreign_identity_discard_unreflected(
        Some(&host_settings),
        "id-15"
    ));
    let agent_only = settings(json!({
        "agents": { "id-15": { "runtime": { "discard_unreflected_journals": true } } },
    }));
    assert!(!resolve_foreign_identity_discard_unreflected(
        Some(&agent_only),
        "id-15"
    ));
    assert!(!resolve_foreign_identity_discard_unreflected(None, "id-15"));
}
