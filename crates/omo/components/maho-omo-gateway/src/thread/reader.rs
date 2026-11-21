//! Bounded transcript reader for `thread_read`.
//!
//! Upstream counterpart: `thread/reader.ts` and `thread/read-budget.ts`. The result names which source answered: a live
//! host and the durable file are not interchangeable, so on the JSONL path `source_incomplete` is true exactly when no
//! live host holds the session. An empty file is an empty transcript, never an error; only a missing file is
//! `not_found`. A read hands back one payload plus an opaque cursor bound to the transcript's revision.

use std::path::Path;

use serde_json::Value;

use super::contracts_results::ThreadReadSource;
use super::errors::{ThreadToolFailure, thread_tool_failure};

/// Default number of trailing entries a first read takes.
pub const THREAD_TAIL_DEFAULT_ITEMS: usize = 60;
pub const THREAD_READ_DEFAULT_BYTES: usize = 131_072;
pub const THREAD_READ_MAX_BYTES: usize = 1_048_576;

const CURSOR_VERSION: &str = "t1";
const OLDER_CURSOR_VERSION: &str = "t1o";

/// Where a read comes from: the entries a live host holds (newest last), or the session's JSONL file.
pub enum ReadSource<'a> {
    Live(&'a [Value]),
    Jsonl {
        path: &'a Path,
        live_host_present: bool,
    },
}

/// One bounded slice of a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSlice {
    pub source: ThreadReadSource,
    pub source_incomplete: bool,
    pub items: Vec<Value>,
    pub truncated: bool,
    pub next_cursor: Option<String>,
}

/// `Start` fills the budget oldest first; `End` from the newest entry backward (a first read, which must end at the
/// latest message, #14173), still returning the entries oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetFrom {
    Start,
    End,
}

/// The entries a slice returns: in order until the next would pass `max_bytes`. The first entry taken always ships even
/// when it alone overruns the budget, so a page never stalls on an oversized entry.
pub fn take_within_budget(window: &[Value], max_bytes: usize, from: BudgetFrom) -> Vec<Value> {
    let mut selected: Vec<Value> = Vec::new();
    let mut encoded = 0usize;
    let ordered: Vec<&Value> = match from {
        BudgetFrom::Start => window.iter().collect(),
        BudgetFrom::End => window.iter().rev().collect(),
    };
    for entry in ordered {
        let size = serde_json::to_string(entry).map_or(0, |text| text.len());
        if !selected.is_empty() && encoded + size > max_bytes {
            break;
        }
        selected.push(entry.clone());
        encoded += size;
    }
    if from == BudgetFrom::End {
        selected.reverse();
    }
    selected
}

fn identity_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(char::from(DIGITS[(value % 36) as usize]));
        value /= 36;
    }
    digits.iter().rev().collect()
}

