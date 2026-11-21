//! Port of `retention/cap.test.ts` (omo #12538): the runtime size cap's eviction order, its crash
//! windows and claims, the journal floor, reservations, the health gate, the
//! `discard_unreflected_journals` switch, the pass budget and cursor, the doctors' evictable
//! measure, and retention followed by the cap in one pass.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{
    RuntimeCapEvictableOptions, RuntimeCapKeepReason, RuntimeCapTier, RuntimeSizeCapOptions,
    RuntimeSizeCapReport, measure_runtime_cap_evictable_bytes, run_runtime_size_cap,
};
use crate::identity::layout::MemoryIdentityPaths;
use crate::locks::{
    AcquireLockOptions, CreateLockRecordOptions, acquire_lock, create_lock_record,
    reflection_scheduler_lock_path, release_lock,
};
use crate::reflection::run_id_high_water::{RunIdHighWaterPaths, scan_reflection_run_high_water};
use crate::retention::guard::read_reflection_retention_guard;
use crate::retention::pass::{
    RuntimeRetentionOptions, RuntimeRetentionReport, run_runtime_retention,
};
use crate::retention::policy::{
    DEFAULT_RUNTIME_RETENTION, RetentionContext, RetentionError, RuntimeRetentionPolicy,
};
use crate::retention::size::{measure_runtime_size, measure_tree_bytes};
use crate::retention::test_support::{
    DAY_MS, Identity, JournalOptions, JournalShape, NOW, RunShape, Sentinel, age_tree, days_ago,
    encoded_session, identity, iso, journal, now, read_names, reflection_run, run_id_mark,
    runs_dir, sidecar, tombstones, write_json,
};

const HOUR_MS: i64 = 60 * 60 * 1000;

fn hours_ago(hours: i64) -> i64 {
    NOW - hours * HOUR_MS
}

// `discard_unreflected_journals` defaults OFF (the LEAD's D1 call); B02, B12 and B18 run the default
// and an explicit switch-on row (health-gated, the r4 behaviour).
const UNREFLECTED_ON: RuntimeRetentionPolicy = RuntimeRetentionPolicy {
    abandoned_journal_days: 14.0,
    discard_unreflected_journals: true,
    ..DEFAULT_RUNTIME_RETENTION
};
const UNREFLECTED_DEFAULT: RuntimeRetentionPolicy = RuntimeRetentionPolicy {
    abandoned_journal_days: 14.0,
    ..DEFAULT_RUNTIME_RETENTION
};

fn cap_options(paths: &MemoryIdentityPaths, max_bytes: u64) -> RuntimeSizeCapOptions<'_> {
    RuntimeSizeCapOptions::new(
        RetentionContext::new(paths, UNREFLECTED_ON, &now),
        max_bytes,
    )
}

fn cap(paths: &MemoryIdentityPaths, max_bytes: u64) -> RuntimeSizeCapReport {
    run_runtime_size_cap(&cap_options(paths, max_bytes)).expect("run the size cap")
}

fn padded(dir: PathBuf, bytes: usize, at: i64) -> PathBuf {
    std::fs::write(dir.join("padding.bin"), vec![1_u8; bytes]).expect("padding file");
    age_tree(&dir, at);
    dir
}

fn journal_at(
    paths: &MemoryIdentityPaths,
    session_id: &str,
    shape: JournalShape,
    idle_since: i64,
) -> PathBuf {
    journal(
        paths,
        session_id,
        shape,
        idle_since,
        JournalOptions::default(),
    )
}

// The cap's guard creates locks/ and raises run-id.json before it measures; settle both first so a
// test's own measure is the one the cap sees.
fn settle_guard(paths: &MemoryIdentityPaths) {
    read_reflection_retention_guard(&RetentionContext::new(paths, UNREFLECTED_ON, &now))
        .expect("settle the retention guard");
}

fn folded_runs(paths: &MemoryIdentityPaths, count: usize, bytes: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 1..=count {
        let run_id = format!("reflection-run-{index}");
        let finished_at = days_ago((count - index + 1) as f64) + index as i64;
        let outcome = if index % 2 == 0 {
            "no_changes"
        } else {
            "merged"
        };
        let dir = reflection_run(
            paths,
            &run_id,
            RunShape {
                outcome: Some(outcome.to_string()),
                finished_at: Some(finished_at),
                ..RunShape::default()
            },
        );
        padded(dir, bytes, finished_at);
        ids.push(run_id);
    }
    ids
}

fn tree_bytes(path: &Path) -> u64 {
    measure_tree_bytes(path, None).expect("measure tree bytes")
}

fn runtime_bytes(paths: &MemoryIdentityPaths) -> u64 {
    measure_runtime_size(&paths.runtime, None)
        .expect("measure runtime size")
        .bytes
}

fn bytes_of(paths: &MemoryIdentityPaths, run_ids: &[String]) -> u64 {
    run_ids
        .iter()
        .map(|run_id| tree_bytes(&runs_dir(paths).join(run_id)))
        .sum()
}

fn live_names(dir: &Path) -> Vec<String> {
    read_names(dir)
        .into_iter()
        .filter(|name| !name.starts_with(".prune-"))
        .collect()
}

fn sorted(names: &[String]) -> Vec<String> {
    let mut names = names.to_vec();
    names.sort();
    names
}

