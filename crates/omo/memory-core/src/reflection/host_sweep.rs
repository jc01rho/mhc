//! Port of `reflection/host-sweep.ts` (omo #12517, DESIGN.r2 6b, H4 of the #10144/#11976 review):
//! one host-wide pass over every memory identity. The backlog sits mostly on identities no session
//! will bind again, so `doctor --fix` (unbounded) and a bound session's maintenance pass (bounded)
//! walk the same sorted identity list from one persisted cursor, under one lock, and each identity
//! is the unit of work. Steps are supplied by the caller so retention and the size cap join this
//! pass without a second cursor.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::fs::{
    metadata, read_dir_names, read_to_string, remove_file, rename, retry_on_eintr, write,
};
use crate::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, WithLockError,
    create_lock_record, with_lock,
};
use crate::support::time::{format_rfc3339_millis, now_millis};

pub const HOST_SWEEP_STATE_FILE: &str = "runtime-sweep.json";
pub const HOST_SWEEP_LOCK_FILE: &str = "runtime-sweep.lock";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostSweepState {
    pub cursor: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSweepIdentity {
    pub name: String,
    pub root: PathBuf,
    pub runtime_dir: PathBuf,
    pub has_repo: bool,
}

pub type HostSweepStep<'a> = Box<dyn FnMut(&HostSweepIdentity) -> Result<Vec<String>, String> + 'a>;

pub struct HostSweepOptions<'a> {
    pub memory_root: &'a Path,
    pub steps: Vec<HostSweepStep<'a>>,
    /// Halt after this many identities (the bounded driver); `None` means every identity once.
    pub max_identities: Option<usize>,
    /// Halt once this many milliseconds have passed (checked between identities).
    pub budget_ms: Option<u64>,
    pub now: Option<&'a dyn Fn() -> u64>,
    pub aborted: Option<&'a dyn Fn() -> bool>,
    /// Walk and run the steps, but never write the cursor.
    pub dry_run: bool,
}

