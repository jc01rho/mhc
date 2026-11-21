//! Port of `reflection/run-id-high-water.ts` (omo #12517): the highest reflection run number any
//! trace on disk still carries, and the durable high-water mark that survives their removal.

use std::path::{Path, PathBuf};

use crate::fs::{read_dir_names, read_to_string, remove_file, rename, retry_on_eintr, write};
use crate::identity::layout::MemoryIdentityPaths;

pub const REFLECTION_RUN_ID_PREFIX: &str = "reflection-run-";
const RESERVATION_STATE_FILES: [&str; 2] = ["active.lock", "pending.json"];
const RUN_ID_MARK_FILE: &str = "run-id.json";
/// `Number.MAX_SAFE_INTEGER`: a larger run number is not one the allocator could have minted.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy)]
pub struct RunIdHighWaterPaths<'a> {
    pub reflection: &'a Path,
    pub reflection_sessions: &'a Path,
    pub worktrees: &'a Path,
}

impl<'a> From<&'a MemoryIdentityPaths> for RunIdHighWaterPaths<'a> {
    fn from(paths: &'a MemoryIdentityPaths) -> Self {
        Self {
            reflection: &paths.reflection,
            reflection_sessions: &paths.reflection_sessions,
            worktrees: &paths.worktrees,
        }
    }
}

/// The run number of `reflection-run-7`, `reflection-run-7.json`, or the epoch-prefixed
/// `1755000000000-reflection-run-7`; 0 for any other name.
pub fn reflection_run_number(name: &str) -> u64 {
    let Some(rest) = strip_epoch_prefix(name).strip_prefix(REFLECTION_RUN_ID_PREFIX) else {
        return 0;
    };
    let digits = rest.strip_suffix(".json").unwrap_or(rest);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return 0;
    }
    match digits.parse::<u64>() {
        Ok(number) if number <= MAX_SAFE_INTEGER => number,
        _ => 0,
    }
}

/// `(?:\d+-)?`: an all-digit segment followed by `-` is an epoch prefix.
fn strip_epoch_prefix(name: &str) -> &str {
    let digits = name.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && name.as_bytes().get(digits) == Some(&b'-') {
        &name[digits + 1..]
    } else {
        name
    }
}

/// The highest run number any trace on disk still carries (H1): completion records, run dirs,
/// legacy reflection-session dirs, worktrees, and the live reservation files. The allocator mints
/// one past it, so every removal path exempts the trace that carries it.
pub fn scan_reflection_run_high_water(paths: RunIdHighWaterPaths<'_>) -> u64 {
    let directories = [
        paths.reflection.join("completions"),
        paths.reflection.join("runs"),
        paths.reflection_sessions.to_path_buf(),
        paths.worktrees.to_path_buf(),
    ];
    let named = directories
        .iter()
        .map(|directory| highest_named_run(directory.as_path()))
        .max()
        .unwrap_or(0);
    let reserved = RESERVATION_STATE_FILES
        .iter()
        .map(|name| reserved_run(&paths.reflection.join(name)))
        .max()
        .unwrap_or(0);
    named.max(reserved)
}

fn highest_named_run(directory: &Path) -> u64 {
    read_dir_names(directory)
        .unwrap_or_default()
        .iter()
        .map(|name| reflection_run_number(name.as_str()))
        .max()
        .unwrap_or(0)
}

/// Unreadable reservation state contributes nothing: the reservation store reports it itself.
fn reserved_run(path: &Path) -> u64 {
    let Ok(raw) = read_to_string(path) else {
        return 0;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return 0;
    };
    parsed
        .get("runId")
        .and_then(serde_json::Value::as_str)
        .map_or(0, reflection_run_number)
}

fn reflection_run_id_mark_path(reflection_dir: &Path) -> PathBuf {
    reflection_dir.join(RUN_ID_MARK_FILE)
}

/// The durable high-water mark (H1 Guard 2), or `None` when absent or unreadable (scan only).
pub fn read_reflection_run_id_mark(reflection_dir: &Path) -> Option<u64> {
    let raw = read_to_string(&reflection_run_id_mark_path(reflection_dir)).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    safe_non_negative_integer(parsed.get("highWater")?)
}

fn safe_non_negative_integer(value: &serde_json::Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return (number <= MAX_SAFE_INTEGER).then_some(number);
    }
    let number = value.as_f64()?;
    (number >= 0.0 && number.fract() == 0.0 && number <= MAX_SAFE_INTEGER as f64)
        .then_some(number as u64)
}

/// Raises the durable mark to `high_water` and never lowers it. The caller holds the
/// `reflection-scheduler` lock, the lock the allocator mints under, so read-max-write cannot race.
pub fn record_reflection_run_id_mark(
    reflection_dir: &Path,
    high_water: u64,
) -> std::io::Result<u64> {
    let current = read_reflection_run_id_mark(reflection_dir);
    let next = current.unwrap_or(0).max(high_water);
    if current == Some(next) {
        return Ok(next);
    }
    create_private_dir_all(reflection_dir)?;
    let target = reflection_run_id_mark_path(reflection_dir);
    let body = format!(
        "{}\n",
        serde_json::json!({ "version": 1, "highWater": next })
    );
    write_private_atomically(&target, body.as_bytes())?;
    Ok(next)
}

fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    retry_on_eintr(|| builder.create(path))
}

fn write_private_atomically(target: &Path, data: &[u8]) -> std::io::Result<()> {
    let temporary = PathBuf::from(format!(
        "{}.tmp-{}",
        target.display(),
        crate::support::random::random_uuid()
    ));
    let written = write(&temporary, data)
        .and_then(|()| restrict_to_owner(&temporary))
        .and_then(|()| rename(&temporary, target));
    if written.is_err() {
        let _ = remove_file(&temporary);
    }
    written
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "run_id_high_water_tests.rs"]
mod tests;