fn evicted_names(report: &RuntimeSizeCapReport) -> Vec<String> {
    report
        .evicted
        .iter()
        .map(|entry| entry.name.clone())
        .collect()
}

fn evicted_tiers(report: &RuntimeSizeCapReport) -> Vec<RuntimeCapTier> {
    report.evicted.iter().map(|entry| entry.tier).collect()
}

fn evicted_pairs(report: &RuntimeSizeCapReport) -> Vec<(RuntimeCapTier, String)> {
    report
        .evicted
        .iter()
        .map(|entry| (entry.tier, entry.name.clone()))
        .collect()
}

fn folded_pairs(run_ids: &[String]) -> Vec<(RuntimeCapTier, String)> {
    run_ids
        .iter()
        .map(|run_id| (RuntimeCapTier::FoldedRun, run_id.clone()))
        .collect()
}

fn names_in_tier(report: &RuntimeSizeCapReport, tier: RuntimeCapTier) -> Vec<String> {
    report
        .evicted
        .iter()
        .filter(|entry| entry.tier == tier)
        .map(|entry| entry.name.clone())
        .collect()
}

fn skipped(report: &RuntimeSizeCapReport, reason: &str) -> bool {
    report.skipped.iter().any(|entry| entry == reason)
}

/// Every path under `root`, relative and sorted (`readdir(root, { recursive: true })`).
fn listing(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for name in read_names(dir) {
            let path = dir.join(&name);
            out.push(
                path.strip_prefix(root)
                    .expect("path under root")
                    .to_string_lossy()
                    .into_owned(),
            );
            if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

// B01's budget comes from the bytes the fixture wrote, never from a live measure of runtime/ taken
// before the cap takes its own (#13194): anything written between the two reads moved the boundary.
// The budget sits half a folded run past the third run, so runtime/'s own directories and a small
// unrelated write both stay inside it; the 256 KiB pad keeps that half run well above them. The
// identity is returned so the caller's own checks still see its files.
fn cap_three_oldest_folded(before_cap: impl FnOnce(&MemoryIdentityPaths)) -> Identity {
    let fixture = identity();
    let paths = &fixture.paths;
    let runs = folded_runs(paths, 10, 256 * 1024);
    let sidecars: Vec<PathBuf> = (1..=5)
        .map(|n| {
            padded(
                sidecar(paths, &format!("s-{n}"), days_ago(1.0)),
                32 * 1024,
                days_ago(1.0),
            )
        })
        .collect();
    let journals: Vec<PathBuf> = (1..=5)
        .map(|n| journal_at(paths, &format!("j-{n}"), JournalShape::Never, days_ago(3.0)))
        .collect();
    settle_guard(paths);
    let mut fixture_bytes = bytes_of(paths, &runs);
    for path in sidecars.iter().chain(&journals) {
        fixture_bytes += tree_bytes(path);
    }
    let half_third_run = bytes_of(paths, &runs[2..3]) / 2;
    let max_bytes = fixture_bytes - bytes_of(paths, &runs[..3]) + half_third_run;
    before_cap(paths);

    let report = cap(paths, max_bytes);

    assert_eq!(evicted_pairs(&report), folded_pairs(&runs[..3]));
    assert_eq!(live_names(&runs_dir(paths)), sorted(&runs[3..]));
    for dir in sidecars.iter().chain(&journals) {
        assert!(dir.exists());
    }
    assert!(report.after_bytes <= max_bytes);
    assert_eq!(report.exceeded_bytes, 0);
    fixture
}

// runtime size cap: eviction order

#[test]
fn folded_runs_go_oldest_first_until_it_fits_and_nothing_else_is_touched() {
    cap_three_oldest_folded(|_| {});
}

#[test]
fn exactly_the_three_oldest_folded_runs_go_despite_an_unrelated_write_after_the_budget() {
    let fixture = cap_three_oldest_folded(|paths| {
        std::fs::write(paths.runtime.join("unrelated.bin"), vec![1_u8; 16 * 1024])
            .expect("unrelated file");
    });
    assert!(fixture.paths.runtime.join("unrelated.bin").exists());
}

#[test]
fn idle_sidecars_and_journals_follow_too_few_folded_bytes_and_the_hot_ones_survive() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 3, 1024);
    let old_sidecars = ["side-5h", "side-4h", "side-3h", "side-2h"];
    for (index, name) in old_sidecars.iter().enumerate() {
        let at = hours_ago(5 - index as i64);
        padded(sidecar(paths, name, at), 8 * 1024, at);
    }
    let hot_sidecar = sidecar(paths, "side-hot", NOW - 30 * 60 * 1000);
    let old_journal = journal_at(paths, "j-old", JournalShape::Never, days_ago(3.0));
    let hot_journal = journal_at(paths, "j-hot", JournalShape::Never, hours_ago(1));

    let report = cap(paths, 1);

    let tiers = evicted_tiers(&report);
    let mut ordered = tiers.clone();
    ordered.sort_by_key(|tier| tier_rank(*tier));
    assert_eq!(tiers, ordered);
    assert_eq!(
        names_in_tier(&report, RuntimeCapTier::Sidecar),
        old_sidecars.map(encoded_session)
    );
    assert_eq!(
        names_in_tier(&report, RuntimeCapTier::NeverReflectedJournal),
        vec!["j-old"]
    );
    assert!(!old_journal.exists());
    assert!(hot_journal.exists());
    assert!(hot_sidecar.exists());
    assert!(report.exceeded_bytes > 0);
}

