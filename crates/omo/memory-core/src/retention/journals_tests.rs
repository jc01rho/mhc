use std::path::Path;

use super::*;
use crate::identity::layout::MemoryIdentityPaths;
use crate::journal::lock::DefaultJournalLock;
use crate::journal::store::{TranscriptJournal, TranscriptJournalOptions};
use crate::journal::sweep::{EmptyJournalKeepReason, sweep_empty_transcript_journals};
use crate::locks::{
    AcquireLockOptions, CreateLockRecordOptions, acquire_lock, create_lock_record,
    reflection_scheduler_lock_path, release_lock,
};
use crate::retention::policy::RuntimeRetentionPolicy;
use crate::retention::reflection_health_signal::UnreflectedRetentionGate;
use crate::retention::test_support::{
    JournalOptions, JournalShape, NOW, age_tree, append_rows_behind_state, days_ago, identity, iso,
    journal, read_json, second_step, third_step, write_json,
};

fn unreflected_policy() -> RuntimeRetentionPolicy {
    RuntimeRetentionPolicy {
        discard_unreflected_journals: true,
        ..RuntimeRetentionPolicy::default()
    }
}

fn prune(
    paths: &MemoryIdentityPaths,
    gate: Option<&UnreflectedRetentionGate>,
) -> RetentionStoreResult {
    let now = || NOW;
    let context = RetentionContext::new(paths, unreflected_policy(), &now);
    let guard = LazyRetentionGuard::new(context);
    prune_transcript_journals(&context, &guard, gate, &DefaultJournalLock).expect("prune journals")
}

fn open_gate() -> UnreflectedRetentionGate {
    UnreflectedRetentionGate::open()
}

fn state_of(dir: &Path) -> serde_json::Value {
    read_json(&dir.join("state.json"))
}

fn journal_with_hand_written_state(
    paths: &MemoryIdentityPaths,
    session_id: &str,
    state: serde_json::Value,
) -> std::path::PathBuf {
    let dir = paths.transcripts.join(session_id);
    let mut record = serde_json::json!({ "schema_version": "v3_assistant_steps" });
    for (key, value) in state.as_object().expect("state object") {
        record[key] = value.clone();
    }
    write_json(&dir.join("state.json"), &record);
    std::fs::write(
        dir.join("transcript.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "kind": "assistant",
                "text": "a",
                "captured_at": "2026-10-01T00:00:00.000Z",
                "source_line_id": "a1:assistant",
                "source_message_id": "a1",
            })
        ),
    )
    .expect("raw transcript");
    age_tree(&dir, days_ago(60.0));
    dir
}

#[test]
fn a_fully_reflected_journal_idle_past_retention_loses_only_its_transcript_and_is_stamped() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "s-full",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.removed, vec!["s-full".to_string()]);
    assert!(!exists(&dir.join("transcript.jsonl")));
    let state = state_of(&dir);
    assert!(state["transcript_pruned_at"].is_string());
    assert_eq!(state["reflected_through_message_id"], "a1");
    assert_eq!(state["reflected_completed_steps"], 1);
    assert!(state.get("reflected_through_byte_offset").is_none());

    let wall_clock_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_millis() as u64;
    let sweep =
        sweep_empty_transcript_journals(&paths.transcripts, wall_clock_ms + 60_000, 0, &|_| false)
            .expect("sweep");
    assert!(sweep.removed.is_empty());
    assert_eq!(sweep.kept[&EmptyJournalKeepReason::StateContent], 1);
}

#[test]
fn a_fully_reflected_journal_with_a_pending_compaction_is_kept() {
    let id = identity();
    let paths = &id.paths;
    journal(
        paths,
        "s-sibling",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );
    let dir = journal(
        paths,
        "s-full",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions {
            pending_compaction: true,
        },
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.removed, vec!["s-sibling".to_string()]);
    assert_eq!(report.kept["s-full"].as_str(), "pending-compaction");
    assert!(exists(&dir.join("transcript.jsonl")));
}

