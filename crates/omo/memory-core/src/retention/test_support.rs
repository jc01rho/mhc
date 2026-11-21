//! Fixtures for the runtime retention tests: a fake identity under a temp memory root, a pinned
//! clock, and writers for every store the pass reads. Ages are set with `filetime` against the
//! pinned clock, never wall time.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use crate::identity::layout::{MemoryIdentityPaths, build_identity_paths};
use crate::journal::TranscriptProjection;
use crate::journal::store::{TranscriptJournal, TranscriptJournalOptions};
use crate::support::time::format_rfc3339_millis;

pub(super) const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// `Date.parse("2026-11-05T00:00:00.000Z")`.
pub(super) const NOW: i64 = 1_793_836_800_000;

pub(super) fn now() -> i64 {
    NOW
}

pub(super) fn days_ago(days: f64) -> i64 {
    NOW - (days * DAY_MS as f64) as i64
}

pub(super) fn iso(millis: i64) -> String {
    format_rfc3339_millis(millis)
}

/// A temp memory root plus the identity paths under it; the root is deleted on drop.
pub(super) struct Identity {
    _root: tempfile::TempDir,
    pub(super) paths: MemoryIdentityPaths,
}

pub(super) fn identity() -> Identity {
    let root = tempfile::tempdir().expect("temp memory root");
    let paths = build_identity_paths(root.path(), "agent-test");
    Identity { _root: root, paths }
}

pub(super) fn write_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture directory");
    }
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(value).expect("fixture json")
    );
    std::fs::write(path, body).expect("fixture json file");
}

/// Sets the mtime of `path` and everything under it, children first so writes do not move the
/// directory afterwards.
pub(super) fn age_tree(path: &Path, at: i64) {
    let metadata = std::fs::symlink_metadata(path).expect("aged path");
    if metadata.is_dir() {
        for name in read_names(path) {
            age_tree(&path.join(name), at);
        }
    }
    filetime::set_file_mtime(
        path,
        filetime::FileTime::from_unix_time(at.div_euclid(1000), 0),
    )
    .expect("age path");
}

pub(super) fn read_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

pub(super) fn runs_dir(paths: &MemoryIdentityPaths) -> PathBuf {
    paths.reflection.join("runs")
}

pub(super) fn completions_dir(paths: &MemoryIdentityPaths) -> PathBuf {
    paths.reflection.join("completions")
}

/// Names left behind by a claim that never finished its delete.
pub(super) fn tombstones(dir: &Path) -> Vec<String> {
    read_names(dir)
        .into_iter()
        .filter(|name| name.starts_with(".prune-"))
        .collect()
}

#[derive(Default)]
pub(super) struct RunShape {
    pub(super) outcome: Option<String>,
    pub(super) finished_at: Option<i64>,
    pub(super) mtime: Option<i64>,
    pub(super) final_run_id: Option<String>,
    pub(super) sentinel: Option<Sentinel>,
    pub(super) read_only_payload: bool,
    /// Write time of an abandoned/quarantined sentinel; defaults to `finished_at`.
    pub(super) sentinel_written_at: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Sentinel {
    Final,
    Abandoned,
    Quarantined,
    None,
}

/// A reflection run dir with a ledger, a child log and the requested terminal sentinel.
pub(super) fn reflection_run(
    paths: &MemoryIdentityPaths,
    run_id: &str,
    shape: RunShape,
) -> PathBuf {
    let dir = runs_dir(paths).join(run_id);
    std::fs::create_dir_all(&dir).expect("run directory");
    write_json(&dir.join("ledger.json"), &json!({ "runId": run_id }));
    std::fs::write(dir.join("child-1.log"), "child output\n").expect("child log");
    let finished_at = shape.finished_at.unwrap_or_else(|| days_ago(8.0));
    let sentinel = shape.sentinel.unwrap_or(Sentinel::Final);
    match sentinel {
        Sentinel::Final => write_json(
            &dir.join("final.json"),
            &json!({
                "runId": shape.final_run_id.as_deref().unwrap_or(run_id),
                "outcome": shape.outcome.as_deref().unwrap_or("merged"),
                "finishedAt": iso(finished_at),
            }),
        ),
        Sentinel::Abandoned => write_json(
            &dir.join("abandoned.json"),
            &json!({
                "runId": run_id,
                "reason": "launch_interrupted",
                "generation": iso(finished_at),
                "abandonedAt": iso(shape.sentinel_written_at.unwrap_or(finished_at)),
            }),
        ),
        Sentinel::Quarantined => write_json(
            &dir.join("quarantined.json"),
            &json!({
                "runId": run_id,
                "reason": "ledger_unreadable",
                "generation": iso(finished_at),
                "quarantinedAt": iso(shape.sentinel_written_at.unwrap_or(finished_at)),
            }),
        ),
        Sentinel::None => {}
    }
    if shape.read_only_payload {
        read_only_payload(&dir);
    }
    age_tree(&dir, shape.mtime.unwrap_or(finished_at));
    dir
}

/// The three payload files the reflection worker chmods to 0o400.
pub(super) fn read_only_payload(dir: &Path) {
    for name in [
        "reflection-persona.md",
        "reflection-task.md",
        "transcript-payload.json",
    ] {
        let path = dir.join(name);
        std::fs::write(&path, "payload\n").expect("payload file");
        set_read_only(&path);
    }
}

#[cfg(unix)]
fn set_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).expect("read-only mode");
}

#[cfg(not(unix))]
fn set_read_only(_path: &Path) {}

#[derive(Default)]
pub(super) struct CompletionShape {
    pub(super) status: Option<String>,
    pub(super) finished_at: Option<i64>,
}

