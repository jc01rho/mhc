//! Port of `retention/scan-robustness.test.ts` (omo #12538 review r3-r5): the cap and the doctors'
//! scans over one bad journal entry, the measurement deadline, lazy journal classification, the
//! cap's own resume point, and a journal that turns unreadable under the cap's lock.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::{count_runtime_past_retention, measure_unreflected_journal_bytes};
use crate::identity::layout::MemoryIdentityPaths;
use crate::journal::lock::{DefaultJournalLock, JournalLock};
use crate::journal::store::JournalError;
use crate::retention::cap::{
    RuntimeCapCursor, RuntimeCapEvictableOptions, RuntimeCapKeepReason, RuntimeSizeCapOptions,
    RuntimeSizeCapReport, measure_runtime_cap_evictable_bytes, run_runtime_size_cap,
};
use crate::retention::guard::read_reflection_retention_guard;
use crate::retention::pass::{
    RuntimeRetentionOptions, RuntimeRetentionReport, run_runtime_retention,
};
use crate::retention::policy::{
    DEFAULT_RUNTIME_RETENTION, RetentionContext, RetentionError, RuntimeRetentionPolicy,
};
use crate::retention::size::measure_runtime_size;
use crate::retention::test_support::{
    JournalOptions, JournalShape, RunShape, age_tree, days_ago, identity, journal, now, read_names,
    reflection_run, runs_dir, write_json,
};

const UNREFLECTED_ON: RuntimeRetentionPolicy = RuntimeRetentionPolicy {
    abandoned_journal_days: 14.0,
    discard_unreflected_journals: true,
    ..DEFAULT_RUNTIME_RETENTION
};

/// A pass budget of 0 ms: one item per store walk is admitted, and the cap's size walk always
/// overruns what is left, so the cap never makes progress on its own.
const ZERO_BUDGET: RuntimeRetentionPolicy = RuntimeRetentionPolicy {
    pass_budget_ms: 0,
    ..UNREFLECTED_ON
};

fn context(paths: &MemoryIdentityPaths, policy: RuntimeRetentionPolicy) -> RetentionContext<'_> {
    RetentionContext::new(paths, policy, &now)
}

fn cap(
    paths: &MemoryIdentityPaths,
    max_bytes: u64,
) -> Result<RuntimeSizeCapReport, RetentionError> {
    run_runtime_size_cap(&RuntimeSizeCapOptions::new(
        context(paths, UNREFLECTED_ON),
        max_bytes,
    ))
}

fn evictable_options<'a>(
    paths: &'a MemoryIdentityPaths,
    cancel: Option<&'a dyn Fn() -> bool>,
) -> RuntimeCapEvictableOptions<'a> {
    RuntimeCapEvictableOptions {
        paths,
        policy: UNREFLECTED_ON,
        now: &now,
        reflection_enabled: None,
        journal_floor_ms: None,
        cancel,
    }
}

fn evicted_names(report: &RuntimeSizeCapReport) -> Vec<String> {
    report
        .evicted
        .iter()
        .map(|entry| entry.name.clone())
        .collect()
}

fn fully_journal(paths: &MemoryIdentityPaths, session_id: &str) -> PathBuf {
    journal(
        paths,
        session_id,
        JournalShape::Fully,
        days_ago(60.0),
        JournalOptions::default(),
    )
}

fn folded_runs(paths: &MemoryIdentityPaths, count: usize, bytes: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 1..=count {
        let run_id = format!("reflection-run-{index}");
        let finished_at = days_ago((count - index + 1) as f64) + index as i64;
        let dir = reflection_run(
            paths,
            &run_id,
            RunShape {
                outcome: Some("merged".to_string()),
                finished_at: Some(finished_at),
                ..RunShape::default()
            },
        );
        std::fs::write(dir.join("padding.bin"), vec![1u8; bytes]).unwrap();
        age_tree(&dir, finished_at);
        ids.push(run_id);
    }
    ids
}

/// A stray plain file and a journal whose state.json is a directory, beside one good journal.
fn bad_journal_entries(paths: &MemoryIdentityPaths) {
    std::fs::create_dir_all(&paths.transcripts).unwrap();
    std::fs::write(paths.transcripts.join(".DS_Store"), "x").unwrap();
    std::fs::create_dir_all(paths.transcripts.join("j-torn").join("state.json")).unwrap();
    std::fs::write(
        paths.transcripts.join("j-torn").join("transcript.jsonl"),
        "{}\n",
    )
    .unwrap();
}

/// An injected journal lock: runs its hook on the lock path, then takes the default lock.
struct HookedJournalLock<F: Fn(&Path) -> Result<(), JournalError>>(F);