#[test]
fn a_never_reflected_journal_idle_past_the_abandoned_window_goes_and_reconcile_rebuilds_it() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "s-never",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.removed, vec!["s-never".to_string()]);
    assert!(!exists(&dir));

    let rebuilt = TranscriptJournal::new(TranscriptJournalOptions::new(&dir));
    let messages = vec![
        crate::journal::TranscriptProjection::User {
            message_id: "u1".to_string(),
            text: "first".to_string(),
        },
        crate::journal::TranscriptProjection::Assistant {
            message_id: "a1".to_string(),
            text_blocks: vec!["first answer".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        },
    ];
    let appended = rebuilt
        .reconcile(&messages)
        .expect("reconcile rebuilt journal");
    assert_eq!(appended.appended, 2);
    assert_eq!(appended.skipped, 0);
    let state = rebuilt.get_state().expect("rebuilt state");
    assert!(state.reflected_through_message_id.is_none());
    assert_eq!(state.reflected_completed_steps, 0);
    assert_eq!(state.steps_since_last_successful_reflection, 1);
}

#[test]
fn a_no_steps_journal_with_a_user_row_idle_past_the_abandoned_window_goes() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "s-nosteps",
        JournalShape::NoSteps,
        days_ago(15.0),
        JournalOptions::default(),
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.removed, vec!["s-nosteps".to_string()]);
    assert!(!exists(&dir));
}

#[test]
fn never_reflected_journals_that_are_recent_live_or_locked_are_kept_with_their_reason() {
    let id = identity();
    let paths = &id.paths;
    journal(
        paths,
        "s-sibling",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );
    journal(
        paths,
        "s-recent",
        JournalShape::Never,
        days_ago(13.0),
        JournalOptions::default(),
    );
    journal(
        paths,
        "s-live",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );
    let locked = journal(
        paths,
        "s-locked",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );
    std::fs::write(locked.join("state.lock"), "1\n\n").expect("lock file");
    age_tree(&locked, days_ago(15.0));

    let now = || NOW;
    let live = |session_id: &str| session_id == "s-live";
    let context = RetentionContext {
        is_live_session: Some(&live),
        ..RetentionContext::new(paths, unreflected_policy(), &now)
    };
    let guard = LazyRetentionGuard::new(context);
    let report =
        prune_transcript_journals(&context, &guard, Some(&open_gate()), &DefaultJournalLock)
            .expect("prune journals");

    assert_eq!(report.removed, vec!["s-sibling".to_string()]);
    assert_eq!(report.kept["s-recent"].as_str(), "recent");
    assert_eq!(report.kept["s-live"].as_str(), "live");
    assert_eq!(report.kept["s-locked"].as_str(), "locked");
}

#[test]
fn a_partly_reflected_journal_idle_past_the_abandoned_window_keeps_its_anchor_and_counts() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "s-partly",
        JournalShape::Partly,
        days_ago(15.0),
        JournalOptions::default(),
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.removed, vec!["s-partly".to_string()]);
    assert!(!exists(&dir.join("transcript.jsonl")));
    let state = state_of(&dir);
    assert_eq!(state["reflected_through_message_id"], "a1");
    assert_eq!(state["reflected_completed_steps"], 1);
    assert_eq!(state["steps_since_last_successful_reflection"], 1);
    assert!(state["transcript_pruned_at"].is_string());
    assert!(state.get("reflected_through_byte_offset").is_none());
}

#[test]
fn a_partly_reflected_journal_younger_than_the_abandoned_window_is_kept() {
    let id = identity();
    let paths = &id.paths;
    journal(
        paths,
        "s-partly",
        JournalShape::Partly,
        days_ago(10.0),
        JournalOptions::default(),
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.kept["s-partly"].as_str(), "recent");
    assert!(report.removed.is_empty());
}

#[test]
fn a_journal_whose_state_json_does_not_parse_is_kept_as_unreadable() {
    let id = identity();
    let paths = &id.paths;
    journal(
        paths,
        "s-sibling",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );
    let dir = journal(
        paths,
        "s-broken",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );
    std::fs::write(dir.join("state.json"), "{not json").expect("broken state");
    age_tree(&dir, days_ago(8.0));

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.removed, vec!["s-sibling".to_string()]);
    assert_eq!(report.kept["s-broken"].as_str(), "state-unreadable");
    assert!(exists(&dir.join("transcript.jsonl")));
}

