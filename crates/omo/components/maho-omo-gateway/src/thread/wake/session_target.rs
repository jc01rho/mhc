use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::thread::address_book::find_disk_sessions;
use crate::thread::graceful_close_returns::SESSION_CLOSED_ENTRY;

pub const SESSION_FACTS_WINDOW_BYTES: u64 = 64 * 1024;
const RPC_EFFECT_ENTRY: &str = "rpc_effect";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeSessionKind {
    Interactive,
    Child,
    Worker,
    Unknown,
}

impl WakeSessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Child => "child",
            Self::Worker => "worker",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeTarget {
    Gone,
    Ambiguous {
        session_paths: Vec<String>,
    },
    Found {
        session_path: String,
        cwd: String,
        session_kind: WakeSessionKind,
        closed_at: Option<i64>,
    },
}

fn parse_session_lines(text: &str, drop_first: bool, drop_last: bool) -> Vec<Value> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if drop_first && !lines.is_empty() {
        lines.remove(0);
    }
    if drop_last && !lines.is_empty() {
        lines.pop();
    }
    lines
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}

fn header_of(path: &Path) -> Option<Value> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0_u8; SESSION_FACTS_WINDOW_BYTES as usize];
    let read = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..read]);
    parse_session_lines(&text, false, true)
        .into_iter()
        .find(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("session")
                && entry
                    .get("id")
                    .or_else(|| entry.get("session_id"))
                    .or_else(|| entry.get("sessionId"))
                    .and_then(Value::as_str)
                    .is_some()
        })
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn parse_iso_timestamp_ms(stamp: &str) -> Option<i64> {
    let trimmed = stamp.trim();
    let (date_part, time_part) = trimmed.split_once('T')?;
    let mut date_iter = date_part.splitn(3, '-');
    let year: i64 = date_iter.next()?.parse().ok()?;
    let month: i64 = date_iter.next()?.parse().ok()?;
    let day: i64 = date_iter.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let time_no_z = time_part.strip_suffix('Z').unwrap_or(time_part);
    let mut time_iter = time_no_z.splitn(3, ':');
    let hour: i64 = time_iter.next()?.parse().ok()?;
    let minute: i64 = time_iter.next()?.parse().ok()?;
    let sec_rest = time_iter.next()?;
    let (sec_str, millis) = match sec_rest.split_once('.') {
        Some((s, frac)) => {
            let digits: String = frac
                .chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect();
            let padded = format!("{digits:0<3}");
            (s, padded.parse::<i64>().ok()?)
        }
        None => (sec_rest, 0),
    };
    let second: i64 = sec_str.parse().ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1_000 + millis)
}

pub fn close_instant_of(custom_type: Option<&str>, data: &Value) -> Option<i64> {
    let stamp = match custom_type {
        Some(RPC_EFFECT_ENTRY)
            if data.get("kind").and_then(Value::as_str) == Some("unload")
                && data.get("state").and_then(Value::as_str) == Some("done") =>
        {
            data.get("updatedAt").and_then(Value::as_str)
        }
        Some(SESSION_CLOSED_ENTRY) => data.get("at").and_then(Value::as_str),
        _ => None,
    }?;
    parse_iso_timestamp_ms(stamp)
}

pub fn closed_at_of(path: &Path) -> Option<i64> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(SESSION_FACTS_WINDOW_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0_u8; (size - start) as usize];
    let read = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..read]);
    let mut closed_at: Option<i64> = None;
    for entry in parse_session_lines(&text, start > 0, false) {
        if entry.get("type").and_then(Value::as_str) != Some("custom") {
            continue;
        }
        let Some(data) = entry.get("data").filter(|d| d.is_object()) else {
            continue;
        };
        let custom_type = entry.get("customType").and_then(Value::as_str);
        if let Some(at) = close_instant_of(custom_type, data)
            && closed_at.is_none_or(|prev| at > prev)
        {
            closed_at = Some(at);
        }
    }
    closed_at
}

pub fn session_kind_of(header: Option<&Value>) -> WakeSessionKind {
    let Some(header) = header.filter(|h| h.is_object()) else {
        return WakeSessionKind::Unknown;
    };
    if header.get("kind").and_then(Value::as_str) == Some("worker") {
        return WakeSessionKind::Worker;
    }
    if header.get("spawnedBy").is_some_and(Value::is_object) {
        return WakeSessionKind::Child;
    }
    if header.get("kind").is_none() {
        return WakeSessionKind::Interactive;
    }
    WakeSessionKind::Unknown
}