impl<F> JournalLock for HookedJournalLock<F>
where
    F: Fn(&Path) -> Result<(), JournalError> + Send + Sync,
{
    fn with_lock(
        &self,
        lock_path: &Path,
        task: &mut dyn FnMut() -> Result<(), JournalError>,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<(), JournalError> {
        (self.0)(lock_path)?;
        DefaultJournalLock.with_lock(lock_path, task, cancel)
    }
}

// runtime cap and doctor scans tolerate one bad journal entry (#12538 review r3 M1)

#[test]
fn the_cap_keeps_a_torn_journal_as_state_unreadable_and_still_evicts_the_good_one() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 2, 1024);
    let good = fully_journal(paths, "j-fully");
    bad_journal_entries(paths);

    let report = cap(paths, 1).unwrap();

    assert_eq!(
        report.kept.get("j-torn"),
        Some(&RuntimeCapKeepReason::StateUnreadable)
    );
    assert!(report.evicted.iter().any(|entry| entry.name == "j-fully"));
    assert!(!good.join("transcript.jsonl").exists());
}

#[test]
fn the_doctors_scans_resolve_over_the_same_bad_entries() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 2, 1024);
    journal(
        paths,
        "j-never",
        JournalShape::Never,
        days_ago(60.0),
        JournalOptions::default(),
    );
    bad_journal_entries(paths);

    assert!(measure_runtime_cap_evictable_bytes(&evictable_options(paths, None)).unwrap() > 0);
    assert!(measure_unreflected_journal_bytes(&context(paths, UNREFLECTED_ON)).unwrap() > 0);
    assert_eq!(
        count_runtime_past_retention(&context(paths, UNREFLECTED_ON))
            .unwrap()
            .journals,
        1
    );
}

// the doctors' journal measures run under the measurement deadline (#12538 review r3 M2)

#[test]
fn a_fired_deadline_rejects_both_journal_measures_with_the_abort() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 2, 1024);
    journal(
        paths,
        "j-never",
        JournalShape::Never,
        days_ago(60.0),
        JournalOptions::default(),
    );
    let fired = || true;
    let mut deadline = context(paths, UNREFLECTED_ON);
    deadline.cancel = Some(&fired);

    assert!(matches!(
        measure_unreflected_journal_bytes(&deadline),
        Err(RetentionError::Aborted)
    ));
    assert!(matches!(
        measure_runtime_cap_evictable_bytes(&evictable_options(paths, Some(&fired))),
        Err(RetentionError::Aborted)
    ));
}

// the cap classifies journals only when it reaches their tier (#12538 review r3 L1)

#[test]
fn a_cap_that_fits_on_folded_runs_looks_at_no_journal() {
    let fixture = identity();
    let paths = &fixture.paths;
    let runs = folded_runs(paths, 4, 256 * 1024);
    fully_journal(paths, "j-fully");
    bad_journal_entries(paths);
    read_reflection_retention_guard(&context(paths, UNREFLECTED_ON)).unwrap();
    let looked = RefCell::new(Vec::<String>::new());
    let is_live = |session_id: &str| {
        looked.borrow_mut().push(session_id.to_string());
        false
    };
    let max_bytes = measure_runtime_size(&paths.runtime, None).unwrap().bytes - 200 * 1024;
    let mut watched = context(paths, UNREFLECTED_ON);
    watched.is_live_session = Some(&is_live);

    let report = run_runtime_size_cap(&RuntimeSizeCapOptions::new(watched, max_bytes)).unwrap();

    assert_eq!(evicted_names(&report), vec![runs[0].clone()]);
    assert!(looked.borrow().is_empty());
}

// a resumed cap pass that walks no new item keeps its cursor (#12538 review r3 L2)

fn stored_folded_resume() -> RuntimeCapCursor {
    RuntimeCapCursor {
        tier: "folded-run".to_string(),
        clock: 0,
        last_name: "reflection-run-0".to_string(),
    }
}

#[test]
fn a_measurement_walk_over_the_budget_returns_the_stored_cursor_unchanged() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 4, 1024);
    let resume = stored_folded_resume();
    let reads = Cell::new(0u64);
    let clock = || -> u64 {
        let read = reads.get();
        reads.set(read + 1);
        read * 1000
    };
    let mut options = RuntimeSizeCapOptions::new(context(paths, UNREFLECTED_ON), 1);
    options.resume = Some(&resume);
    options.budget_ms = Some(500);
    options.clock = Some(&clock);

    let report = run_runtime_size_cap(&options).unwrap();

    assert!(
        report
            .skipped
            .iter()
            .any(|reason| reason == "measure-over-budget")
    );
    assert_eq!(report.cursor, Some(resume.clone()));
}

