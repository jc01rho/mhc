use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::OnceCell;

use crate::files::automount_realpath::{
    AutomountRealpath, AutomountRealpathOptions, realpath_out_of_process,
};
use crate::files::mount_table::{
    MountView, depth_within, read_mount_table, resolved_segments_of, unmounted_automount_path,
};
use crate::node_path::{normalize, resolve};
use crate::types::BoxFuture;

const MOUNT_VIEW_MAX_AGE: Duration = Duration::from_millis(60_000);

pub(crate) async fn read_mount_view() -> MountView {
    let mounts = read_mount_table().await;
    let roots: Vec<String> = mounts
        .iter()
        .filter(|entry| entry.fs_type == "autofs")
        .map(|entry| entry.mount_point.clone())
        .collect();
    let case_insensitive = cfg!(target_os = "macos");
    let mut aliases = BTreeMap::new();
    if !roots.is_empty()
        && let Ok(mut entries) = tokio::fs::read_dir("/").await
    {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if !entry.file_type().await.is_ok_and(|ft| ft.is_symlink()) {
                continue;
            }
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            let alias = format!("/{name_str}");
            let Ok(link) = tokio::fs::read_link(&alias).await else {
                continue;
            };
            let target_path = if link.is_absolute() {
                normalize(&link)
            } else {
                resolve(Path::new("/"), &link)
            };
            let target = target_path.to_string_lossy().into_owned();
            let target_segments = resolved_segments_of(Path::new(&target));
            if roots
                .iter()
                .any(|root| depth_within(root, &target_segments, case_insensitive).is_some())
            {
                aliases.insert(alias, target);
            }
        }
    }
    MountView {
        mounts,
        aliases,
        case_insensitive,
    }
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
    fn view_at_least(&self, after: u64) -> CachedView {
        let mut guard = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = guard.as_ref()
            && entry.read > after
        {
            return entry.clone();
        }
        let next = CachedView::new(guard.as_ref().map_or(after, |entry| entry.read) + 1);
        *guard = Some(next.clone());
        next
    }

    pub fn lookup(&self, path: impl AsRef<Path>) -> BoxFuture<'static, Option<String>> {
        let path = path.as_ref().to_path_buf();
        let before = {
            let mut guard = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            let before = guard.as_ref().map_or(0, |entry| entry.read);
            let fresh_enough = guard
                .as_ref()
                .is_some_and(|entry| entry.read_at.elapsed() <= MOUNT_VIEW_MAX_AGE);
            if !fresh_enough {
                *guard = Some(CachedView::new(before + 1));
            }
            before
        };
        let read_view = Arc::clone(&self.read_view);
        let latest = Arc::clone(&self.latest);
        Box::pin(async move {
            let first = latest
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()?;
            let view = first.cell.get_or_init(|| read_view()).await;
            let trigger_path = unmounted_automount_path(&path, view);
            if trigger_path.is_none() || first.read > before {
                return trigger_path;
            }
            let fresh = AutomountTriggerLookup {
                read_view: Arc::clone(&read_view),
                latest: Arc::clone(&latest),
            }
            .view_at_least(before);
            let fresh_view = fresh.cell.get_or_init(|| read_view()).await;
            unmounted_automount_path(&path, fresh_view)
        })
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