/// Content revision a cursor is bound to: the entry count plus an FNV-1a hash of every entry's identity (`id` and
/// `timestamp`). An append changes the count, a rewrite changes an identity, a truncation both: each invalidates the
/// cursor instead of silently re-slicing a different transcript.
pub fn revision_of(entries: &[Value]) -> String {
    let mut hash: u32 = 2_166_136_261;
    for entry in entries {
        let identity = format!(
            "{}|{}",
            identity_text(entry.get("id")),
            identity_text(entry.get("timestamp"))
        );
        for unit in identity.encode_utf16() {
            hash ^= u32::from(unit);
            hash = hash.wrapping_mul(16_777_619);
        }
    }
    format!("{}.{}", entries.len(), base36(hash))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Cursor {
    /// Continues after `offset`.
    Forward { revision: String, offset: usize },
    /// The entries of a tail window before `before`, down to the window's first entry `floor`.
    Older {
        revision: String,
        before: usize,
        floor: usize,
    },
}

impl Cursor {
    fn revision(&self) -> &str {
        match self {
            Self::Forward { revision, .. } | Self::Older { revision, .. } => revision,
        }
    }
}

fn encode_cursor(cursor: &Cursor) -> String {
    let text = match cursor {
        Cursor::Forward { revision, offset } => format!("{CURSOR_VERSION}:{revision}:{offset}"),
        Cursor::Older {
            revision,
            before,
            floor,
        } => format!("{OLDER_CURSOR_VERSION}:{revision}:{before}:{floor}"),
    };
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(raw: &str) -> Option<String> {
    if raw.is_empty() || !raw.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..raw.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(raw.get(index..index + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

fn decode_cursor(raw: &str) -> Option<Cursor> {
    let decoded = decode_hex(raw)?;
    let mut parts = decoded.split(':');
    let version = parts.next()?;
    let revision = parts.next()?.to_owned();
    let (count, hash) = revision.split_once('.')?;
    if count.is_empty()
        || !count.chars().all(|c| c.is_ascii_digit())
        || hash.is_empty()
        || !hash
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
    {
        return None;
    }
    let offsets: Option<Vec<usize>> = parts.map(|part| part.parse::<usize>().ok()).collect();
    match (version, offsets?.as_slice()) {
        (CURSOR_VERSION, [offset]) => Some(Cursor::Forward {
            revision,
            offset: *offset,
        }),
        (OLDER_CURSOR_VERSION, [before, floor]) if floor <= before => Some(Cursor::Older {
            revision,
            before: *before,
            floor: *floor,
        }),
        _ => None,
    }
}

fn fail(code: &str, message: impl Into<String>, next_action: &str) -> Box<ThreadToolFailure> {
    Box::new(thread_tool_failure(code, message, next_action, None))
}

/// The JSONL file's object lines; a line that is not a JSON object is skipped, never fatal.
fn load_jsonl(path: &Path) -> Option<Vec<Value>> {
    let text = std::fs::read(path).ok()?;
    Some(
        String::from_utf8_lossy(&text)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(Value::is_object)
            .collect(),
    )
}

/// Reads one bounded slice. Without a cursor it is a tail read: the newest of the last 60 entries that fit the budget.
/// With a cursor it pages: forward after the last returned entry, or, from a tail read's cursor, the older entries of
/// its window. Truncation is a budget outcome, not a failure: `truncated` with a `next_cursor`.
pub fn read_transcript(
    source: ReadSource<'_>,
    max_bytes: Option<u64>,
    cursor: Option<&str>,
) -> Result<TranscriptSlice, Box<ThreadToolFailure>> {
    let max_bytes = match max_bytes {
        None => THREAD_READ_DEFAULT_BYTES,
        Some(0) => {
            return Err(fail(
                "invalid_arguments",
                "max_bytes must be a positive whole number; received 0.",
                "Retry with max_bytes between 1 and 1048576, or omit it to take the default 131072.",
            ));
        }
        Some(bytes) if bytes > THREAD_READ_MAX_BYTES as u64 => {
            return Err(fail(
                "message_too_large",
                format!("max_bytes {bytes} exceeds the {THREAD_READ_MAX_BYTES}-byte hard cap."),
                "Re-read with max_bytes at most 1048576 and follow next_cursor for the rest of the transcript.",
            ));
        }
        Some(bytes) => usize::try_from(bytes).unwrap_or(THREAD_READ_MAX_BYTES),
    };
    let cursor = match cursor {
        None => None,
        Some(raw) => Some(decode_cursor(raw).ok_or_else(|| {
            fail(
                "cursor_invalid",
                "The cursor is not a cursor this reader issued.",
                "Re-read the thread without a cursor to get a fresh next_cursor, or call thread_list (tool.thread_list() in an eval cell) to confirm the thread address.",
            )
        })?),
    };
    let (entries, source_kind, source_incomplete) = match source {
        ReadSource::Live(entries) => (entries.to_vec(), ThreadReadSource::LiveHost, false),
        ReadSource::Jsonl {
            path,
            live_host_present,
        } => {
            let Some(entries) = load_jsonl(path) else {
                return Err(fail(
                    "not_found",
                    format!("No session transcript exists at {}.", path.display()),
                    "Call thread_list (tool.thread_list() in an eval cell) to get a current thread value, then read that thread instead.",
                ));
            };
            (entries, ThreadReadSource::SessionJsonl, !live_host_present)
        }
    };
    let revision = revision_of(&entries);
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.revision() != revision)
    {
        return Err(fail(
            "cursor_stale",
            "The transcript changed after this cursor was issued, so its position no longer names the same entries.",
            "Re-read the thread without a cursor to restart from a current revision.",
        ));
    }
    let slice = |items: Vec<Value>, truncated: bool, next_cursor: Option<String>| TranscriptSlice {
        source: source_kind,
        source_incomplete,
        items,
        truncated,
        next_cursor,
    };
    if let Some(Cursor::Forward { offset, .. }) = cursor {
        let start = offset.min(entries.len());
        let selected = take_within_budget(&entries[start..], max_bytes, BudgetFrom::Start);
        let next = start + selected.len();
        let truncated = next < entries.len();
        let next_cursor = truncated.then(|| {
            encode_cursor(&Cursor::Forward {
                revision: revision.clone(),
                offset: next,
            })
        });
        return Ok(slice(selected, truncated, next_cursor));
    }
    let (floor, end) = match cursor {
        Some(Cursor::Older { before, floor, .. }) => (floor, before.min(entries.len())),
        _ => (
            entries.len().saturating_sub(THREAD_TAIL_DEFAULT_ITEMS),
            entries.len(),
        ),
    };
    let floor = floor.min(end);
    let selected = take_within_budget(&entries[floor..end], max_bytes, BudgetFrom::End);
    let start = end - selected.len();
    let truncated = start > floor;
    let next_cursor = truncated.then(|| {
        encode_cursor(&Cursor::Older {
            revision: revision.clone(),
            before: start,
            floor,
        })
    });
    Ok(slice(selected, truncated, next_cursor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entries(count: usize) -> Vec<Value> {
        (0..count)
            .map(|index| json!({ "id": format!("e{index}"), "role": "user", "content": "x".repeat(10) }))
            .collect()
    }

    #[test]
    fn a_first_read_ends_at_the_newest_entry_and_pages_older_ones() {
        let transcript = entries(5);
        let one = serde_json::to_string(&transcript[0]).map_or(0, |text| text.len()) as u64;
        let first = read_transcript(ReadSource::Live(&transcript), Some(one * 2), None)
            .expect("first read");
        assert_eq!(first.items, transcript[3..5].to_vec());
        assert!(first.truncated);
        let cursor = first.next_cursor.expect("cursor");
        let older = read_transcript(ReadSource::Live(&transcript), Some(one * 2), Some(&cursor))
            .expect("older page");
        assert_eq!(older.items, transcript[1..3].to_vec());
        let last = read_transcript(
            ReadSource::Live(&transcript),
            Some(one * 2),
            older.next_cursor.as_deref(),
        )
        .expect("last page");
        assert_eq!(last.items, transcript[0..1].to_vec());
        assert!(!last.truncated);
        assert_eq!(last.next_cursor, None);
    }

    #[test]
    fn a_cursor_over_a_changed_transcript_is_stale_and_a_foreign_one_invalid() {
        let transcript = entries(3);
        let first = read_transcript(ReadSource::Live(&transcript), Some(1), None).expect("read");
        let cursor = first.next_cursor.expect("cursor");
        let grown = entries(4);
        let stale =
            read_transcript(ReadSource::Live(&grown), Some(1), Some(&cursor)).expect_err("stale");
        assert_eq!(stale.code.as_str(), "cursor_stale");
        let invalid =
            read_transcript(ReadSource::Live(&grown), None, Some("zz")).expect_err("invalid");
        assert_eq!(invalid.code.as_str(), "cursor_invalid");
    }

    #[test]
    fn the_budget_is_bounded_and_the_jsonl_source_says_it_may_lag() {
        let too_large = read_transcript(ReadSource::Live(&[]), Some(2_000_000), None)
            .expect_err("over the cap");
        assert_eq!(too_large.code.as_str(), "message_too_large");
        let missing = read_transcript(
            ReadSource::Jsonl {
                path: Path::new("/nonexistent/maho-thread-reader.jsonl"),
                live_host_present: false,
            },
            None,
            None,
        )
        .expect_err("missing file");
        assert_eq!(missing.code.as_str(), "not_found");
    }
}