#[test]
fn three_journals_in_shapes_no_rule_names_are_kept_as_unclassified() {
    let id = identity();
    let paths = &id.paths;
    let anchored_zero = journal_with_hand_written_state(
        paths,
        "anchored-zero",
        serde_json::json!({
            "total_completed_steps": 1,
            "reflected_completed_steps": 0,
            "steps_since_last_successful_reflection": 1,
            "reflected_through_message_id": "a1",
        }),
    );
    let counted_no_anchor = journal_with_hand_written_state(
        paths,
        "counted-no-anchor",
        serde_json::json!({
            "total_completed_steps": 1,
            "reflected_completed_steps": 1,
            "steps_since_last_successful_reflection": 0,
        }),
    );
    let negative = journal_with_hand_written_state(
        paths,
        "negative",
        serde_json::json!({
            "total_completed_steps": 2,
            "reflected_completed_steps": -1,
            "steps_since_last_successful_reflection": 3,
            "reflected_through_message_id": "a1",
        }),
    );
    let sibling = journal(
        paths,
        "sibling",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.kept["anchored-zero"].as_str(), "unclassified");
    assert_eq!(report.kept["counted-no-anchor"].as_str(), "unclassified");
    assert_eq!(report.kept["negative"].as_str(), "unclassified");
    assert_eq!(report.removed, vec!["sibling".to_string()]);
    for dir in [&anchored_zero, &counted_no_anchor, &negative] {
        assert!(exists(&dir.join("transcript.jsonl")));
    }
    assert!(!exists(&sibling));
}

#[test]
fn a_journal_whose_state_json_is_a_directory_is_kept_as_unreadable() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "s-a-broken",
        JournalShape::Fully,
        days_ago(10.0),
        JournalOptions::default(),
    );
    journal(
        paths,
        "s-b-full",
        JournalShape::Fully,
        days_ago(10.0),
        JournalOptions::default(),
    );
    std::fs::remove_file(dir.join("state.json")).expect("drop state file");
    std::fs::create_dir_all(dir.join("state.json")).expect("state as directory");
    age_tree(&dir, days_ago(10.0));

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.kept["s-a-broken"].as_str(), "state-unreadable");
    assert_eq!(report.removed, vec!["s-b-full".to_string()]);
}

#[test]
fn a_reserved_conversation_keeps_its_journal_untouched() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "X",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );
    journal(
        paths,
        "Z",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );
    write_json(
        &paths.reflection.join("active.lock"),
        &serde_json::json!({
            "runId": "reflection-run-2",
            "reservedAt": iso(days_ago(1.0)),
            "request": { "trigger": "manual", "conversationIds": ["X"] },
        }),
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.kept["X"].as_str(), "reserved");
    assert_eq!(report.removed, vec!["Z".to_string()]);
    assert!(exists(&dir.join("transcript.jsonl")));
}

#[test]
fn a_held_scheduler_lock_keeps_every_journal_as_scheduler_busy() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "Z",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );
    journal(
        paths,
        "F",
        JournalShape::Fully,
        days_ago(15.0),
        JournalOptions::default(),
    );
    std::fs::create_dir_all(&paths.locks).expect("locks dir");
    let lock_path = reflection_scheduler_lock_path(&paths.locks);
    let holder = create_lock_record(
        "reflection-scheduler",
        CreateLockRecordOptions {
            run_id: Some("reflection-run-9".to_string()),
        },
    )
    .expect("lock record");
    acquire_lock(&lock_path, &holder, &AcquireLockOptions::default()).expect("hold the lock");

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.kept["Z"].as_str(), "scheduler-busy");
    assert!(report.removed.is_empty());
    assert!(exists(&dir));

    release_lock(&lock_path, &holder).expect("release the lock");
}

#[test]
fn a_fully_reflected_journal_that_gained_rows_behind_its_state_is_kept_state_behind_transcript() {
    let id = identity();
    let paths = &id.paths;
    let sibling = journal(
        paths,
        "s-full",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );
    let dir = journal(
        paths,
        "s-stale",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );
    append_rows_behind_state(&dir, &second_step(), days_ago(8.0));
    age_tree(&dir, days_ago(8.0));

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.removed, vec!["s-full".to_string()]);
    assert_eq!(report.kept["s-stale"].as_str(), "state-behind-transcript");
    let transcript = std::fs::read_to_string(dir.join("transcript.jsonl")).expect("transcript");
    assert!(transcript.contains("second answer"));
    assert!(!exists(&sibling.join("transcript.jsonl")));
}

#[test]
fn a_partly_reflected_journal_that_gained_rows_behind_its_state_is_kept_state_behind_transcript() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "s-stale",
        JournalShape::Partly,
        days_ago(15.0),
        JournalOptions::default(),
    );
    append_rows_behind_state(&dir, &third_step(), days_ago(15.0));
    age_tree(&dir, days_ago(15.0));

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.kept["s-stale"].as_str(), "state-behind-transcript");
    assert!(report.removed.is_empty());
    let transcript = std::fs::read_to_string(dir.join("transcript.jsonl")).expect("transcript");
    assert!(transcript.contains("third answer"));
}

