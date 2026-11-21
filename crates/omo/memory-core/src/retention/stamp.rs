//! Port of `retention/stamp.ts`: the resume points of an interrupted pass. `runtime/retention.json`
//! holds only the store walk's cursor and, in its own `cap` field, the size cap's resume point; it
//! is written only when one of them stopped early and deleted when neither did. The completion
//! stamp lives in the memory repo's git config (`omo.retentionAt`), written by the caller.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::budget::RetentionCursor;
use super::cap::RuntimeCapCursor;
use super::policy::write_private_file;
use crate::fs::resilient::{read_to_string, remove_file, rename};
use crate::support::random::random_uuid;

pub const RETENTION_STAMP_CONFIG_KEY: &str = "omo.retentionAt";
const CURSOR_FILE: &str = "retention.json";

pub fn retention_cursor_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(CURSOR_FILE)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionResumePoints {
    /// Where the store walk resumes.
    pub cursor: Option<RetentionCursor>,
    /// Where the size cap resumes its tier walk.
    pub cap: Option<RuntimeCapCursor>,
    /// Set with `cap` when the cap's last pass could not measure `runtime/` inside the budget.
    pub cap_stalled: bool,
}

pub fn read_retention_resume_points(runtime_dir: &Path) -> RetentionResumePoints {
    let Ok(raw) = read_to_string(&retention_cursor_path(runtime_dir)) else {
        return RetentionResumePoints::default();
    };
    let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(&raw) else {
        return RetentionResumePoints::default();
    };
    let stored_cursor = parsed.get("cursor").and_then(Value::as_object);
    if stored_cursor
        .is_some_and(|cursor| cursor.get("store").and_then(Value::as_str) == Some("cap"))
    {
        return RetentionResumePoints {
            cap: cap_resume_point(stored_cursor),
            ..RetentionResumePoints::default()
        };
    }
    let cap = cap_resume_point(parsed.get("cap").and_then(Value::as_object));
    let cap_stalled = cap.is_some() && parsed.get("capStalled") == Some(&Value::Bool(true));
    let cursor = stored_cursor.and_then(|cursor| {
        let store = cursor.get("store")?.as_str()?;
        Some(RetentionCursor {
            store: store.to_string(),
            last_name: cursor
                .get("lastName")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    });
    RetentionResumePoints {
        cursor,
        cap,
        cap_stalled,
    }
}

fn cap_resume_point(cursor: Option<&Map<String, Value>>) -> Option<RuntimeCapCursor> {
    let cursor = cursor?;
    if cursor.get("store").and_then(Value::as_str) != Some("cap") {
        return None;
    }
    let tier = cursor.get("tier")?.as_str()?;
    let last_name = cursor.get("lastName")?.as_str()?;
    let clock = cursor.get("clock")?;
    let clock = clock.as_i64().or_else(|| {
        clock
            .as_f64()
            .filter(|value| value.is_finite())
            .map(|value| value as i64)
    })?;
    Some(RuntimeCapCursor {
        tier: tier.to_string(),
        clock,
        last_name: last_name.to_string(),
    })
}

/// Whether an interrupted pass left work the next bind must resume whatever the completion stamp
/// says: a store-walk cursor, or a cap resume point whose pass was not stalled at its measurement.
pub fn retention_resume_pending(runtime_dir: &Path) -> bool {
    let points = read_retention_resume_points(runtime_dir);
    points.cursor.is_some() || (points.cap.is_some() && !points.cap_stalled)
}

pub fn write_retention_resume_points(
    runtime_dir: &Path,
    points: &RetentionResumePoints,
) -> io::Result<()> {
    let target = retention_cursor_path(runtime_dir);
    let temporary = target.with_file_name(format!("{CURSOR_FILE}.tmp-{}", random_uuid()));
    let mut body = Map::new();
    body.insert("version".to_string(), json!(1));
    if let Some(cursor) = &points.cursor {
        let mut stored = Map::new();
        stored.insert("store".to_string(), json!(cursor.store));
        if let Some(last_name) = &cursor.last_name {
            stored.insert("lastName".to_string(), json!(last_name));
        }
        body.insert("cursor".to_string(), Value::Object(stored));
    }
    if let Some(cap) = &points.cap {
        body.insert(
            "cap".to_string(),
            json!({
                "store": "cap",
                "tier": cap.tier,
                "clock": cap.clock,
                "lastName": cap.last_name,
            }),
        );
        if points.cap_stalled {
            body.insert("capStalled".to_string(), json!(true));
        }
    }
    let line = format!("{}\n", Value::Object(body));
    let written =
        write_private_file(&temporary, line.as_bytes()).and_then(|()| rename(&temporary, &target));
    if written.is_err() {
        let _ = remove_file(&temporary);
    }
    written
}

pub fn clear_retention_cursor(runtime_dir: &Path) -> io::Result<()> {
    match remove_file(&retention_cursor_path(runtime_dir)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}