#[test]
fn the_switch_on_evicts_never_and_partly_reflected_journals() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 3, 1024);
    let old_journal = journal_at(paths, "j-old", JournalShape::Never, days_ago(3.0));
    let partly = journal_at(paths, "j-partly", JournalShape::Partly, days_ago(3.0));
    let mut options = cap_options(paths, 1);
    options.context.policy = UNREFLECTED_ON;

    let report = run_runtime_size_cap(&options).unwrap();

    assert_eq!(
        evicted_tiers(&report),
        vec![
            RuntimeCapTier::FoldedRun,
            RuntimeCapTier::FoldedRun,
            RuntimeCapTier::NeverReflectedJournal,
            RuntimeCapTier::PartlyReflectedTranscript,
        ]
    );
    assert!(!old_journal.exists());
    assert!(!partly.join("transcript.jsonl").exists());
}

#[test]
fn the_default_switch_keeps_never_and_partly_reflected_journals() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 3, 1024);
    let old_journal = journal_at(paths, "j-old", JournalShape::Never, days_ago(3.0));
    let partly = journal_at(paths, "j-partly", JournalShape::Partly, days_ago(3.0));
    let old_sidecar = padded(
        sidecar(paths, "side-5h", hours_ago(5)),
        8 * 1024,
        hours_ago(5),
    );
    let mut options = cap_options(paths, 1);
    options.context.policy = UNREFLECTED_DEFAULT;

    let report = run_runtime_size_cap(&options).unwrap();

    assert_eq!(
        evicted_tiers(&report),
        vec![
            RuntimeCapTier::FoldedRun,
            RuntimeCapTier::FoldedRun,
            RuntimeCapTier::Sidecar,
        ]
    );
    assert!(!old_sidecar.exists());
    assert!(old_journal.exists());
    assert!(partly.join("transcript.jsonl").exists());
    assert_eq!(
        report.kept.get("j-old"),
        Some(&RuntimeCapKeepReason::UnreflectedRetentionOff)
    );
}

#[test]
fn only_the_idle_sibling_of_in_flight_live_and_owned_entries_is_evicted() {
    let fixture = identity();
    let paths = &fixture.paths;
    let in_flight = padded(
        reflection_run(
            paths,
            "reflection-run-4",
            RunShape {
                sentinel: Some(Sentinel::None),
                finished_at: Some(days_ago(20.0)),
                ..RunShape::default()
            },
        ),
        64 * 1024,
        days_ago(20.0),
    );
    let live = journal_at(paths, "j-live", JournalShape::Never, days_ago(5.0));
    let owned = sidecar(paths, "owned", days_ago(5.0));
    let owned_names = BTreeSet::from([encoded_session("owned")]);
    let control = journal_at(paths, "j-idle", JournalShape::Never, days_ago(5.0));
    let is_live = |session_id: &str| session_id == "j-live";
    let mut options = cap_options(paths, 1);
    options.context.is_live_session = Some(&is_live);
    options.owned_sidecars = Some(&owned_names);

    let report = run_runtime_size_cap(&options).unwrap();

    assert_eq!(evicted_names(&report), vec!["j-idle"]);
    assert!(!control.exists());
    assert_eq!(report.exceeded_bytes, report.after_bytes - 1);
    for dir in [&in_flight, &live, &owned] {
        assert!(dir.exists());
    }
}

#[test]
fn tier_six_evicts_the_five_oldest_unfolded_runs_and_keeps_the_newest_ten() {
    let fixture = identity();
    let paths = &fixture.paths;
    let mut ids = Vec::new();
    for index in 1..=15_i32 {
        let run_id = format!("reflection-run-{index}");
        let finished_at = days_ago(f64::from(30 - index));
        let sentinel = if index % 3 == 0 {
            Sentinel::Abandoned
        } else {
            Sentinel::Final
        };
        let dir = reflection_run(
            paths,
            &run_id,
            RunShape {
                outcome: Some("failed".to_string()),
                sentinel: Some(sentinel),
                finished_at: Some(finished_at),
                ..RunShape::default()
            },
        );
        padded(dir, 4 * 1024, finished_at);
        ids.push(run_id);
    }

    let report = cap(paths, 1);

    assert_eq!(
        names_in_tier(&report, RuntimeCapTier::UnfoldedRun),
        ids[..5].to_vec()
    );
    assert_eq!(live_names(&runs_dir(paths)), sorted(&ids[5..]));
    for run_id in &ids[5..] {
        assert!(runs_dir(paths).join(run_id).join("child-1.log").exists());
    }
}

