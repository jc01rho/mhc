use std::borrow::Cow;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::OnceCell;
use unicode_normalization::UnicodeNormalization;

use crate::files::automount_realpath::{
    AutomountRealpath, AutomountRealpathOptions, realpath_out_of_process,
};
pub use crate::files::mount_table::MountView;
use crate::files::mount_table::{MountEntry, read_mount_view};
use crate::types::BoxFuture;

const MOUNT_VIEW_MAX_AGE: Duration = Duration::from_millis(60_000);

fn raw_segments_of(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|s| !s.is_empty()).collect()
}

pub(crate) fn resolved_segments_of(path: &Path) -> Vec<String> {
    let text = path.to_string_lossy();
    let full = if text.starts_with('/') || path.is_absolute() {
        text.into_owned()
    } else if let Ok(cwd) = std::env::current_dir() {
        format!("{}/{text}", cwd.to_string_lossy())
    } else {
        text.into_owned()
    };
    let mut out = Vec::new();
    for segment in raw_segments_of(&full) {
        match segment {
            "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_owned()),
        }
    }
    out
}

fn fold_segment(segment: &str, case_insensitive: bool) -> Cow<'_, str> {
    if case_insensitive {
        Cow::Owned(segment.nfc().collect::<String>().to_lowercase())
    } else {
        Cow::Borrowed(segment)
    }
}

pub(crate) fn depth_within(root: &str, path: &[String], case_insensitive: bool) -> Option<usize> {
    let root_segments = raw_segments_of(root);
    if root_segments.len() > path.len() {
        return None;
    }
    let matches_prefix = root_segments.iter().enumerate().all(|(index, segment)| {
        fold_segment(segment, case_insensitive) == fold_segment(&path[index], case_insensitive)
    });
    matches_prefix.then_some(root_segments.len())
}

fn through_alias(path: Vec<String>, view: &MountView) -> Vec<String> {
    for (alias, target) in &view.aliases {
        if let Some(depth) = depth_within(alias, &path, view.case_insensitive) {
            let mut mapped: Vec<String> = raw_segments_of(target)
                .into_iter()
                .map(str::to_owned)
                .collect();
            mapped.extend_from_slice(&path[depth..]);
            return mapped;
        }
    }
    path
}

pub fn unmounted_automount_path(path: impl AsRef<Path>, view: &MountView) -> Option<String> {
    let mapped = through_alias(resolved_segments_of(path.as_ref()), view);
    let mut owners: Vec<&MountEntry> = Vec::new();
    let mut owner_depth: Option<usize> = None;
    for entry in &view.mounts {
        let Some(depth) = depth_within(&entry.mount_point, &mapped, view.case_insensitive) else {
            continue;
        };
        if owner_depth.is_some_and(|current| depth < current) {
            continue;
        }
        if owner_depth != Some(depth) {
            owners.clear();
        }
        owner_depth = Some(depth);
        owners.push(entry);
    }
    let owner = *owners.first()?;
    if owners.iter().any(|entry| entry.fs_type != "autofs") {
        return None;
    }
    let depth = owner_depth?;
    if depth >= mapped.len() {
        return Some(owner.mount_point.clone());
    }
    let prefix = owner.mount_point.trim_end_matches('/');
    Some(format!("{prefix}/{}", mapped[depth..].join("/")))
}

pub type ReadMountViewFn = Arc<dyn Fn() -> BoxFuture<'static, MountView> + Send + Sync>;

#[derive(Clone)]
struct CachedView {
    read: u64,
    read_at: Instant,
    cell: Arc<OnceCell<MountView>>,
}

impl CachedView {
    fn new(read: u64) -> Self {
        Self {
            read,
            read_at: Instant::now(),
            cell: Arc::new(OnceCell::new()),
        }
    }
}

#[derive(Clone)]
pub struct AutomountTriggerLookup {
    read_view: ReadMountViewFn,
    latest: Arc<StdMutex<Option<CachedView>>>,
}

