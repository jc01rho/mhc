//! Isolated canonicalization for paths under an unmounted automount trigger (senpi `files/automount-realpath.ts`).
//!
//! A path under an automount trigger must never run on Tokio's shared blocking pool: an automounter that
//! does not answer keeps that blocking thread forever (senpi#7157). Instead, resolution runs on a
//! dedicated OS thread bounded by a deadline; if the automounter does not answer before the deadline,
//! the caller is released with [`AutomountRealpath::Unanswered`].

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::files::bounded_realpath::is_missing_path_error;
use crate::node_path::{normalize, resolve, strip_verbatim_prefix};

const AUTOMOUNT_RESOLUTION_DEADLINE_MS: u64 = 5000;
const AUTOMOUNT_RESOLUTION_DEADLINE: Duration =
    Duration::from_millis(AUTOMOUNT_RESOLUTION_DEADLINE_MS);
const MAX_SYMLINK_HOPS: u32 = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomountRealpath {
    Resolved(PathBuf),
    Failed { code: String },
    Unanswered,
}

#[derive(Debug, Clone, Default)]
pub struct AutomountAbort {
    inner: Arc<(StdMutex<bool>, Condvar)>,
}

impl AutomountAbort {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        let (lock, cvar) = &*self.inner;
        let mut aborted = lock.lock().unwrap_or_else(PoisonError::into_inner);
        *aborted = true;
        cvar.notify_all();
    }

    pub fn is_aborted(&self) -> bool {
        let (lock, _) = &*self.inner;
        *lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn wait_aborted(&self, max_wait: Duration) -> bool {
        let (lock, cvar) = &*self.inner;
        let guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = cvar
            .wait_timeout_while(guard, max_wait, |aborted| !*aborted)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

pub type AutomountResolverFn =
    Arc<dyn Fn(&Path, AutomountAbort) -> AutomountRealpath + Send + Sync>;

#[derive(Clone, Default)]
pub struct AutomountRealpathOptions {
    pub deadline: Option<Duration>,
    pub resolver: Option<AutomountResolverFn>,
}

fn canonicalize_sync(path: &Path) -> io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(if cfg!(windows) {
        strip_verbatim_prefix(&canonical)
    } else {
        canonical
    })
}

fn join_missing(mut parent: PathBuf, missing: &[OsString]) -> PathBuf {
    for name in missing.iter().rev() {
        parent.push(name);
    }
    normalize(&parent)
}

fn peel_parent(path: &Path) -> Option<(PathBuf, Option<OsString>)> {
    let mut components: Vec<Component<'_>> = path.components().collect();
    let last = components.pop()?;
    let segment = match last {
        Component::Prefix(_) | Component::RootDir => return None,
        Component::CurDir => None,
        Component::ParentDir => Some(OsString::from("..")),
        Component::Normal(name) => Some(name.to_os_string()),
    };
    let parent: PathBuf = components.into_iter().collect();
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some((parent, segment))
}

fn errno_code(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::NotADirectory => "ENOTDIR",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::FilesystemLoop => "ELOOP",
        _ => "EIO",
    }
}

pub fn canonicalize_by_opening_sync(file_path: &Path) -> AutomountRealpath {
    let mut current = file_path.to_path_buf();
    let mut missing: Vec<OsString> = Vec::new();
    let mut hops = 0u32;
    loop {
        match canonicalize_sync(&current) {
            Ok(canonical_parent) => {
                return AutomountRealpath::Resolved(join_missing(canonical_parent, &missing));
            }
            Err(error) if is_missing_path_error(&error) => {}
            Err(error) => {
                return AutomountRealpath::Failed {
                    code: errno_code(&error).to_owned(),
                };
            }
        }

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    return AutomountRealpath::Failed {
                        code: "ELOOP".to_owned(),
                    };
                }
                match std::fs::read_link(&current) {
                    Ok(target) => {
                        let directory = current
                            .parent()
                            .map_or_else(PathBuf::new, Path::to_path_buf);
                        let mut next = directory.join(target);
                        for name in missing.iter().rev() {
                            next.push(name);
                        }
                        current = resolve(Path::new(""), &next);
                        missing.clear();
                        continue;
                    }
                    Err(error) if is_missing_path_error(&error) => {}
                    Err(error) => {
                        return AutomountRealpath::Failed {
                            code: errno_code(&error).to_owned(),
                        };
                    }
                }
            }
            Ok(_) => {}
            Err(error) if is_missing_path_error(&error) => {}
            Err(error) => {
                return AutomountRealpath::Failed {
                    code: errno_code(&error).to_owned(),
                };
            }
        }

        let Some((parent, segment)) = peel_parent(&current) else {
            return AutomountRealpath::Resolved(file_path.to_path_buf());
        };
        if let Some(name) = segment {
            missing.push(name);
        }
        current = parent;
    }
}

pub async fn realpath_out_of_process(
    path: &Path,
    options: &AutomountRealpathOptions,
) -> AutomountRealpath {
    let deadline = options.deadline.unwrap_or(AUTOMOUNT_RESOLUTION_DEADLINE);
    let abort = AutomountAbort::new();
    let worker_abort = abort.clone();
    let target = path.to_path_buf();
    let resolver = options.resolver.clone();
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let answer = match resolver {
            Some(resolve_fn) => resolve_fn(&target, worker_abort),
            None => canonicalize_by_opening_sync(&target),
        };
        if tx.send(answer).is_err() {}
    });
    match timeout(deadline, rx).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) => AutomountRealpath::Failed {
            code: "EIO".to_owned(),
        },
        Err(_) => {
            abort.abort();
            AutomountRealpath::Unanswered
        }
    }
}