#[test]
fn unfolded_runs_beyond_the_newest_ten_younger_than_retention_are_kept_as_recent() {
    let fixture = identity();
    let paths = &fixture.paths;
    for index in 1..=12_i64 {
        reflection_run(
            paths,
            &format!("reflection-run-{index}"),
            RunShape {
                outcome: Some("failed".to_string()),
                finished_at: Some(days_ago(6.0) + index * HOUR_MS),
                ..RunShape::default()
            },
        );
    }

    let report = cap(paths, 1);

    assert!(report.evicted.is_empty());
    assert_eq!(
        report.kept.get("reflection-run-1"),
        Some(&RuntimeCapKeepReason::Recent)
    );
    assert_eq!(
        report.kept.get("reflection-run-2"),
        Some(&RuntimeCapKeepReason::Recent)
    );
}

#[test]
fn both_run_anchors_survive_tier_one() {
    let fixture = identity();
    let paths = &fixture.paths;
    padded(
        reflection_run(
            paths,
            "reflection-run-50",
            RunShape {
                finished_at: Some(days_ago(6.0)),
                ..RunShape::default()
            },
        ),
        16 * 1024,
        days_ago(6.0),
    );
    folded_runs(paths, 4, 64 * 1024);

    let report = cap(paths, 1);

    assert_eq!(
        evicted_names(&report),
        vec!["reflection-run-1", "reflection-run-2", "reflection-run-3"]
    );
    assert_eq!(
        report.kept.get("reflection-run-50"),
        Some(&RuntimeCapKeepReason::HighWater)
    );
    assert_eq!(
        report.kept.get("reflection-run-4"),
        Some(&RuntimeCapKeepReason::Newest)
    );
    assert_eq!(
        live_names(&runs_dir(paths)),
        vec!["reflection-run-4", "reflection-run-50"]
    );
}

#[test]
fn max_bytes_zero_measures_and_evicts_nothing() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 3, 64 * 1024);

    let report = cap(paths, 0);

    assert!(report.evicted.is_empty());
    assert!(report.before_bytes > 0);
    assert_eq!(live_names(&runs_dir(paths)).len(), 3);
    assert_eq!(
        evicted_names(&cap(paths, 1)),
        vec!["reflection-run-1", "reflection-run-2"]
    );
}

// runtime size cap: crash windows and claims

#[test]
fn a_tombstone_left_by_an_interrupted_eviction_goes_and_eviction_continues() {
    let fixture = identity();
    let paths = &fixture.paths;
    let runs = folded_runs(paths, 4, 64 * 1024);
    settle_guard(paths);
    let tombstone = runs_dir(paths).join(".prune-reflection-run-0-4b0f");
    std::fs::create_dir_all(&tombstone).expect("tombstone directory");
    std::fs::write(
        tombstone.join("child-1.log"),
        "left by an interrupted pass\n",
    )
    .expect("tombstone file");
    let max_bytes = runtime_bytes(paths) - tree_bytes(&tombstone) - bytes_of(paths, &runs[..1]);

    let report = cap(paths, max_bytes);

    assert!(!tombstone.exists());
    assert_eq!(evicted_names(&report), runs[..1].to_vec());
}

#[test]
fn an_aborted_signal_evicts_nothing_and_rejects_with_the_abort() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 3, 64 * 1024);
    let aborted = || true;
    let mut options = cap_options(paths, 1);
    options.context.cancel = Some(&aborted);

    assert!(matches!(
        run_runtime_size_cap(&options),
        Err(RetentionError::Aborted)
    ));
    assert_eq!(live_names(&runs_dir(paths)).len(), 3);
}

#[test]
fn run_dirs_with_read_only_payload_files_are_removed_completely() {
    let fixture = identity();
    let paths = &fixture.paths;
    for (run_id, outcome, days) in [
        ("reflection-run-1", "merged", 3.0),
        ("reflection-run-2", "merged", 2.0),
        ("reflection-run-3", "failed", 40.0),
    ] {
        reflection_run(
            paths,
            run_id,
            RunShape {
                outcome: Some(outcome.to_string()),
                finished_at: Some(days_ago(days)),
                read_only_payload: true,
                ..RunShape::default()
            },
        );
    }
    for index in 4..=13_i64 {
        reflection_run(
            paths,
            &format!("reflection-run-{index}"),
            RunShape {
                outcome: Some("failed".to_string()),
                finished_at: Some(days_ago(30.0) + index),
                ..RunShape::default()
            },
        );
    }
    reflection_run(
        paths,
        "reflection-run-20",
        RunShape {
            outcome: Some("merged".to_string()),
            finished_at: Some(days_ago(1.0)),
            ..RunShape::default()
        },
    );

    let report = cap(paths, 1);

    assert_eq!(
        evicted_names(&report),
        vec!["reflection-run-1", "reflection-run-2", "reflection-run-3"]
    );
    for run_id in ["reflection-run-1", "reflection-run-2", "reflection-run-3"] {
        assert!(!runs_dir(paths).join(run_id).exists());
    }
    assert!(tombstones(&runs_dir(paths)).is_empty());
    assert_eq!(report.after_bytes, runtime_bytes(paths));
    assert!(report.after_bytes < report.before_bytes);
}

