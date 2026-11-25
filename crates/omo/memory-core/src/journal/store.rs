//! Durable append-only transcript journal and reflection state storage.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use crate::journal::cursor::{
    ReflectionSnapshot, ReflectionTranscriptState, capture_cursor_snapshot_with_max_bytes,
    count_completed_steps, derive_state, derive_state_with_total, finalize_cursor,
    initial_reflection_state, parse_state,
};
use crate::journal::entries::{
    TranscriptEntry, TranscriptProjection, parse_transcript_entry, project_transcript_entries,
};
use crate::journal::lock::{
    DefaultJournalLock, JournalLock, JournalLockTimeoutError, run_with_lock,
};

/// Typed errors produced by transcript journal operations.
#[derive(Debug)]
pub enum JournalError {
    LockTimeout(JournalLockTimeoutError),
    Aborted,
    InvalidTranscriptJournalRow,
    CorruptState(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockTimeout(err) => write!(f, "{err}"),
            Self::Aborted => write!(f, "The operation was aborted"),
            Self::InvalidTranscriptJournalRow => write!(f, "Invalid transcript journal row"),
            Self::CorruptState(msg) => write!(f, "Corrupt transcript state: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Json(err) => write!(f, "JSON serialization error: {err}"),
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LockTimeout(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for JournalError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for JournalError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl From<JournalLockTimeoutError> for JournalError {
    fn from(err: JournalLockTimeoutError) -> Self {
        Self::LockTimeout(err)
    }
}

/// Result of an append operation on the transcript journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendResult {
    pub appended: usize,
    pub skipped: usize,
}

/// Warning sink: a message plus its named fields.
pub type JournalWarn = Box<dyn Fn(&str, &[(&str, String)]) + Send + Sync>;

/// Configuration options for constructing a [`TranscriptJournal`].
pub struct TranscriptJournalOptions {
    pub journal_dir: PathBuf,
    pub now: Option<Box<dyn Fn() -> String + Send + Sync>>,
    pub lock: Option<Arc<dyn JournalLock>>,
    pub snapshot_max_bytes: Option<u64>,
    pub warn: Option<JournalWarn>,
}

impl TranscriptJournalOptions {
    /// Create new options for the given directory.
    pub fn new(journal_dir: impl Into<PathBuf>) -> Self {
        Self {
            journal_dir: journal_dir.into(),
            now: None,
            lock: None,
            snapshot_max_bytes: None,
            warn: None,
        }
    }
}

/// Cached transcript journal rows keyed by file byte length and mtime (#14351).
#[derive(Clone)]
struct TranscriptTailCache {
    byte_offset: u64,
    modified: Option<SystemTime>,
    entries: Vec<TranscriptEntry>,
    completed_steps: usize,
    source_ids: HashSet<String>,
}

#[derive(Default)]
struct JournalCache {
    transcript_tail: Option<TranscriptTailCache>,
    /// `state.json` as last read or written by this store; a no-op settle writes nothing (#14351).
    persisted_state_json: Option<String>,
}

fn parse_transcript_rows(raw: &str) -> Result<Vec<TranscriptEntry>, JournalError> {
    let mut entries = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|_| JournalError::InvalidTranscriptJournalRow)?;
        entries.push(parse_transcript_entry(value)?);
    }
    Ok(entries)
}

/// Durable append-only journal for conversation transcript entries.
pub struct TranscriptJournal {
    pub transcript_path: PathBuf,
    pub state_path: PathBuf,
    pub lock_path: PathBuf,
    journal_dir: PathBuf,
    now: Box<dyn Fn() -> String + Send + Sync>,
    lock: Arc<dyn JournalLock>,
    snapshot_max_bytes: Option<u64>,
    warn: Option<JournalWarn>,
    cache: Mutex<JournalCache>,
}

impl TranscriptJournal {
    /// Create a new transcript journal with the provided options.
    pub fn new(options: TranscriptJournalOptions) -> Self {
        let transcript_path = options.journal_dir.join("transcript.jsonl");
        let state_path = options.journal_dir.join("state.json");
        let lock_path = options.journal_dir.join("state.lock");
        let now = options
            .now
            .unwrap_or_else(|| Box::new(crate::support::time::now_iso));
        let lock = options.lock.unwrap_or_else(|| Arc::new(DefaultJournalLock));
        Self {
            transcript_path,
            state_path,
            lock_path,
            journal_dir: options.journal_dir,
            now,
            lock,
            snapshot_max_bytes: options.snapshot_max_bytes,
            warn: options.warn,
            cache: Mutex::new(JournalCache::default()),
        }
    }