impl<'a> HostSweepOptions<'a> {
    pub fn new(memory_root: &'a Path) -> Self {
        Self {
            memory_root,
            steps: Vec::new(),
            max_identities: None,
            budget_ms: None,
            now: None,
            aborted: None,
            dry_run: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSweepResult {
    Busy,
    Swept {
        visited: Vec<String>,
        completed: bool,
        lines: Vec<String>,
    },
}

fn is_directory(path: &Path) -> bool {
    metadata(path).is_ok_and(|status| status.is_dir())
}

/// Every `agents/<id>` that has a `runtime/`, repo or not, in sorted order.
fn list_runtime_identities(memory_root: &Path) -> Result<Vec<HostSweepIdentity>, String> {
    let agents_root = memory_root.join("agents");
    let names = match read_dir_names(&agents_root) {
        Ok(names) => names,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot list {}: {error}", agents_root.display())),
    };
    let mut identities = Vec::new();
    for name in names.into_iter().filter(|name| !name.starts_with('.')) {
        let root = agents_root.join(&name);
        let runtime_dir = root.join("runtime");
        if !is_directory(&runtime_dir) {
            continue;
        }
        let has_repo = is_directory(&root.join("repo"));
        identities.push(HostSweepIdentity {
            name,
            root,
            runtime_dir,
            has_repo,
        });
    }
    Ok(identities)
}

pub fn read_host_sweep_state(memory_root: &Path) -> HostSweepState {
    let Ok(raw) = read_to_string(&memory_root.join(HOST_SWEEP_STATE_FILE)) else {
        return HostSweepState::default();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return HostSweepState::default();
    };
    let text = |key: &str| {
        parsed
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    HostSweepState {
        cursor: text("cursor"),
        completed_at: text("completedAt"),
    }
}

fn write_host_sweep_state(memory_root: &Path, state: &HostSweepState) -> std::io::Result<()> {
    create_private_dir_all(memory_root)?;
    let mut fields = serde_json::Map::new();
    fields.insert("version".to_string(), serde_json::Value::from(1));
    if let Some(cursor) = &state.cursor {
        fields.insert(
            "cursor".to_string(),
            serde_json::Value::from(cursor.as_str()),
        );
    }
    if let Some(completed_at) = &state.completed_at {
        fields.insert(
            "completedAt".to_string(),
            serde_json::Value::from(completed_at.as_str()),
        );
    }
    let body = format!("{}\n", serde_json::Value::Object(fields));
    let target = memory_root.join(HOST_SWEEP_STATE_FILE);
    let temporary = PathBuf::from(format!(
        "{}.tmp-{}",
        target.display(),
        crate::support::random::random_uuid()
    ));
    let written = write(&temporary, body.as_bytes())
        .and_then(|()| restrict_to_owner(&temporary))
        .and_then(|()| rename(&temporary, &target));
    if written.is_err() {
        let _ = remove_file(&temporary);
    }
    written
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

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// The identities still to visit in this round (after the cursor), and, for an unbounded driver,
/// the ones before it too, so `doctor --fix` covers every identity in one run.
fn resume_order<'i>(
    identities: &'i [HostSweepIdentity],
    cursor: Option<&str>,
    unbounded: bool,
) -> (Vec<&'i HostSweepIdentity>, Vec<&'i HostSweepIdentity>) {
    let Some(cursor) = cursor else {
        return (identities.iter().collect(), Vec::new());
    };
    let rest = identities
        .iter()
        .filter(|identity| identity.name.as_str() > cursor)
        .collect();
    let wrap = if unbounded {
        identities
            .iter()
            .filter(|identity| identity.name.as_str() <= cursor)
            .collect()
    } else {
        Vec::new()
    };
    (rest, wrap)
}

struct SweepLimits<'c> {
    max_identities: Option<usize>,
    budget_ms: Option<u64>,
    started_at: u64,
    now: &'c dyn Fn() -> u64,
    aborted: Option<&'c dyn Fn() -> bool>,
}

fn visit(
    identity: &HostSweepIdentity,
    steps: &mut [HostSweepStep<'_>],
    limits: &SweepLimits<'_>,
    visited: &mut Vec<String>,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    if limits.aborted.is_some_and(|aborted| aborted()) {
        return Ok(false);
    }
    if limits
        .max_identities
        .is_some_and(|max| visited.len() >= max)
    {
        return Ok(false);
    }
    if let Some(budget_ms) = limits.budget_ms
        && !visited.is_empty()
        && (limits.now)().saturating_sub(limits.started_at) >= budget_ms
    {
        return Ok(false);
    }
    for step in steps.iter_mut() {
        lines.extend(step(identity)?);
    }
    visited.push(identity.name.clone());
    Ok(true)
}

fn system_now_ms() -> u64 {
    u64::try_from(now_millis()).unwrap_or(0)
}

/// Visits identities from the persisted cursor under `runtime-sweep.lock` (never waits: a second
/// driver gets `Busy`). The cursor is persisted after every identity, so an interrupted pass resumes
/// where it stopped; reaching the end of the sorted list records `completedAt` and clears the
/// cursor (the round is done).
pub fn sweep_host_identities(options: HostSweepOptions<'_>) -> Result<HostSweepResult, String> {
    let HostSweepOptions {
        memory_root,
        mut steps,
        max_identities,
        budget_ms,
        now,
        aborted,
        dry_run,
    } = options;
    let clock: &dyn Fn() -> u64 = now.unwrap_or(&system_now_ms);
    let limits = SweepLimits {
        max_identities,
        budget_ms,
        started_at: clock(),
        now: clock,
        aborted,
    };
    let record = create_lock_record("runtime-sweep", CreateLockRecordOptions::default())
        .map_err(|error| error.to_string())?;
    let acquire = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        ..AcquireLockOptions::default()
    };
    let swept = with_lock(
        &memory_root.join(HOST_SWEEP_LOCK_FILE),
        &record,
        &acquire,
        || -> Result<HostSweepResult, String> {
            let state = read_host_sweep_state(memory_root);
            let unbounded = max_identities.is_none() && budget_ms.is_none();
            let identities = list_runtime_identities(memory_root)?;
            let (rest, wrap) = resume_order(&identities, state.cursor.as_deref(), unbounded);
            let mut visited = Vec::new();
            let mut lines = Vec::new();
            let mut reached_end = true;
            for identity in rest {
                if !visit(identity, &mut steps, &limits, &mut visited, &mut lines)? {
                    reached_end = false;
                    break;
                }
                if !dry_run {
                    let cursor = HostSweepState {
                        cursor: Some(identity.name.clone()),
                        completed_at: None,
                    };
                    write_host_sweep_state(memory_root, &cursor)
                        .map_err(|error| format!("cannot persist the sweep cursor: {error}"))?;
                }
            }
            if reached_end && !dry_run {
                let completed_at = i64::try_from((limits.now)()).unwrap_or(i64::MAX);
                let done = HostSweepState {
                    cursor: None,
                    completed_at: Some(format_rfc3339_millis(completed_at)),
                };
                write_host_sweep_state(memory_root, &done)
                    .map_err(|error| format!("cannot persist the sweep completion: {error}"))?;
            }
            if reached_end {
                for identity in wrap {
                    if !visit(identity, &mut steps, &limits, &mut visited, &mut lines)? {
                        break;
                    }
                }
            }
            Ok(HostSweepResult::Swept {
                visited,
                completed: reached_end,
                lines,
            })
        },
    );
    match swept {
        Ok(result) => Ok(result),
        Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => Ok(HostSweepResult::Busy),
        Err(WithLockError::Acquire(error)) => Err(error.to_string()),
        Err(WithLockError::User(error)) => Err(error),
    }
}

#[cfg(test)]
#[path = "host_sweep_tests.rs"]
mod tests;