#[test]
fn the_highest_numbered_trace_survives_and_the_next_mint_is_unchanged() {
    for with_mark in [false, true] {
        let fixture = identity();
        let paths = &fixture.paths;
        reflection_run(
            paths,
            "reflection-run-90",
            RunShape {
                outcome: Some("merged".to_string()),
                finished_at: Some(days_ago(6.0)),
                ..RunShape::default()
            },
        );
        folded_runs(paths, 5, 64 * 1024);
        for index in 6..=20_i64 {
            reflection_run(
                paths,
                &format!("reflection-run-{index}"),
                RunShape {
                    outcome: Some("failed".to_string()),
                    finished_at: Some(days_ago(40.0) + index),
                    ..RunShape::default()
                },
            );
        }
        if with_mark {
            run_id_mark(paths, 90);
        }
        let next_before = scan_reflection_run_high_water(RunIdHighWaterPaths::from(paths)) + 1;

        let report = cap(paths, 1);

        assert_eq!(
            report.kept.get("reflection-run-90"),
            Some(&RuntimeCapKeepReason::HighWater)
        );
        assert!(runs_dir(paths).join("reflection-run-90").exists());
        assert_eq!(
            scan_reflection_run_high_water(RunIdHighWaterPaths::from(paths)) + 1,
            next_before
        );
    }
}

// runtime size cap: journal floor for callers without session liveness (B13 CLI variant, L5)

#[test]
fn the_cli_journal_floor_evicts_only_journals_idle_past_it() {
    let fixture = identity();
    let paths = &fixture.paths;
    let idle_week = journal_at(paths, "j-never-8d", JournalShape::Never, days_ago(8.0));
    let idle_days = journal_at(paths, "j-never-3d", JournalShape::Never, days_ago(3.0));
    let fully_recent = journal_at(paths, "j-fully-3d", JournalShape::Fully, days_ago(3.0));
    let partly_recent = journal_at(paths, "j-partly-3d", JournalShape::Partly, days_ago(3.0));
    let mut options = cap_options(paths, 1);
    options.journal_floor_ms = Some(7 * DAY_MS);

    let report = run_runtime_size_cap(&options).unwrap();

    assert_eq!(evicted_names(&report), vec!["j-never-8d"]);
    assert!(!idle_week.exists());
    for dir in [&idle_days, &fully_recent, &partly_recent] {
        assert!(dir.exists());
    }
    assert!(fully_recent.join("transcript.jsonl").exists());
    assert!(partly_recent.join("transcript.jsonl").exists());
}

// runtime size cap: journals named by a reservation (B15, J0)

fn reserve(paths: &MemoryIdentityPaths, file: &str, run_id: &str, conversation_ids: &[&str]) {
    write_json(
        &paths.reflection.join(file),
        &json!({
            "runId": run_id,
            "reservedAt": iso(days_ago(1.0)),
            "request": { "trigger": "manual", "conversationIds": conversation_ids },
        }),
    );
}

#[test]
fn journals_named_by_active_and_pending_reservations_are_kept_as_reserved() {
    let fixture = identity();
    let paths = &fixture.paths;
    let reserved_fully = journal_at(paths, "conv-x", JournalShape::Fully, days_ago(3.0));
    let reserved_never = journal_at(paths, "conv-y", JournalShape::Never, days_ago(3.0));
    let sibling_fully = journal_at(paths, "conv-x2", JournalShape::Fully, days_ago(3.0));
    let sibling_never = journal_at(paths, "conv-y2", JournalShape::Never, days_ago(3.0));
    reserve(paths, "active.lock", "reflection-run-7", &["conv-x"]);
    reserve(paths, "pending.json", "reflection-run-8", &["conv-y"]);

    let report = cap(paths, 1);

    assert_eq!(
        report.kept.get("conv-x"),
        Some(&RuntimeCapKeepReason::Reserved)
    );
    assert_eq!(
        report.kept.get("conv-y"),
        Some(&RuntimeCapKeepReason::Reserved)
    );
    assert!(reserved_fully.join("transcript.jsonl").exists());
    assert!(reserved_never.exists());
    assert!(!sibling_fully.join("transcript.jsonl").exists());
    assert!(!sibling_never.exists());
}

#[test]
fn a_contended_scheduler_lock_skips_the_journal_tiers_and_still_evicts_runs_and_sidecars() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 3, 64 * 1024);
    let idle_sidecar = padded(sidecar(paths, "idle", hours_ago(5)), 8 * 1024, hours_ago(5));
    let never_journal = journal_at(paths, "conv-z", JournalShape::Never, days_ago(3.0));
    std::fs::create_dir_all(&paths.locks).expect("locks directory");
    let lock_path = reflection_scheduler_lock_path(&paths.locks);
    let holder = create_lock_record(
        "reflection-scheduler",
        CreateLockRecordOptions {
            run_id: Some("other-process".to_string()),
        },
    )
    .expect("holder record");
    acquire_lock(&lock_path, &holder, &AcquireLockOptions::default())
        .expect("hold the scheduler lock");

    let report = run_runtime_size_cap(&cap_options(paths, 1));
    release_lock(&lock_path, &holder).expect("release the scheduler lock");
    let report = report.expect("run the size cap");

    assert!(skipped(&report, "reflection-scheduler-busy"));
    assert_eq!(
        evicted_tiers(&report),
        vec![
            RuntimeCapTier::FoldedRun,
            RuntimeCapTier::FoldedRun,
            RuntimeCapTier::Sidecar,
        ]
    );
    assert!(!idle_sidecar.exists());
    assert!(never_journal.exists());
}

// runtime size cap: health gate on unfolded journals (B17, M6)

