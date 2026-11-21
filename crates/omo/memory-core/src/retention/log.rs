//! Port of `retention/log.ts` (#12932): what a retention pass removed, one JSON line per pass that
//! removed anything, appended to `runtime/retention-log.jsonl`. A pass that removes nothing writes
//! nothing, so a visit of an identity with nothing to remove still leaves its mtimes alone. The log
//! is bounded: past `max_bytes` the file becomes `retention-log.1.jsonl` (one previous generation)
//! before the next line is written, and each store lists at most `max_names_per_store` names with
//! the rest counted. The log is a record, not state: no pass reads it, and the doctors read only
//! its last entry.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{Map, Value, json};

use super::policy::{append_private_file, is_gone};
use crate::fs::resilient::rename;
use crate::support::time::format_rfc3339_millis;

pub const RETENTION_LOG_FILE: &str = "retention-log.jsonl";
pub const RETENTION_LOG_PREVIOUS_FILE: &str = "retention-log.1.jsonl";
const DEFAULT_MAX_BYTES: u64 = 512 * 1024;
const DEFAULT_MAX_NAMES_PER_STORE: usize = 200;
/// Half the reader's tail, so the newest whole line always fits in it.
const MAX_LINE_BYTES: usize = 32 * 1024;
/// The doctors read this much of the log's end for the last entry.
const TAIL_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionLogInput {
    /// Pass time, epoch ms.
    pub at: i64,
    /// Names the pass deleted, per store (`runs`, `journals`, `cap:<tier>`, ...); empty stores are
    /// left out.
    pub removed: Vec<(String, Vec<String>)>,
    /// Bytes the size cap evicted, when it evicted anything.
    pub cap_evicted_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionLogLimits {
    pub max_bytes: Option<u64>,
    pub max_names_per_store: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionLogEntry {
    /// ISO time of the pass.
    pub at: String,
    /// Items the pass removed, every store together.
    pub total: u64,
}

pub fn append_retention_log(
    runtime_dir: &Path,
    input: &RetentionLogInput,
    limits: &RetentionLogLimits,
) -> io::Result<()> {
    let stores: Vec<&(String, Vec<String>)> = input
        .removed
        .iter()
        .filter(|(_, names)| !names.is_empty())
        .collect();
    let total: usize = stores.iter().map(|(_, names)| names.len()).sum();
    if total == 0 {
        return Ok(());
    }
    let mut max_names = limits
        .max_names_per_store
        .unwrap_or(DEFAULT_MAX_NAMES_PER_STORE);
    let line = loop {
        let line = format_line(input, total, &stores, max_names);
        if line.len() <= MAX_LINE_BYTES || max_names == 0 {
            break line;
        }
        max_names /= 2;
    };
    let path = runtime_dir.join(RETENTION_LOG_FILE);
    let size = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    if size > 0 && size + line.len() as u64 > limits.max_bytes.unwrap_or(DEFAULT_MAX_BYTES) {
        rename(&path, &runtime_dir.join(RETENTION_LOG_PREVIOUS_FILE))?;
    }
    append_private_file(&path, line.as_bytes())
}

fn format_line(
    input: &RetentionLogInput,
    total: usize,
    stores: &[&(String, Vec<String>)],
    max_names: usize,
) -> String {
    let mut removed = Map::new();
    let mut more = Map::new();
    for (store, names) in stores.iter().map(|entry| (&entry.0, &entry.1)) {
        let kept: Vec<&String> = names.iter().take(max_names).collect();
        removed.insert(store.clone(), json!(kept));
        if names.len() > max_names {
            more.insert(store.clone(), json!(names.len() - max_names));
        }
    }
    let mut line = Map::new();
    line.insert("v".to_string(), json!(1));
    line.insert("at".to_string(), json!(format_rfc3339_millis(input.at)));
    line.insert("total".to_string(), json!(total));
    line.insert("removed".to_string(), Value::Object(removed));
    if !more.is_empty() {
        line.insert("more".to_string(), Value::Object(more));
    }
    if let Some(bytes) = input.cap_evicted_bytes.filter(|bytes| *bytes > 0) {
        line.insert("capEvictedBytes".to_string(), json!(bytes));
    }
    format!("{}\n", Value::Object(line))
}

/// The newest whole entry of the log, else of the previous generation; `None` when neither has one.
pub fn read_last_retention_log_entry(runtime_dir: &Path) -> io::Result<Option<RetentionLogEntry>> {
    for file in [RETENTION_LOG_FILE, RETENTION_LOG_PREVIOUS_FILE] {
        if let Some(entry) = last_entry(&read_tail(&runtime_dir.join(file))?) {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

fn read_tail(path: &Path) -> io::Result<String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if is_gone(&error) => return Ok(String::new()),
        Err(error) => return Err(error),
    };
    let size = file.metadata()?.len();
    let length = size.min(TAIL_BYTES);
    file.seek(SeekFrom::Start(size - length))?;
    let mut buffer = Vec::with_capacity(length as usize);
    file.take(length).read_to_end(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn last_entry(tail: &str) -> Option<RetentionLogEntry> {
    for line in tail.rsplit('\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let (Some(at), Some(total)) = (
            parsed.get("at").and_then(Value::as_str),
            parsed.get("total").and_then(Value::as_u64),
        ) {
            return Some(RetentionLogEntry {
                at: at.to_string(),
                total,
            });
        }
    }
    None
}