impl AutomountTriggerLookup {
    pub async fn lookup(&self, path: impl AsRef<Path>) -> Option<String> {
        let path = path.as_ref();
        let (before, current) = {
            let mut guard = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            let before = guard.as_ref().map_or(0, |entry| entry.read);
            let current = match guard.as_ref() {
                Some(entry) if entry.read_at.elapsed() <= MOUNT_VIEW_MAX_AGE => entry.clone(),
                _ => {
                    let next = CachedView::new(before + 1);
                    *guard = Some(next.clone());
                    next
                }
            };
            (before, current)
        };
        let view = current.cell.get_or_init(|| (self.read_view)()).await;
        let trigger_path = unmounted_automount_path(path, view);
        if trigger_path.is_none() || current.read > before {
            return trigger_path;
        }
        let fresh = {
            let mut guard = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            match guard.as_ref() {
                Some(entry) if entry.read > before => entry.clone(),
                other => {
                    let next = CachedView::new(other.map_or(0, |entry| entry.read) + 1);
                    *guard = Some(next.clone());
                    next
                }
            }
        };
        let fresh_view = fresh.cell.get_or_init(|| (self.read_view)()).await;
        unmounted_automount_path(path, fresh_view)
    }
}

pub fn create_automount_trigger_lookup<F, Fut>(read_view: F) -> AutomountTriggerLookup
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = MountView> + Send + 'static,
{
    AutomountTriggerLookup {
        read_view: Arc::new(move || Box::pin(read_view())),
        latest: Arc::new(StdMutex::new(None)),
    }
}

#[derive(Clone)]
struct ActiveAutomount {
    lookup: AutomountTriggerLookup,
    resolve: AutomountRealpathOptions,
    in_process_calls: Option<Arc<StdMutex<Vec<PathBuf>>>>,
}

fn active_slot() -> &'static StdMutex<ActiveAutomount> {
    static ACTIVE: OnceLock<StdMutex<ActiveAutomount>> = OnceLock::new();
    ACTIVE.get_or_init(|| {
        StdMutex::new(ActiveAutomount {
            lookup: create_automount_trigger_lookup(read_mount_view),
            resolve: AutomountRealpathOptions::default(),
            in_process_calls: None,
        })
    })
}

fn current_active() -> ActiveAutomount {
    active_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

pub(crate) fn record_in_process_call(path: &Path) {
    if let Some(calls) = current_active().in_process_calls {
        calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path.to_path_buf());
    }
}

pub struct AutomountTriggerGuard {
    previous: Option<ActiveAutomount>,
    calls: Arc<StdMutex<Vec<PathBuf>>>,
}

impl AutomountTriggerGuard {
    pub fn clear_in_process_calls(&self) {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    pub fn in_process_calls_under(&self, dir: &Path) -> Vec<PathBuf> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|path| path.as_path() == dir || path.starts_with(dir))
            .cloned()
            .collect()
    }

    pub fn restore(self) {}
}

impl Drop for AutomountTriggerGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            *active_slot().lock().unwrap_or_else(PoisonError::into_inner) = previous;
        }
    }
}

pub fn answer_automount_triggers_from<F, Fut>(
    read_view: F,
    resolve: AutomountRealpathOptions,
) -> AutomountTriggerGuard
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = MountView> + Send + 'static,
{
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let next = ActiveAutomount {
        lookup: create_automount_trigger_lookup(read_view),
        resolve,
        in_process_calls: Some(Arc::clone(&calls)),
    };
    let mut guard = active_slot().lock().unwrap_or_else(PoisonError::into_inner);
    let previous = std::mem::replace(&mut *guard, next);
    drop(guard);
    AutomountTriggerGuard {
        previous: Some(previous),
        calls,
    }
}

pub async fn realpath_under_automount(path: impl AsRef<Path>) -> AutomountRealpath {
    let resolve = current_active().resolve;
    realpath_out_of_process(path.as_ref(), &resolve).await
}

pub async fn automount_trigger_path(path: impl AsRef<Path>) -> Option<String> {
    let lookup = current_active().lookup;
    lookup.lookup(path.as_ref()).await
}