fn completion_of(paths: &MemoryIdentityPaths, run_id: &str, outcome: &str, finished_at: i64) {
    let consecutive_failures = u32::from(outcome == "failed");
    write_json(
        &paths
            .reflection
            .join("completions")
            .join(format!("{run_id}.json")),
        &json!({
            "schemaVersion": 1,
            "runId": run_id,
            "identity": "agent-test",
            "category": "quick",
            "conversationIds": [],
            "trigger": "step-count",
            "outcome": outcome,
            "startedAt": iso(finished_at - 60_000),
            "finishedAt": iso(finished_at),
            "durationMs": 60_000,
            "consecutiveFailures": consecutive_failures,
            "delivery": { "status": "consumed" },
        }),
    );
}

/// The gate fixture; its fully-reflected journal is written too, though no assertion reads it.
struct GateShape {
    never: PathBuf,
    partly: PathBuf,
    sidecar: PathBuf,
}

fn gate_shape(paths: &MemoryIdentityPaths) -> GateShape {
    journal_at(paths, "j-fully", JournalShape::Fully, days_ago(3.0));
    GateShape {
        never: journal_at(paths, "j-never", JournalShape::Never, days_ago(3.0)),
        partly: journal_at(paths, "j-partly", JournalShape::Partly, days_ago(3.0)),
        sidecar: padded(sidecar(paths, "idle", hours_ago(5)), 8 * 1024, hours_ago(5)),
    }
}

#[test]
fn failing_reflection_without_a_fold_suspends_tiers_four_and_five() {
    let fixture = identity();
    let paths = &fixture.paths;
    completion_of(paths, "reflection-run-1", "merged", days_ago(20.0));
    completion_of(paths, "reflection-run-2", "failed", days_ago(2.0));
    let dirs = gate_shape(paths);

    let report = cap(paths, 1);

    assert_eq!(
        evicted_tiers(&report),
        vec![
            RuntimeCapTier::FullyReflectedTranscript,
            RuntimeCapTier::Sidecar,
        ]
    );
    assert_eq!(
        report.kept.get("j-never"),
        Some(&RuntimeCapKeepReason::ReflectionUnhealthy)
    );
    assert_eq!(
        report.kept.get("j-partly"),
        Some(&RuntimeCapKeepReason::ReflectionUnhealthy)
    );
    assert_eq!(report.suspended_since, Some(iso(days_ago(2.0))));
    assert!(dirs.never.exists());
    assert!(dirs.partly.join("transcript.jsonl").exists());
    assert!(!dirs.sidecar.exists());
    assert!(report.exceeded_bytes > 0);
}

#[test]
fn a_recent_fold_no_completions_or_disabled_reflection_open_the_gate() {
    for variant in ["recent-fold", "no-completions", "disabled"] {
        let fixture = identity();
        let paths = &fixture.paths;
        if variant == "recent-fold" {
            completion_of(paths, "reflection-run-1", "failed", days_ago(3.0));
            completion_of(paths, "reflection-run-2", "merged", days_ago(2.0));
        }
        if variant == "disabled" {
            completion_of(paths, "reflection-run-2", "failed", days_ago(2.0));
        }
        let dirs = gate_shape(paths);
        let mut options = cap_options(paths, 1);
        if variant == "disabled" {
            options.context.reflection_enabled = Some(false);
        }

        let report = run_runtime_size_cap(&options).unwrap();

        assert_eq!(report.suspended_since, None);
        assert!(!dirs.never.exists());
        assert!(!dirs.partly.join("transcript.jsonl").exists());
    }
}

// runtime size cap: discard_unreflected_journals (LEAD D1: default off, B18)

#[test]
fn the_default_switch_keeps_sixty_day_unreflected_journals_as_the_held_remainder() {
    let fixture = identity();
    let paths = &fixture.paths;
    let never = journal_at(paths, "j-never-60d", JournalShape::Never, days_ago(60.0));
    let partly = journal_at(paths, "j-partly-60d", JournalShape::Partly, days_ago(60.0));
    let fully = journal_at(paths, "j-fully-60d", JournalShape::Fully, days_ago(60.0));
    let held_bytes = tree_bytes(&never) + tree_bytes(&partly.join("transcript.jsonl"));
    let mut options = cap_options(paths, 1);
    options.context.policy = UNREFLECTED_DEFAULT;

    let report = run_runtime_size_cap(&options).unwrap();

    assert_eq!(
        report.kept.get("j-never-60d"),
        Some(&RuntimeCapKeepReason::UnreflectedRetentionOff)
    );
    assert_eq!(
        report.kept.get("j-partly-60d"),
        Some(&RuntimeCapKeepReason::UnreflectedRetentionOff)
    );
    assert_eq!(report.unreflected_held_bytes, held_bytes);
    assert!(never.exists());
    assert!(partly.join("transcript.jsonl").exists());
    assert!(!fully.join("transcript.jsonl").exists());
}

#[test]
fn the_switch_on_evicts_a_sixty_day_never_reflected_journal() {
    let fixture = identity();
    let paths = &fixture.paths;
    let never = journal_at(paths, "j-never-60d", JournalShape::Never, days_ago(60.0));
    let mut options = cap_options(paths, 1);
    options.context.policy = UNREFLECTED_ON;

    let report = run_runtime_size_cap(&options).unwrap();

    assert_eq!(evicted_names(&report), vec!["j-never-60d"]);
    assert_eq!(report.unreflected_held_bytes, 0);
    assert!(!never.exists());
}