#[test]
fn a_deadline_before_the_first_new_item_returns_the_stored_cursor_unchanged() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 4, 1024);
    read_reflection_retention_guard(&context(paths, UNREFLECTED_ON)).unwrap();
    let resume = stored_folded_resume();
    // Reads 0 and 1 frame the measurement walk (1 ms, inside the budget); the first item's check
    // is past it.
    let reads = Cell::new(0usize);
    let clock = || -> u64 {
        let read = reads.get();
        reads.set(read + 1);
        [0, 1].get(read).copied().unwrap_or(1000)
    };
    let mut options = RuntimeSizeCapOptions::new(context(paths, UNREFLECTED_ON), 1);
    options.resume = Some(&resume);
    options.budget_ms = Some(5);
    options.clock = Some(&clock);

    let report = run_runtime_size_cap(&options).unwrap();

    assert!(report.evicted.is_empty());
    assert_eq!(report.cursor, Some(resume.clone()));
}

fn cursor_file(paths: &MemoryIdentityPaths) -> PathBuf {
    paths.runtime.join("retention.json")
}

fn persisted_cursors(paths: &MemoryIdentityPaths) -> Option<Value> {
    let path = cursor_file(paths);
    if !path.exists() {
        return None;
    }
    Some(serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
}

fn persisted_cap(paths: &MemoryIdentityPaths) -> Option<Value> {
    persisted_cursors(paths).and_then(|cursors| cursors.get("cap").cloned())
}

/// The cap resume point as the pass persists it in `retention.json`.
fn cap_cursor_json(cursor: &RuntimeCapCursor) -> Value {
    json!({
        "store": "cap",
        "tier": cursor.tier,
        "clock": cursor.clock,
        "lastName": cursor.last_name,
    })
}

fn live_runs(paths: &MemoryIdentityPaths) -> Vec<String> {
    read_names(&runs_dir(paths))
        .into_iter()
        .filter(|name| !name.starts_with(".prune-"))
        .collect()
}

/// 13 failed runs 60..48 days old: retention never removes them, the cap's tier 6 may evict the
/// oldest 3.
fn unfolded_runs(paths: &MemoryIdentityPaths) -> (Vec<String>, Vec<i64>) {
    let mut runs = Vec::new();
    let mut at = Vec::new();
    for index in 1_i32..=13 {
        let finished_at = days_ago(f64::from(61 - index));
        let run_id = format!("reflection-run-{index}");
        reflection_run(
            paths,
            &run_id,
            RunShape {
                outcome: Some("failed".to_string()),
                finished_at: Some(finished_at),
                ..RunShape::default()
            },
        );
        runs.push(run_id);
        at.push(finished_at);
    }
    (runs, at)
}

fn retention_pass(
    paths: &MemoryIdentityPaths,
    policy: RuntimeRetentionPolicy,
) -> RuntimeRetentionReport {
    let mut options = RuntimeRetentionOptions::new(context(paths, policy));
    options.max_bytes = Some(1);
    run_runtime_retention(&options).unwrap()
}

// a saved cap resume point never stops retention (#12538 review r4 M)

#[test]
fn a_stalled_cap_with_a_stored_resume_point_never_stops_retention() {
    let fixture = identity();
    let paths = &fixture.paths;
    for index in 1_i32..=4 {
        reflection_run(
            paths,
            &format!("reflection-run-{index}"),
            RunShape {
                finished_at: Some(days_ago(f64::from(60 - index))),
                ..RunShape::default()
            },
        );
    }
    let stale = fully_journal(paths, "j-fully");
    let resume = stored_folded_resume();
    // The shape a pass of #12538@237e8d01d1 persisted: the cap's resume point as the pass's only
    // cursor.
    write_json(
        &cursor_file(paths),
        &json!({ "version": 1, "cursor": cap_cursor_json(&resume) }),
    );

    let mut cap_resume_points = Vec::new();
    for _ in 0..10 {
        retention_pass(paths, ZERO_BUDGET);
        cap_resume_points.push(persisted_cap(paths));
    }

    // The newest folded run (also the run-id high water) stays as the anchor.
    assert_eq!(live_runs(paths), vec!["reflection-run-4".to_string()]);
    assert!(!stale.join("transcript.jsonl").exists());
    assert_eq!(cap_resume_points, vec![Some(cap_cursor_json(&resume)); 10]);
}

#[test]
fn the_store_walk_and_the_cap_each_resume_at_their_own_point() {
    let fixture = identity();
    let paths = &fixture.paths;
    let (runs, at) = unfolded_runs(paths);
    let resume = RuntimeCapCursor {
        tier: "unfolded-run".to_string(),
        clock: (at[0] + at[1]) / 2,
        last_name: runs[0].clone(),
    };
    write_json(
        &cursor_file(paths),
        &json!({
            "version": 1,
            "cursor": { "store": "journals", "lastName": "s-05" },
            "cap": cap_cursor_json(&resume),
        }),
    );

    let report = retention_pass(paths, UNREFLECTED_ON);

    assert_eq!(
        report.cap.as_ref().map(evicted_names),
        Some(vec![runs[1].clone(), runs[2].clone()])
    );
    let mut expected = vec![runs[0].clone()];
    expected.extend_from_slice(&runs[3..]);
    expected.sort();
    assert_eq!(live_runs(paths), expected);
    assert_eq!(persisted_cursors(paths), None);
}

#[test]
fn a_cap_that_made_no_progress_keeps_its_resume_point_and_the_next_pass_completes_there() {
    let fixture = identity();
    let paths = &fixture.paths;
    let (runs, at) = unfolded_runs(paths);
    let resume = RuntimeCapCursor {
        tier: "unfolded-run".to_string(),
        clock: (at[0] + at[1]) / 2,
        last_name: runs[0].clone(),
    };
    write_json(
        &cursor_file(paths),
        &json!({ "version": 1, "cap": cap_cursor_json(&resume) }),
    );

    let stalled = retention_pass(paths, ZERO_BUDGET);

    let stalled_cap = stalled.cap.as_ref().expect("the stalled pass ran its cap");
    assert!(
        stalled_cap
            .skipped
            .iter()
            .any(|reason| reason == "measure-over-budget")
    );
    assert!(stalled_cap.evicted.is_empty());
    assert_eq!(persisted_cap(paths), Some(cap_cursor_json(&resume)));

    let resumed = retention_pass(paths, UNREFLECTED_ON);

    let resumed_cap = resumed.cap.as_ref().expect("the resumed pass ran its cap");
    assert_eq!(
        evicted_names(resumed_cap),
        vec![runs[1].clone(), runs[2].clone()]
    );
    assert_eq!(resumed_cap.cursor, None);
    assert_eq!(persisted_cursors(paths), None);
}

// a journal that turns unreadable before the cap's locked eviction (#12538 review r4 L)

#[test]
fn a_journal_whose_state_turns_into_a_directory_under_its_lock_is_kept_and_the_next_is_evicted() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 2, 1024);
    let race = fully_journal(paths, "j-race");
    let good = fully_journal(paths, "j-zz-good");
    let race_lock = race.join("state.lock");
    let raced = Mutex::new(Vec::<PathBuf>::new());
    let lock = HookedJournalLock(|lock_path: &Path| -> Result<(), JournalError> {
        if lock_path == race_lock.as_path() {
            raced.lock().unwrap().push(lock_path.to_path_buf());
            std::fs::remove_file(race.join("state.json")).unwrap();
            std::fs::create_dir(race.join("state.json")).unwrap();
        }
        Ok(())
    });
    let mut options = RuntimeSizeCapOptions::new(context(paths, UNREFLECTED_ON), 1);
    options.journal_lock = Some(&lock);

    let report = run_runtime_size_cap(&options).unwrap();

    assert_eq!(raced.lock().unwrap().len(), 1);
    assert_eq!(
        report.kept.get("j-race"),
        Some(&RuntimeCapKeepReason::StateUnreadable)
    );
    assert!(report.evicted.iter().any(|entry| entry.name == "j-zz-good"));
    assert!(!good.join("transcript.jsonl").exists());
}