pub(super) fn completion(
    paths: &MemoryIdentityPaths,
    run_id: &str,
    shape: CompletionShape,
) -> PathBuf {
    let path = completions_dir(paths).join(format!("{run_id}.json"));
    let finished_at = shape.finished_at.unwrap_or_else(|| days_ago(31.0));
    write_json(
        &path,
        &json!({
            "schemaVersion": 1,
            "runId": run_id,
            "identity": "agent-test",
            "category": "quick",
            "conversationIds": ["conversation-a"],
            "trigger": "manual",
            "outcome": "merged",
            "startedAt": iso(finished_at - 60_000),
            "finishedAt": iso(finished_at),
            "durationMs": 60_000,
            "consecutiveFailures": 0,
            "delivery": { "status": shape.status.as_deref().unwrap_or("consumed") },
        }),
    );
    age_tree(&path, finished_at);
    path
}

pub(super) fn reservation(paths: &MemoryIdentityPaths, file: &str, run_id: &str) {
    write_json(
        &paths.reflection.join(file),
        &json!({
            "runId": run_id,
            "reservedAt": iso(days_ago(1.0)),
            "request": { "trigger": "manual", "conversationIds": [] },
        }),
    );
}

pub(super) fn run_id_mark(paths: &MemoryIdentityPaths, high_water: u64) {
    write_json(
        &paths.reflection.join("run-id.json"),
        &json!({ "version": 1, "highWater": high_water }),
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum JournalShape {
    Fully,
    Partly,
    Never,
    NoSteps,
}

#[derive(Default)]
pub(super) struct JournalOptions {
    pub(super) pending_compaction: bool,
}

pub(super) fn user(message_id: &str, text: &str) -> TranscriptProjection {
    TranscriptProjection::User {
        message_id: message_id.to_string(),
        text: text.to_string(),
    }
}

pub(super) fn assistant(message_id: &str, text: &str) -> TranscriptProjection {
    TranscriptProjection::Assistant {
        message_id: message_id.to_string(),
        text_blocks: vec![text.to_string()],
        reasoning_blocks: Vec::new(),
        tool_calls: Vec::new(),
    }
}

fn first_two() -> Vec<TranscriptProjection> {
    vec![user("u1", "first"), assistant("a1", "first answer")]
}

fn all_four() -> Vec<TranscriptProjection> {
    vec![
        user("u1", "first"),
        assistant("a1", "first answer"),
        user("u2", "second"),
        assistant("a2", "second answer"),
    ]
}

/// A journal built through `TranscriptJournal`, then aged to `idle_since`.
pub(super) fn journal(
    paths: &MemoryIdentityPaths,
    session_id: &str,
    shape: JournalShape,
    idle_since: i64,
    options: JournalOptions,
) -> PathBuf {
    let dir = paths.transcripts.join(session_id);
    let store = TranscriptJournal::new(TranscriptJournalOptions {
        now: Some(Box::new(move || iso(idle_since))),
        ..TranscriptJournalOptions::new(dir.clone())
    });
    if shape == JournalShape::NoSteps {
        store
            .reconcile(&[user("u1", "hello")])
            .expect("reconcile no-steps journal");
    } else {
        store.reconcile(&first_two()).expect("reconcile journal");
        if shape != JournalShape::Never {
            let snapshot = store
                .capture_reflection_snapshot(None)
                .expect("capture snapshot")
                .expect("expected a snapshot");
            store
                .finalize_reflection(&snapshot, true)
                .expect("finalize reflection");
        }
        if shape == JournalShape::Partly {
            store.reconcile(&all_four()).expect("reconcile second step");
        }
    }
    if options.pending_compaction {
        store
            .set_pending_compaction(true)
            .expect("pending compaction");
    }
    age_tree(&dir, idle_since);
    dir
}

/// Appends rows to a journal's transcript without touching its state, the shape a platform that
/// refused the state rename leaves behind (REVIEW-6 M1).
pub(super) fn append_rows_behind_state(
    dir: &Path,
    messages: &[TranscriptProjection],
    captured_at: i64,
) {
    let mut body = String::new();
    for message in messages {
        for entry in crate::journal::project_transcript_entries(message, &iso(captured_at)) {
            body.push_str(&serde_json::to_string(&entry).expect("transcript row"));
            body.push('\n');
        }
    }
    let path = dir.join("transcript.jsonl");
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(&body);
    std::fs::write(&path, existing).expect("append transcript rows");
}

pub(super) fn second_step() -> Vec<TranscriptProjection> {
    vec![user("u2", "second"), assistant("a2", "second answer")]
}

pub(super) fn third_step() -> Vec<TranscriptProjection> {
    vec![assistant("a5", "third answer")]
}

pub(super) fn encoded_session(session_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(session_id.as_bytes())
}

pub(super) fn sidecar(paths: &MemoryIdentityPaths, session_id: &str, at: i64) -> PathBuf {
    let dir = paths
        .recall
        .join("sidecars")
        .join(encoded_session(session_id));
    std::fs::create_dir_all(&dir).expect("sidecar directory");
    std::fs::write(dir.join("wakes.ndjson"), "{}\n").expect("sidecar wakes");
    age_tree(&dir, at);
    dir
}

/// A pid the kernel no longer knows: a child that already exited and was reaped.
pub(super) fn exited_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn a short-lived child");
    let pid = child.id();
    child.wait().expect("reap the short-lived child");
    pid
}

/// Reads a JSON file the way the TypeScript assertions do.
pub(super) fn read_json(path: &Path) -> Value {
    let raw = std::fs::read_to_string(path).expect("read json file");
    serde_json::from_str(&raw).expect("parse json file")
}

pub(super) fn exists(path: &Path) -> bool {
    path.exists()
}