// runtime size cap: pass budget and cursor (B16, H4; r3 N8, N3)

/// A stepping timer: every read advances `step_ms`, so the budget is spent by reads, not by wall
/// time.
fn stepping_clock(step_ms: u64) -> impl Fn() -> u64 {
    let reads = Cell::new(0_u64);
    move || {
        let read = reads.get();
        reads.set(read + 1);
        read * step_ms
    }
}

#[test]
fn a_budget_for_fifteen_evictions_stops_with_a_tier_cursor_and_the_resumed_walk_finishes() {
    let fixture = identity();
    let paths = &fixture.paths;
    let runs = folded_runs(paths, 42, 1024);
    settle_guard(paths);
    let max_bytes = runtime_bytes(paths) - bytes_of(paths, &runs[..40]);
    let clock = stepping_clock(10);
    let mut options = cap_options(paths, max_bytes);
    options.budget_ms = Some(160);
    options.clock = Some(&clock);

    let first = run_runtime_size_cap(&options).unwrap();

    assert_eq!(first.evicted.len(), 15);
    // `cursor.store == "cap"`: RuntimeCapCursor is the cap's own type and carries no store field.
    assert!(first.cursor.is_some());
    assert_eq!(
        first.cursor.as_ref().map(|cursor| cursor.tier.as_str()),
        Some("folded-run")
    );
    assert_eq!(
        first
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_name.as_str()),
        Some(runs[14].as_str())
    );
    assert!(first.exceeded_bytes > 0);

    let mut resumed = cap_options(paths, max_bytes);
    resumed.resume = first.cursor.as_ref();
    let second = run_runtime_size_cap(&resumed).unwrap();

    assert_eq!(second.cursor, None);
    assert_eq!(evicted_names(&second), runs[15..40].to_vec());
    assert!(second.after_bytes <= max_bytes);
}

#[test]
fn a_measurement_walk_longer_than_the_budget_records_measure_over_budget() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 4, 64 * 1024);
    let clock = stepping_clock(1000);
    let mut options = cap_options(paths, 1);
    options.budget_ms = Some(500);
    options.clock = Some(&clock);

    let report = run_runtime_size_cap(&options).unwrap();

    assert!(report.evicted.is_empty());
    assert_eq!(report.cursor, None);
    assert!(skipped(&report, "measure-over-budget"));
}

#[test]
fn a_fired_measurement_deadline_evicts_nothing_and_records_measure_over_budget() {
    let fixture = identity();
    let paths = &fixture.paths;
    let runs = folded_runs(paths, 4, 64 * 1024);
    let fired = || true;
    let mut options = cap_options(paths, 1);
    options.budget_ms = Some(60_000);
    options.measure_cancel = Some(&fired);

    let report = run_runtime_size_cap(&options).unwrap();

    assert!(report.evicted.is_empty());
    assert_eq!(report.cursor, None);
    assert!(skipped(&report, "measure-over-budget"));
    assert_eq!(live_names(&runs_dir(paths)), sorted(&runs));
}

#[test]
fn an_abort_during_the_measurement_walk_rejects_instead_of_recording_measure_over_budget() {
    let fixture = identity();
    let paths = &fixture.paths;
    let runs = folded_runs(paths, 4, 64 * 1024);
    let aborted = Cell::new(false);
    let cancel = || aborted.get();
    // The first clock read starts the measurement walk; the caller is disposed right there.
    let clock = || -> u64 {
        aborted.set(true);
        0
    };
    let mut options = cap_options(paths, 1);
    options.context.cancel = Some(&cancel);
    options.measure_cancel = Some(&cancel);
    options.budget_ms = Some(60_000);
    options.clock = Some(&clock);

    assert!(matches!(
        run_runtime_size_cap(&options),
        Err(RetentionError::Aborted)
    ));
    assert_eq!(live_names(&runs_dir(paths)), sorted(&runs));
}

#[test]
fn an_identity_under_its_cap_writes_nothing_under_runtime() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 2, 64 * 1024);
    let before = listing(&paths.runtime);

    let report = cap(paths, u64::MAX);

    assert!(report.evicted.is_empty());
    assert_eq!(listing(&paths.runtime), before);
}

// runtime size cap: what the next pass could evict, for the doctors (#12538 review M1)

fn evictable_default(paths: &MemoryIdentityPaths) -> u64 {
    measure_runtime_cap_evictable_bytes(&RuntimeCapEvictableOptions {
        paths,
        policy: UNREFLECTED_DEFAULT,
        now: &now,
        reflection_enabled: None,
        journal_floor_ms: None,
        cancel: None,
    })
    .expect("measure the evictable bytes")
}

#[test]
fn evictable_bytes_count_the_folded_runs_below_the_newest_and_change_nothing() {
    let fixture = identity();
    let paths = &fixture.paths;
    let runs = folded_runs(paths, 4, 64 * 1024);
    journal_at(paths, "j-never", JournalShape::Never, days_ago(60.0));
    let before = listing(&paths.runtime);

    let evictable = evictable_default(paths);

    assert_eq!(evictable, bytes_of(paths, &runs[..3]));
    assert_eq!(listing(&paths.runtime), before);
}