pub fn resolve_wake_target(sessions_directory: &Path, durable_id: &str) -> WakeTarget {
    let found: Vec<_> = find_disk_sessions(sessions_directory, durable_id, true, &[])
        .into_iter()
        .filter(|session| session.durable_id == durable_id)
        .collect();
    let Some(only) = found.first() else {
        return WakeTarget::Gone;
    };
    if found.len() > 1 {
        return WakeTarget::Ambiguous {
            session_paths: found.into_iter().map(|s| s.session_path).collect(),
        };
    }
    let path = Path::new(&only.session_path);
    let closed_at = closed_at_of(path);
    WakeTarget::Found {
        session_path: only.session_path.clone(),
        cwd: only.cwd.clone(),
        session_kind: session_kind_of(header_of(path).as_ref()),
        closed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_session_file(dir: &Path, folder: &str, durable_id: &str, lines: &[Value]) -> String {
        let project_dir = dir.join(folder);
        std::fs::create_dir_all(&project_dir).expect("mkdir");
        let file_path = project_dir.join(format!("2026-11-19_{durable_id}.jsonl"));
        let mut content = String::new();
        for line in lines {
            content.push_str(&line.to_string());
            content.push('\n');
        }
        std::fs::write(&file_path, content).expect("write");
        file_path.to_string_lossy().into_owned()
    }

    #[test]
    fn given_session_transcripts_with_unload_effect_or_session_closed_when_resolved_then_newest_close_instant_and_kind_are_read()
     {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_session_file(
            dir.path(),
            "--work-a--",
            "dur-1",
            &[
                json!({
                    "type": "session",
                    "id": "dur-1",
                    "session_id": "dur-1",
                    "cwd": "/work/a",
                    "created_at": "2026-11-19T00:00:00.000Z"
                }),
                json!({
                    "type": "custom",
                    "customType": "rpc_effect",
                    "data": {
                        "kind": "unload",
                        "state": "done",
                        "updatedAt": "2026-11-19T05:00:00.000Z"
                    }
                }),
                json!({
                    "type": "custom",
                    "customType": "session-closed",
                    "data": {
                        "at": "2026-11-19T06:00:00.000Z",
                        "returned": { "deliveries": [] }
                    }
                }),
            ],
        );

        let expected_at = parse_iso_timestamp_ms("2026-11-19T06:00:00.000Z");
        assert_eq!(
            resolve_wake_target(dir.path(), "dur-1"),
            WakeTarget::Found {
                session_path: path,
                cwd: "/work/a".into(),
                session_kind: WakeSessionKind::Interactive,
                closed_at: expected_at,
            }
        );
    }

    #[test]
    fn given_worker_child_and_ambiguous_sessions_when_resolved_then_kinds_and_ambiguity_are_reported()
     {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(resolve_wake_target(dir.path(), "missing"), WakeTarget::Gone);

        write_session_file(
            dir.path(),
            "--work-w--",
            "dur-worker",
            &[json!({
                "type": "session",
                "id": "dur-worker",
                "session_id": "dur-worker",
                "cwd": "/work/w",
                "kind": "worker"
            })],
        );
        assert!(matches!(
            resolve_wake_target(dir.path(), "dur-worker"),
            WakeTarget::Found {
                session_kind: WakeSessionKind::Worker,
                closed_at: None,
                ..
            }
        ));

        write_session_file(
            dir.path(),
            "--work-c--",
            "dur-child",
            &[json!({
                "type": "session",
                "id": "dur-child",
                "session_id": "dur-child",
                "cwd": "/work/c",
                "spawnedBy": { "sessionId": "parent" }
            })],
        );
        assert!(matches!(
            resolve_wake_target(dir.path(), "dur-child"),
            WakeTarget::Found {
                session_kind: WakeSessionKind::Child,
                ..
            }
        ));

        write_session_file(
            dir.path(),
            "--work-1--",
            "dur-dup",
            &[json!({ "type": "session", "id": "dur-dup", "session_id": "dur-dup", "cwd": "/w1" })],
        );
        write_session_file(
            dir.path(),
            "--work-2--",
            "dur-dup",
            &[json!({ "type": "session", "id": "dur-dup", "session_id": "dur-dup", "cwd": "/w2" })],
        );
        assert!(matches!(
            resolve_wake_target(dir.path(), "dur-dup"),
            WakeTarget::Ambiguous { session_paths } if session_paths.len() == 2
        ));
    }
}