#[test]
fn rows_landing_behind_the_state_between_selection_and_claim_keep_the_transcript() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "s-race",
        JournalShape::Fully,
        days_ago(8.0),
        JournalOptions::default(),
    );
    let appended = std::cell::RefCell::new(false);
    let hook = |_session_id: &str| {
        if !*appended.borrow() {
            *appended.borrow_mut() = true;
            append_rows_behind_state(&dir, &second_step(), days_ago(8.0));
            age_tree(&dir, days_ago(8.0));
        }
    };
    let now = || NOW;
    let context = RetentionContext {
        before_journal_claim: Some(&hook),
        ..RetentionContext::new(paths, unreflected_policy(), &now)
    };
    let guard = LazyRetentionGuard::new(context);
    let report =
        prune_transcript_journals(&context, &guard, Some(&open_gate()), &DefaultJournalLock)
            .expect("prune journals");

    assert_eq!(report.kept["s-race"].as_str(), "state-behind-transcript");
    let transcript = std::fs::read_to_string(dir.join("transcript.jsonl")).expect("transcript");
    assert!(transcript.contains("second answer"));
}

#[test]
fn a_capture_between_the_guard_and_the_claim_keeps_the_journal_recent_and_its_rows_merge_intact() {
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "X",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );
    let captured = std::cell::RefCell::new(None);
    let hook = |session_id: &str| {
        if session_id != "X" {
            return;
        }
        let store = TranscriptJournal::new(TranscriptJournalOptions {
            now: Some(Box::new(|| iso(NOW))),
            ..TranscriptJournalOptions::new(dir.clone())
        });
        *captured.borrow_mut() = store
            .capture_reflection_snapshot(None)
            .expect("capture a snapshot");
    };
    let now = || NOW;
    let context = RetentionContext {
        before_journal_claim: Some(&hook),
        ..RetentionContext::new(paths, unreflected_policy(), &now)
    };
    let guard = LazyRetentionGuard::new(context);

    let report =
        prune_transcript_journals(&context, &guard, Some(&open_gate()), &DefaultJournalLock)
            .expect("prune journals");

    assert_eq!(report.kept["X"].as_str(), "recent");
    let snapshot = captured
        .into_inner()
        .expect("the claim hook captured a snapshot");
    TranscriptJournal::new(TranscriptJournalOptions::new(dir.clone()))
        .finalize_reflection(&snapshot, true)
        .expect("finalize the reflection");
    let state = state_of(&dir);
    assert_eq!(state["reflected_through_message_id"], "a1");
    assert_eq!(state["reflected_completed_steps"], 1);
    assert!(exists(&dir.join("transcript.jsonl")));
}

#[test]
fn a_capture_after_the_claim_is_null_for_both_a_removed_and_a_truncated_journal() {
    let id = identity();
    let paths = &id.paths;
    journal(
        paths,
        "X",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );
    journal(
        paths,
        "P",
        JournalShape::Partly,
        days_ago(15.0),
        JournalOptions::default(),
    );

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.removed, vec!["P".to_string(), "X".to_string()]);
    for session_id in ["X", "P"] {
        let store = TranscriptJournal::new(TranscriptJournalOptions::new(
            paths.transcripts.join(session_id),
        ));
        let captured = store
            .capture_reflection_snapshot(None)
            .expect("capture a snapshot");
        assert!(
            captured.is_none(),
            "{session_id} still captured a snapshot after its claim"
        );
    }
}

#[test]
fn a_journal_whose_last_capture_started_inside_the_window_is_kept_recent_though_its_files_look_old()
{
    let id = identity();
    let paths = &id.paths;
    let dir = journal(
        paths,
        "S",
        JournalShape::Never,
        days_ago(15.0),
        JournalOptions::default(),
    );
    let mut state = state_of(&dir).as_object().expect("state object").clone();
    state.insert(
        "last_reflection_started_at".to_string(),
        serde_json::Value::String(iso(days_ago(1.0))),
    );
    write_json(&dir.join("state.json"), &serde_json::Value::Object(state));
    age_tree(&dir, days_ago(15.0));

    let report = prune(paths, Some(&open_gate()));

    assert_eq!(report.kept["S"].as_str(), "recent");
    assert!(exists(&dir));
}