#[test]
fn only_the_newest_folded_run_leaves_nothing_evictable() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 1, 64 * 1024);
    journal_at(paths, "j-never", JournalShape::Never, days_ago(60.0));

    assert_eq!(evictable_default(paths), 0);
}

// runtime retention then cap in one pass (B12 on)

fn retention_with_cap(
    paths: &MemoryIdentityPaths,
    policy: RuntimeRetentionPolicy,
    max_bytes: u64,
) -> RuntimeRetentionReport {
    let mut options = RuntimeRetentionOptions::new(RetentionContext::new(paths, policy, &now));
    options.max_bytes = Some(max_bytes);
    run_runtime_retention(&options).expect("run runtime retention")
}

#[test]
fn retention_frees_old_data_first_and_the_cap_evicts_only_tier_one() {
    let fixture = identity();
    let paths = &fixture.paths;
    for (run_id, days) in [("reflection-run-1", 12.0), ("reflection-run-2", 9.0)] {
        padded(
            reflection_run(
                paths,
                run_id,
                RunShape {
                    finished_at: Some(days_ago(days)),
                    ..RunShape::default()
                },
            ),
            64 * 1024,
            days_ago(days),
        );
    }
    let mut young = Vec::new();
    for index in 3..=7_i32 {
        let run_id = format!("reflection-run-{index}");
        let finished_at = days_ago(f64::from(8 - index)) - i64::from(index);
        padded(
            reflection_run(
                paths,
                &run_id,
                RunShape {
                    finished_at: Some(finished_at),
                    ..RunShape::default()
                },
            ),
            64 * 1024,
            finished_at,
        );
        young.push(run_id);
    }
    journal_at(paths, "j-abandoned", JournalShape::Never, days_ago(15.0));
    journal_at(paths, "j-partly", JournalShape::Partly, days_ago(15.0));
    let fresh_journal = journal_at(paths, "j-fresh", JournalShape::Never, days_ago(3.0));
    let fresh_sidecar = sidecar(paths, "fresh", days_ago(1.0));
    let after_retention = runtime_bytes(paths)
        - bytes_of(
            paths,
            &[
                "reflection-run-1".to_string(),
                "reflection-run-2".to_string(),
            ],
        )
        - tree_bytes(&paths.transcripts.join("j-abandoned"))
        - tree_bytes(&paths.transcripts.join("j-partly").join("transcript.jsonl"));
    let margin = bytes_of(paths, &young[2..3]) / 2;
    let max_bytes = after_retention - bytes_of(paths, &young[..2]) + margin;

    let report = retention_with_cap(paths, UNREFLECTED_ON, max_bytes);

    assert_eq!(
        report.runs.removed,
        vec!["reflection-run-1", "reflection-run-2"]
    );
    assert_eq!(
        sorted(&report.journals.removed),
        vec!["j-abandoned", "j-partly"]
    );
    assert_eq!(
        report.cap.as_ref().map(evicted_pairs),
        Some(folded_pairs(&young[..2]))
    );
    assert!(
        report
            .cap
            .as_ref()
            .expect("the pass ran its cap")
            .after_bytes
            <= max_bytes
    );
    assert!(fresh_journal.exists());
    assert!(fresh_sidecar.exists());
}

const TIER_ORDER: [RuntimeCapTier; 6] = [
    RuntimeCapTier::FoldedRun,
    RuntimeCapTier::FullyReflectedTranscript,
    RuntimeCapTier::Sidecar,
    RuntimeCapTier::NeverReflectedJournal,
    RuntimeCapTier::PartlyReflectedTranscript,
    RuntimeCapTier::UnfoldedRun,
];

fn tier_rank(tier: RuntimeCapTier) -> usize {
    TIER_ORDER
        .iter()
        .position(|entry| *entry == tier)
        .expect("a known tier")
}

// runtime retention then cap with the default switch (B12 default, off)

#[test]
fn the_default_switch_keeps_unreflected_journals_and_the_cap_evicts_folded_runs_only() {
    let fixture = identity();
    let paths = &fixture.paths;
    for index in 1..=4_i32 {
        let finished_at = days_ago(f64::from(5 - index));
        padded(
            reflection_run(
                paths,
                &format!("reflection-run-{index}"),
                RunShape {
                    finished_at: Some(finished_at),
                    ..RunShape::default()
                },
            ),
            64 * 1024,
            finished_at,
        );
    }
    let abandoned = journal_at(paths, "j-abandoned", JournalShape::Never, days_ago(15.0));
    let partly = journal_at(paths, "j-partly", JournalShape::Partly, days_ago(15.0));

    let report = retention_with_cap(paths, UNREFLECTED_DEFAULT, 1);

    assert!(report.journals.removed.is_empty());
    assert_eq!(
        report.cap.as_ref().map(evicted_tiers),
        Some(vec![
            RuntimeCapTier::FoldedRun,
            RuntimeCapTier::FoldedRun,
            RuntimeCapTier::FoldedRun,
        ])
    );
    assert!(abandoned.exists());
    assert!(partly.join("transcript.jsonl").exists());
}
