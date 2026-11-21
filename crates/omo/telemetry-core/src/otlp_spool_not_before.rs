//! Rust port of `packages/telemetry-core/src/otlp-spool-not-before.ts`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::otlp_http::NotBefore;

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn read_not_before(path: &Path) -> i64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map_or(0, |value| value as i64)
}

fn defer_not_before(path: &Path, until_ms: i64) {
    if until_ms <= read_not_before(path) {
        return;
    }
    let temp = with_suffix(path, &format!(".{}.tmp", std::process::id()));
    let published = std::fs::write(&temp, until_ms.to_string())
        .and_then(|()| std::fs::rename(&temp, path))
        .is_ok();
    if !published {
        let _removed = std::fs::remove_file(&temp);
    }
}

/// The install's shared "do not send before" time, one epoch-ms number in `<spool dir>.not-before`
/// beside the spool (outside it, so the spool holds only batches). The collector rate-limits per
/// install, so a 429 seen by one process must pause every process of that install, not only the one
/// that received it. The time only moves forward; an unreadable or missing file means "send now".
pub fn create_spool_not_before(dir: &Path) -> NotBefore {
    let path = with_suffix(dir, ".not-before");
    let read_path = path.clone();
    NotBefore {
        read: Arc::new(move || read_not_before(&read_path)),
        // Best effort: without the shared file this process still backs off on its own.
        defer: Arc::new(move |until_ms: i64| defer_not_before(&path, until_ms)),
    }
}
