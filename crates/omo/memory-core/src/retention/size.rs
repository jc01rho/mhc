//! Port of `retention/size.ts`: how many bytes an identity's `runtime/` holds, counted the way `du`
//! counts them (allocated blocks when the platform reports them, else the size), so the doctor
//! number is the number the user sees on disk and the number the size cap acts on. Symlinks count
//! as themselves and are never followed.

use std::fs::Metadata;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use super::policy::{RetentionError, check_abort, is_gone, list_names};
use crate::fs::resilient::symlink_metadata;
use crate::support::time::format_rfc3339_millis;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStoreSize {
    pub store: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSize {
    pub bytes: u64,
    /// Per store, largest first: `reflection/<x>` and `recall/<x>` are split, every other top
    /// entry is one store.
    pub stores: Vec<RuntimeStoreSize>,
}

/// `memory.runtime.max_bytes` default per identity; `0` means unbounded.
pub const DEFAULT_RUNTIME_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Both doctor surfaces warn from this share of the cap on.
const RUNTIME_SIZE_WARN_RATIO: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSizeLevel {
    Ok,
    Warn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeSizeStanding {
    pub level: RuntimeSizeLevel,
    /// Whole percent of the cap, absent when the cap is unbounded.
    pub percent: Option<u64>,
}

pub fn runtime_size_standing(bytes: u64, max_bytes: u64) -> RuntimeSizeStanding {
    if max_bytes == 0 {
        return RuntimeSizeStanding {
            level: RuntimeSizeLevel::Ok,
            percent: None,
        };
    }
    let percent = (bytes as f64 / max_bytes as f64 * 100.0).floor() as u64;
    let level = if bytes as f64 >= max_bytes as f64 * RUNTIME_SIZE_WARN_RATIO {
        RuntimeSizeLevel::Warn
    } else {
        RuntimeSizeLevel::Ok
    };
    RuntimeSizeStanding {
        level,
        percent: Some(percent),
    }
}

pub fn format_runtime_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let text = if value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    };
    format!("{text} {}", UNITS[unit])
}

fn is_split_store(name: &str) -> bool {
    name == "reflection" || name == "recall"
}

#[cfg(unix)]
fn charged_bytes(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let blocks = metadata.blocks();
    if blocks > 0 || metadata.len() == 0 {
        blocks * 512
    } else {
        metadata.len()
    }
}

#[cfg(not(unix))]
fn charged_bytes(metadata: &Metadata) -> u64 {
    metadata.len()
}

fn lstat_or_none(path: &Path) -> io::Result<Option<Metadata>> {
    match symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if is_gone(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Bytes under `path`, the entry itself included; entries removed while walking count as zero. A
/// fired `cancel` (the pass deadline) aborts the walk between entries.
pub fn measure_tree_bytes(
    path: &Path,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<u64, RetentionError> {
    check_abort(cancel)?;
    let Some(metadata) = lstat_or_none(path)? else {
        return Ok(0);
    };
    let mut bytes = charged_bytes(&metadata);
    if !metadata.is_dir() {
        return Ok(bytes);
    }
    for name in list_names(path)? {
        bytes += measure_tree_bytes(&path.join(name), cancel)?;
    }
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSizeUnknown {
    pub budget_ms: u64,
    /// ISO time the measurement gave up.
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasuredRuntimeSize {
    Size(RuntimeSize),
    Unknown(RuntimeSizeUnknown),
}

/// The doctors' measurement (`/doctor` and `omo doctor`): [`measure_runtime_size`] under a deadline
/// of `budget_ms` (or the caller's `cancel`), so a huge tree reports "size unknown" instead of
/// stalling.
pub fn measure_runtime_size_within_budget(
    runtime_dir: &Path,
    budget_ms: Option<u64>,
    cancel: Option<&dyn Fn() -> bool>,
    now: i64,
) -> Result<MeasuredRuntimeSize, RetentionError> {
    let deadline = budget_ms.map(|budget| Instant::now() + Duration::from_millis(budget));
    let timed_out = move || deadline.is_some_and(|deadline| Instant::now() >= deadline);
    let signal: &dyn Fn() -> bool = match cancel {
        Some(cancel) => cancel,
        None => &timed_out,
    };
    match measure_runtime_size(runtime_dir, Some(signal)) {
        Ok(size) => Ok(MeasuredRuntimeSize::Size(size)),
        Err(RetentionError::Aborted) if signal() => {
            Ok(MeasuredRuntimeSize::Unknown(RuntimeSizeUnknown {
                budget_ms: budget_ms.unwrap_or(0),
                at: format_rfc3339_millis(now),
            }))
        }
        Err(error) => Err(error),
    }
}

pub fn measure_runtime_size(
    runtime_dir: &Path,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<RuntimeSize, RetentionError> {
    check_abort(cancel)?;
    let Some(root) = lstat_or_none(runtime_dir)? else {
        return Ok(RuntimeSize::default());
    };
    let mut bytes = charged_bytes(&root);
    let mut stores = Vec::new();
    if root.is_dir() {
        for name in list_names(runtime_dir)? {
            let path = runtime_dir.join(&name);
            let Some(stats) = lstat_or_none(&path)? else {
                continue;
            };
            if stats.is_dir() && is_split_store(&name) {
                bytes += charged_bytes(&stats);
                for child in list_names(&path)? {
                    let child_bytes = measure_tree_bytes(&path.join(&child), cancel)?;
                    bytes += child_bytes;
                    stores.push(RuntimeStoreSize {
                        store: format!("{name}/{child}"),
                        bytes: child_bytes,
                    });
                }
                continue;
            }
            let store_bytes = measure_tree_bytes(&path, cancel)?;
            bytes += store_bytes;
            stores.push(RuntimeStoreSize {
                store: name,
                bytes: store_bytes,
            });
        }
    }
    stores.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.store.cmp(&right.store))
    });
    Ok(RuntimeSize { bytes, stores })
}