#[test]
fn an_abort_inside_the_last_candidates_journal_lock_rejects_the_cap() {
    let fixture = identity();
    let paths = &fixture.paths;
    folded_runs(paths, 2, 1024);
    let first = fully_journal(paths, "j-first");
    let last = fully_journal(paths, "j-zz-last");
    let last_lock = last.join("state.lock");
    let aborted = AtomicBool::new(false);
    let locked = Mutex::new(Vec::<PathBuf>::new());
    let lock = HookedJournalLock(|lock_path: &Path| -> Result<(), JournalError> {
        locked.lock().unwrap().push(lock_path.to_path_buf());
        if lock_path == last_lock.as_path() {
            // The caller aborts while this candidate's lock is taken, and the lock reports it as a
            // signal-aware lock does.
            aborted.store(true, Ordering::SeqCst);
            return Err(JournalError::Aborted);
        }
        Ok(())
    });
    let cancel = || aborted.load(Ordering::SeqCst);
    let mut signalled = context(paths, UNREFLECTED_ON);
    signalled.cancel = Some(&cancel);
    let mut options = RuntimeSizeCapOptions::new(signalled, 1);
    options.journal_lock = Some(&lock);

    // A cap that kept the aborted journal as state-unreadable would resolve, and its pass could
    // stamp itself complete.
    assert!(matches!(
        run_runtime_size_cap(&options),
        Err(RetentionError::Aborted)
    ));

    assert_eq!(
        *locked.lock().unwrap(),
        vec![first.join("state.lock"), last.join("state.lock")]
    );
    assert!(!first.join("transcript.jsonl").exists());
    assert!(last.join("transcript.jsonl").exists());
}
