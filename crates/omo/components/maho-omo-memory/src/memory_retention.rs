//! Age-based expiry of parked facts payloads, which the omo retention port (memory-core
//! `retention/`) does not cover: omo removes folded reflection runs, idle journals, sidecars,
//! consumed completions and legacy stores, but nothing in v5.1.79 expires the facts queue's parked
//! entries, so this module keeps exactly that behaviour and nothing else.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use memory_core::facts::{
    failures_schema::{
        FactsFailureRecord, FactsFailureState, parse_failures_file, render_failures_file,
    },
    schema::{FactsQueueLayout, queue_entry_suffix},
};
use memory_core::locks::{
    AcquireLockOptions, CreateLockRecordOptions, create_lock_record, facts_queue_lock_path,
    with_lock,
};
use memory_core::support::time::parse_rfc3339;

pub const FACTS_PARKED_RETENTION_DAYS: u64 = 14;
const FACTS_QUEUE_LOCK_WAIT_MS: u64 = 5_000;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone)]
pub struct RetentionOptions {
    pub parked_days: u64,
    pub now_ms: u64,
}

impl Default for RetentionOptions {
    fn default() -> Self {
        Self {
            parked_days: FACTS_PARKED_RETENTION_DAYS,
            now_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub parked_entries_removed: usize,
    pub parked_bytes: u64,
    pub errors: Vec<String>,
}

impl RetentionReport {
    pub fn is_empty(&self) -> bool {
        self.parked_entries_removed == 0
    }

    pub fn freed_bytes(&self) -> u64 {
        self.parked_bytes
    }
}

#[derive(Clone)]
pub struct RetentionPaths {
    pub facts_queue: PathBuf,
    pub locks: PathBuf,
}

pub fn prune_runtime(paths: &RetentionPaths, options: &RetentionOptions) -> RetentionReport {
    let mut report = RetentionReport::default();
    match prune_parked_facts(paths, options) {
        Ok((entries, bytes)) => {
            report.parked_entries_removed = entries;
            report.parked_bytes = bytes;
        }
        Err(error) => report.errors.push(format!("parked: {error}")),
    }
    report
}

fn cutoff_ms(options: &RetentionOptions, days: u64) -> u64 {
    options.now_ms.saturating_sub(days.saturating_mul(DAY_MS))
}

fn layout_of(queue_dir: &Path) -> FactsQueueLayout {
    FactsQueueLayout {
        queue_dir: queue_dir.to_path_buf(),
        parked_dir: queue_dir.join("parked"),
        cursor_dir: queue_dir.join("cursor"),
        consumed_path: queue_dir.join("consumed.json"),
        failures_path: queue_dir.join("failures.json"),
    }
}

fn prune_parked_facts(
    paths: &RetentionPaths,
    options: &RetentionOptions,
) -> io::Result<(usize, u64)> {
    let layout = layout_of(&paths.facts_queue);
    let lock_path = facts_queue_lock_path(&paths.locks);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let record = create_lock_record("facts-retention", CreateLockRecordOptions::default())
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let acquire = AcquireLockOptions {
        wait_timeout_ms: Some(FACTS_QUEUE_LOCK_WAIT_MS),
        ..Default::default()
    };
    with_lock(&lock_path, &record, &acquire, || {
        expired_parked_entries(&layout, options)
    })
    .map_err(|error| match error {
        memory_core::locks::WithLockError::Acquire(inner) => io::Error::other(format!("{inner:?}")),
        memory_core::locks::WithLockError::User(inner) => inner,
    })
}

fn expired_parked_entries(
    layout: &FactsQueueLayout,
    options: &RetentionOptions,
) -> io::Result<(usize, u64)> {
    let raw = match fs::read_to_string(&layout.failures_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => return Err(error),
    };
    // A corrupt ledger is doctor's problem; retention must never rewrite it.
    let Ok(file) = parse_failures_file(&raw) else {
        return Ok((0, 0));
    };
    let cutoff = cutoff_ms(options, options.parked_days);
    let (expired, keep): (Vec<FactsFailureRecord>, Vec<FactsFailureRecord>) =
        file.entries.into_iter().partition(|entry| {
            entry.state == FactsFailureState::Parked
                && entry_time_ms(entry).is_some_and(|at| at < cutoff)
        });
    if expired.is_empty() {
        return Ok((0, 0));
    }

    let mut freed = 0;
    for entry in &expired {
        let suffix = queue_entry_suffix(&entry.conversation_id, &entry.end_message_id);
        // Parked payloads normally live under `parked/`, but a ledger migrated before the
        // parked move existed still owns files in the queue root.
        for dir in [&layout.parked_dir, &layout.queue_dir] {
            let Ok(names) = fs::read_dir(dir) else {
                continue;
            };
            for name in names.filter_map(Result::ok) {
                let name = name.file_name().to_string_lossy().into_owned();
                if !name.ends_with(&suffix) {
                    continue;
                }
                // The name carries the enqueue instant: a requeued payload is newer.
                if payload_time_ms(&name).is_none_or(|at| at >= cutoff) {
                    continue;
                }
                let path = dir.join(&name);
                let Ok(meta) = fs::metadata(&path) else {
                    continue;
                };
                if fs::remove_file(&path).is_ok() {
                    freed += meta.len();
                }
            }
        }
    }

    let rendered = render_failures_file(&keep, &file.updated_at);
    let temp = layout.failures_path.with_extension("json.tmp");
    fs::write(&temp, rendered)?;
    fs::rename(&temp, &layout.failures_path)?;
    Ok((expired.len(), freed))
}

fn payload_time_ms(name: &str) -> Option<u64> {
    let compact = name.get(0..19)?;
    format_compact_timestamp(compact)
        .and_then(|iso| parse_rfc3339(&iso))
        .and_then(|millis| u64::try_from(millis).ok())
}

fn format_compact_timestamp(compact: &str) -> Option<String> {
    let bytes = compact.as_bytes();
    if bytes.len() != 19 || bytes[8] != b'T' || bytes[18] != b'Z' {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}.{}Z",
        &compact[0..4],
        &compact[4..6],
        &compact[6..8],
        &compact[9..11],
        &compact[11..13],
        &compact[13..15],
        &compact[15..18]
    ))
}

fn entry_time_ms(entry: &FactsFailureRecord) -> Option<u64> {
    let raw = entry.parked_at.as_deref().unwrap_or(&entry.last_failure_at);
    parse_rfc3339(raw).and_then(|millis| u64::try_from(millis).ok())
}

pub fn describe(report: &RetentionReport, options: &RetentionOptions) -> String {
    format!(
        "retention freed {} MB ({} parked entries >{}d)",
        report.freed_bytes() / (1024 * 1024),
        report.parked_entries_removed,
        options.parked_days,
    )
}