    fn locked<T>(
        &self,
        cancel: Option<&dyn Fn() -> bool>,
        task: impl FnOnce() -> Result<T, JournalError>,
    ) -> Result<T, JournalError> {
        if let Some(c) = cancel
            && c()
        {
            return Err(JournalError::Aborted);
        }
        std::fs::create_dir_all(&self.journal_dir).map_err(JournalError::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&self.journal_dir, std::fs::Permissions::from_mode(0o700));
        }
        run_with_lock(&*self.lock, &self.lock_path, task, cancel)
    }

    fn load_transcript_cache_unlocked(&self) -> Result<TranscriptTailCache, JournalError> {
        let meta = match std::fs::metadata(&self.transcript_path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.transcript_path)
                    .map_err(JournalError::Io)?;
                let meta = std::fs::metadata(&self.transcript_path).map_err(JournalError::Io)?;
                let empty = TranscriptTailCache {
                    byte_offset: meta.len(),
                    modified: meta.modified().ok(),
                    entries: Vec::new(),
                    completed_steps: 0,
                    source_ids: HashSet::new(),
                };
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .transcript_tail = Some(empty.clone());
                return Ok(empty);
            }
            Err(err) => return Err(JournalError::Io(err)),
        };

        let size = meta.len();
        let modified = meta.modified().ok();
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .transcript_tail
            .clone();

        if let Some(mut cached) = cached {
            if size == cached.byte_offset && modified.is_some() && modified == cached.modified {
                return Ok(cached);
            }
            if size > cached.byte_offset
                && let Some(tail) = self.read_tail_entries_unlocked(size, cached.byte_offset)
            {
                cached.completed_steps += count_completed_steps(&tail);
                for entry in &tail {
                    cached.source_ids.insert(entry.source_line_id().to_owned());
                }
                cached.entries.extend(tail);
                cached.byte_offset = size;
                cached.modified = modified;
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .transcript_tail = Some(cached.clone());
                return Ok(cached);
            }
        }

        let raw = std::fs::read_to_string(&self.transcript_path).map_err(JournalError::Io)?;
        let entries = parse_transcript_rows(&raw)?;
        let completed_steps = count_completed_steps(&entries);
        let source_ids = entries
            .iter()
            .map(|entry| entry.source_line_id().to_owned())
            .collect();
        let fresh = TranscriptTailCache {
            byte_offset: raw.len() as u64,
            modified,
            entries,
            completed_steps,
            source_ids,
        };
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .transcript_tail = Some(fresh.clone());
        Ok(fresh)
    }

    fn read_tail_entries_unlocked(
        &self,
        size: u64,
        byte_offset: u64,
    ) -> Option<Vec<TranscriptEntry>> {
        let mut file = File::open(&self.transcript_path).ok()?;
        file.seek(SeekFrom::Start(byte_offset)).ok()?;
        let length = usize::try_from(size.saturating_sub(byte_offset)).ok()?;
        let mut buffer = vec![0u8; length];
        file.read_exact(&mut buffer).ok()?;
        let raw = std::str::from_utf8(&buffer).ok()?;
        parse_transcript_rows(raw).ok()
    }

    fn read_state_unlocked(&self) -> Result<ReflectionTranscriptState, JournalError> {
        match std::fs::read_to_string(&self.state_path) {
            Ok(content) => {
                let persisted = parse_state(&content)?;
                let persisted_json =
                    serde_json::to_string(&persisted).map_err(JournalError::Json)?;
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .persisted_state_json = Some(persisted_json);
                Ok(persisted)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .persisted_state_json = None;
                Ok(initial_reflection_state())
            }
            Err(err) => Err(JournalError::Io(err)),
        }
    }

    fn write_state_unlocked(
        &self,
        state: &ReflectionTranscriptState,
        total_completed_steps: usize,
        transcript_bytes: usize,
        entries: &[TranscriptEntry],
    ) -> Result<(), JournalError> {
        let mut derived = derive_state_with_total(state, total_completed_steps);
        derived.unreflected_bytes = Some(
            transcript_bytes.saturating_sub(derived.reflected_through_byte_offset.unwrap_or(0)),
        );
        if derived.reflected_through_byte_offset.is_none() {
            derived.reflected_through_byte_offset = derived
                .reflected_through_message_id
                .as_deref()
                .map(|id| crate::journal::cursor::reflected_through_byte_offset(entries, id));
        }
        let derived_json = serde_json::to_string(&derived).map_err(JournalError::Json)?;
        if self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .persisted_state_json
            .as_deref()
            == Some(derived_json.as_str())
        {
            return Ok(());
        }
        let temp_filename = format!(
            "{}.tmp-{}",
            self.state_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            crate::support::random::random_uuid()
        );
        let temporary_path = self.journal_dir.join(temp_filename);
        let json = serde_json::to_string_pretty(&derived).map_err(JournalError::Json)?;
        std::fs::write(&temporary_path, format!("{json}\n")).map_err(JournalError::Io)?;
        if let Err(err) = std::fs::rename(&temporary_path, &self.state_path) {
            let _ = std::fs::remove_file(&temporary_path);
            return Err(JournalError::Io(err));
        }
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .persisted_state_json = Some(derived_json);
        Ok(())
    }

    fn append_unlocked(&self, entries: &[TranscriptEntry]) -> Result<AppendResult, JournalError> {
        let mut cache = self.load_transcript_cache_unlocked()?;
        let existing_rows = cache.entries.len();
        let mut fresh = Vec::new();
        let mut skipped = 0;
        for entry in entries {
            let id = entry.source_line_id();
            if cache.source_ids.contains(id) {
                skipped += 1;
            } else {
                cache.source_ids.insert(id.to_owned());
                fresh.push(entry);
            }
        }
        let appended = fresh.len();
        if !fresh.is_empty() {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.transcript_path)
                .map_err(JournalError::Io)?;
            for entry in fresh.iter().copied() {
                let json = serde_json::to_string(entry).map_err(JournalError::Json)?;
                writeln!(file, "{json}").map_err(JournalError::Io)?;
            }
            file.flush().map_err(JournalError::Io)?;
            let meta = std::fs::metadata(&self.transcript_path).map_err(JournalError::Io)?;
            let fresh_completed = fresh
                .iter()
                .filter(|entry| count_completed_steps(std::slice::from_ref(*entry)) == 1)
                .count();
            cache.completed_steps += fresh_completed;
            cache.entries.extend(fresh.into_iter().cloned());
            cache.byte_offset = meta.len();
            cache.modified = meta.modified().ok();
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .transcript_tail = Some(cache.clone());
        }
        let mut current_state = self.read_state_unlocked()?;
        if existing_rows == 0 && appended > 0 && current_state.transcript_pruned_at.is_some() {
            current_state = self.reanchor_after_prune(current_state, &cache.entries);
        }
        self.write_state_unlocked(
            &current_state,
            cache.completed_steps,
            cache.byte_offset as usize,
            &cache.entries,
        )?;
        Ok(AppendResult { appended, skipped })
    }

    /// A transcript removed by retention is rebuilt from the harness projection, which may be
    /// shorter than the original: senpi's `getBranch()` after a compaction holds only the messages
    /// from the compaction's first kept entry on. The reflected count is recomputed against the
    /// rebuilt rows. When the anchor is no longer projected it lay before the kept window, so every
    /// rebuilt row is after it and unreflected: the cursor restarts at the first row instead of
    /// clamping a stale count.
    fn reanchor_after_prune(
        &self,
        mut state: ReflectionTranscriptState,
        entries: &[TranscriptEntry],
    ) -> ReflectionTranscriptState {
        state.transcript_pruned_at = None;
        state.reflected_through_byte_offset = None;
        let Some(anchor) = state.reflected_through_message_id.clone() else {
            return state;
        };
        let anchor_index = entries
            .iter()
            .rposition(|entry| entry.source_message_id() == anchor);
        if let Some(index) = anchor_index {
            state.reflected_completed_steps = count_completed_steps(&entries[..=index]);
            state.reflected_through_byte_offset = Some(
                crate::journal::cursor::reflected_through_byte_offset(entries, &anchor),
            );
            return state;
        }
        if let Some(warn) = &self.warn {
            warn(
                "journal anchor lost after prune",
                &[
                    ("journalDir", self.journal_dir.display().to_string()),
                    ("reflectedThroughMessageId", anchor),
                ],
            );
        }
        state.reflected_through_message_id = None;
        state.reflected_completed_steps = 0;
        state
    }

    /// Reconcile structured transcript messages by projecting and appending new rows.
    pub fn reconcile(
        &self,
        messages: &[TranscriptProjection],
    ) -> Result<AppendResult, JournalError> {
        self.reconcile_with_cancel(messages, None)
    }

    /// [`Self::reconcile`] that honours `cancel` (the upstream abort signal) before and while it
    /// waits for the journal lock.
    pub fn reconcile_with_cancel(
        &self,
        messages: &[TranscriptProjection],
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<AppendResult, JournalError> {
        let captured_at = (self.now)();
        let entries: Vec<TranscriptEntry> = messages
            .iter()
            .flat_map(|m| project_transcript_entries(m, &captured_at))
            .collect();
        self.append_with_cancel(&entries, cancel)
    }

    /// Append entries directly to the journal, deduplicating by source line ID.
    pub fn append(&self, entries: &[TranscriptEntry]) -> Result<AppendResult, JournalError> {
        self.append_with_cancel(entries, None)
    }

    /// [`Self::append`] that honours `cancel`: it fails up front when already cancelled and while
    /// it waits for the journal lock.
    pub fn append_with_cancel(
        &self,
        entries: &[TranscriptEntry],
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<AppendResult, JournalError> {
        if cancel.is_some_and(|c| c()) {
            return Err(JournalError::Aborted);
        }
        self.locked(cancel, || self.append_unlocked(entries))
    }

    /// Read all validated transcript entries from the journal file.
    pub fn read_entries(&self) -> Result<Vec<TranscriptEntry>, JournalError> {
        self.locked(None, || Ok(self.load_transcript_cache_unlocked()?.entries))
    }

    pub fn read_entries_snapshot(&self) -> Result<Vec<TranscriptEntry>, JournalError> {
        let raw = match std::fs::read_to_string(&self.transcript_path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(JournalError::Io(error)),
        };
        Ok(raw
            .lines()
            .filter_map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).ok()?;
                let row = value.as_object()?;
                if !matches!(
                    row.get("kind").and_then(serde_json::Value::as_str),
                    Some("user" | "assistant" | "reasoning" | "error" | "tool_call")
                ) || !row
                    .get("captured_at")
                    .is_some_and(serde_json::Value::is_string)
                    || !row
                        .get("source_line_id")
                        .is_some_and(serde_json::Value::is_string)
                    || !row
                        .get("source_message_id")
                        .is_some_and(serde_json::Value::is_string)
                {
                    return None;
                }
                parse_transcript_entry(value).ok()
            })
            .collect())
    }

    pub fn record_reflection_failure_at(&self, now_millis: i64) -> Result<(), JournalError> {
        self.locked(None, || {
            let cache = self.load_transcript_cache_unlocked()?;
            let mut state = self.read_state_unlocked()?;
            let failures = state.consecutive_failures.unwrap_or(0).saturating_add(1);
            let exponent = failures.saturating_sub(1).min(6);
            let delay_seconds = (5u64.saturating_mul(1u64 << exponent)).min(300);
            let next = now_millis.saturating_add((delay_seconds * 1000) as i64);
            state.consecutive_failures = Some(failures);
            state.next_eligible_at = Some(crate::support::time::format_rfc3339_millis(next));
            self.write_state_unlocked(
                &state,
                cache.completed_steps,
                cache.byte_offset as usize,
                &cache.entries,
            )
        })
    }

    pub fn backfill_reflection_byte_offset(
        &self,
        entries: &[TranscriptEntry],
    ) -> Result<ReflectionTranscriptState, JournalError> {
        self.locked(None, || {
            let cache = self.load_transcript_cache_unlocked()?;
            let mut state = self.read_state_unlocked()?;
            if state.reflected_through_byte_offset.is_none() {
                state.reflected_through_byte_offset = Some(
                    state
                        .reflected_through_message_id
                        .as_deref()
                        .map_or(0, |id| {
                            crate::journal::cursor::reflected_through_byte_offset(entries, id)
                        }),
                );
            }
            self.write_state_unlocked(
                &state,
                count_completed_steps(entries),
                cache.byte_offset as usize,
                entries,
            )?;
            self.read_state_unlocked()
        })
    }

    pub fn record_reflection_failure(&self) -> Result<(), JournalError> {
        self.record_reflection_failure_at(crate::support::time::now_millis())
    }

    /// Retrieve the current reflection transcript state, recalculating step counters.
    pub fn get_state(&self) -> Result<ReflectionTranscriptState, JournalError> {
        self.locked(None, || {
            let cache = self.load_transcript_cache_unlocked()?;
            let current_state = self.read_state_unlocked()?;
            let state = derive_state_with_total(&current_state, cache.completed_steps);
            self.write_state_unlocked(
                &state,
                cache.completed_steps,
                cache.byte_offset as usize,
                &cache.entries,
            )?;
            Ok(state)
        })
    }

    /// Update the pending compaction flag in the reflection state file.
    pub fn set_pending_compaction(&self, pending: bool) -> Result<(), JournalError> {
        self.locked(None, || {
            let cache = self.load_transcript_cache_unlocked()?;
            let mut state = self.read_state_unlocked()?;
            state.pending_compaction = Some(pending);
            self.write_state_unlocked(
                &state,
                cache.completed_steps,
                cache.byte_offset as usize,
                &cache.entries,
            )
        })
    }

    /// Capture an unreflected snapshot of entries if available.
    pub fn capture_reflection_snapshot(
        &self,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<Option<ReflectionSnapshot>, JournalError> {
        self.locked(cancel, || {
            let cache = self.load_transcript_cache_unlocked()?;
            let current_state = self.read_state_unlocked()?;
            let state = derive_state(&current_state, &cache.entries);
            let snapshot = capture_cursor_snapshot_with_max_bytes(
                &cache.entries,
                &state,
                self.snapshot_max_bytes,
            );
            let Some(snapshot) = snapshot else {
                return Ok(None);
            };
            if let Some(c) = cancel
                && c()
            {
                return Err(JournalError::Aborted);
            }
            let mut updated_state = state;
            updated_state.last_reflection_started_at = Some((self.now)());
            if current_state.reflected_through_byte_offset.is_none() {
                updated_state.reflected_through_byte_offset = Some(
                    current_state
                        .reflected_through_message_id
                        .as_deref()
                        .map_or(0, |id| {
                            crate::journal::cursor::reflected_through_byte_offset(
                                &cache.entries,
                                id,
                            )
                        }),
                );
            }
            self.write_state_unlocked(
                &updated_state,
                cache.completed_steps,
                cache.byte_offset as usize,
                &cache.entries,
            )?;
            Ok(Some(snapshot))
        })
    }

    /// Flush and fsync transcript rows, state file, and journal directory.
    pub fn flush(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<(), JournalError> {
        let is_aborted = || cancel.is_some_and(|c| c());
        self.locked(cancel, || {
            if is_aborted() {
                return Ok(());
            }
            sync_journal_file(&self.transcript_path)?;
            if is_aborted() {
                return Ok(());
            }
            sync_journal_file(&self.state_path)?;
            if is_aborted() {
                return Ok(());
            }
            sync_journal_directory(&self.journal_dir)?;
            Ok(())
        })
    }

    /// Finalize a reflection run using the given snapshot and completion status.
    pub fn finalize_reflection(
        &self,
        snapshot: &ReflectionSnapshot,
        success: bool,
    ) -> Result<(), JournalError> {
        self.locked(None, || {
            let cache = self.load_transcript_cache_unlocked()?;
            let current_state = self.read_state_unlocked()?;
            let state = derive_state(&current_state, &cache.entries);
            let final_state =
                finalize_cursor(&state, &cache.entries, snapshot, success, &(self.now)());
            self.write_state_unlocked(
                &final_state,
                cache.completed_steps,
                cache.byte_offset as usize,
                &cache.entries,
            )
        })
    }
}

/// Kinds Windows raises for a directory handle, plus the absent-file case
/// (upstream `SKIPPABLE_SYNC_CODES`: ENOENT, EISDIR, EPERM, EACCES, EINVAL).
fn is_skippable_sync_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::IsADirectory
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::InvalidInput
    )
}

fn sync_path(path: &Path) -> Result<(), JournalError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) if is_skippable_sync_error(err.kind()) => return Ok(()),
        Err(err) => return Err(JournalError::Io(err)),
    };
    match file.sync_all() {
        Ok(()) => Ok(()),
        Err(err) if is_skippable_sync_error(err.kind()) => Ok(()),
        Err(err) => Err(JournalError::Io(err)),
    }
}

/// Fsync a journal file if it exists.
pub fn sync_journal_file(path: &Path) -> Result<(), JournalError> {
    sync_path(path)
}

/// Fsync the journal directory if supported by the platform.
pub fn sync_journal_directory(dir: &Path) -> Result<(), JournalError> {
    sync_path(dir)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
