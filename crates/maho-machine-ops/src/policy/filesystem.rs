//! Filesystem policy and canonicalization (senpi `policy/filesystem.ts`).

use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::errors::MachineOpError;
use crate::files::bounded_realpath::{
    RESOLUTION_DEADLINE, ResolutionTimedOut, canonicalize_path, is_missing_path_error,
    with_resolution_deadline,
};
use crate::files::realpath_without_open::realpath_without_open_strict_blocking;
use crate::node_path::resolve;
use crate::types::{BoxFuture, OpName};

/// Symlink hops followed while canonicalizing one path before it is reported as a loop.
const MAX_SYMLINK_HOPS: u32 = 40;

/// What a file operation is about to do with a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemOperation {
    Read,
    Enumerate,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemPolicyRequest {
    pub operation: FilesystemOperation,
    pub canonical_path: PathBuf,
    pub tool_name: OpName,
}

/// A filesystem policy must explicitly allow or deny each request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesystemPolicyDecision {
    Allow,
    Deny { reason: String },
}

impl FilesystemPolicyDecision {
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }

    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// One registered filesystem access policy. `denied_roots` is metadata; callers enforce `check`.
pub trait FilesystemPolicy: Send + Sync {
    fn check<'a>(
        &'a self,
        request: &'a FilesystemPolicyRequest,
    ) -> BoxFuture<'a, FilesystemPolicyDecision>;

    fn denied_roots(&self) -> &[PathBuf] {
        &[]
    }
}

struct ComposedFilesystemPolicy {
    policies: Vec<Arc<dyn FilesystemPolicy>>,
}

impl FilesystemPolicy for ComposedFilesystemPolicy {
    fn check<'a>(
        &'a self,
        request: &'a FilesystemPolicyRequest,
    ) -> BoxFuture<'a, FilesystemPolicyDecision> {
        Box::pin(async move {
            for policy in &self.policies {
                let decision = policy.check(request).await;
                if !decision.is_allowed() {
                    return decision;
                }
            }
            FilesystemPolicyDecision::Allow
        })
    }
}

/// Compose policies in registration order. The first denial wins and later policies are not asked.
pub fn compose_filesystem_policies(
    policies: Vec<Arc<dyn FilesystemPolicy>>,
) -> Option<Arc<dyn FilesystemPolicy>> {
    if policies.is_empty() {
        return None;
    }
    Some(Arc::new(ComposedFilesystemPolicy { policies }))
}

fn join_missing(mut parent: PathBuf, missing: &[OsString]) -> PathBuf {
    for name in missing.iter().rev() {
        parent.push(name);
    }
    parent
}

async fn canonicalize_by_opening(file_path: PathBuf) -> io::Result<PathBuf> {
    let mut current = file_path.clone();
    let mut missing: Vec<OsString> = Vec::new();
    let mut hops = 0u32;
    loop {
        match canonicalize_path(&current).await {
            Ok(canonical_parent) => return Ok(join_missing(canonical_parent, &missing)),
            Err(error) if is_missing_path_error(&error) => {}
            Err(error) => return Err(error),
        }

        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    return Err(io::Error::other(format!(
                        "ELOOP: too many levels of symbolic links resolving '{}'",
                        file_path.display()
                    )));
                }
                let target = tokio::fs::read_link(&current).await?;
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
            Ok(_) => {}
            Err(error) if is_missing_path_error(&error) => {}
            Err(error) => return Err(error),
        }

        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            return Ok(file_path);
        };
        if let Some(name) = current.file_name() {
            missing.push(name.to_os_string());
        }
        current = parent;
    }
}

/// [`canonicalize_filesystem_path`] with the opener and the deadline injected: `open_resolver` is the
/// real-`canonicalize` traversal, and when it misses `deadline` the open-free walker answers instead.
pub async fn canonicalize_filesystem_path_with<F, Fut>(
    file_path: &Path,
    deadline: Duration,
    open_resolver: F,
) -> Result<PathBuf, MachineOpError>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = io::Result<PathBuf>>,
{
    let absolute_path = resolve(Path::new(""), file_path);
    match with_resolution_deadline(open_resolver(absolute_path.clone()), deadline).await {
        Ok(result) => result.map_err(MachineOpError::from),
        Err(ResolutionTimedOut) => realpath_without_open_strict_blocking(absolute_path).await,
    }
}

/// Resolve a filesystem target through existing symlinks. Missing descendants are appended to the nearest
/// existing real parent so new write targets still receive a stable canonical path.
///
/// The answer feeds containment decisions, so it comes from real `canonicalize` traversal: that supplies
/// the on-disk case of a case-insensitive name, applies `..` the way the following I/O will, and fails
/// closed on `EACCES`/`EIO`/`ELOOP` instead of guessing. Because this runs before every operation and
/// `canonicalize` never returns on a wedged mount, the traversal is bounded by
/// [`RESOLUTION_DEADLINE`]; past it the open-free walker answers, which still refuses to guess past a
/// non-missing error.
pub async fn canonicalize_filesystem_path(file_path: &Path) -> Result<PathBuf, MachineOpError> {
    canonicalize_filesystem_path_with(file_path, RESOLUTION_DEADLINE, canonicalize_by_opening).await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Verdict {
        decision: FilesystemPolicyDecision,
        asked: Mutex<u32>,
    }

    impl Verdict {
        fn new(decision: FilesystemPolicyDecision) -> Arc<Self> {
            Arc::new(Self {
                decision,
                asked: Mutex::new(0),
            })
        }

        fn asked(&self) -> u32 {
            *self.asked.lock().expect("verdict lock")
        }
    }

    impl FilesystemPolicy for Verdict {
        fn check<'a>(
            &'a self,
            _request: &'a FilesystemPolicyRequest,
        ) -> BoxFuture<'a, FilesystemPolicyDecision> {
            Box::pin(async move {
                *self.asked.lock().expect("verdict lock") += 1;
                self.decision.clone()
            })
        }
    }

    fn request() -> FilesystemPolicyRequest {
        FilesystemPolicyRequest {
            operation: FilesystemOperation::Read,
            canonical_path: PathBuf::from("/x"),
            tool_name: OpName::FilesRead,
        }
    }

    #[test]
    fn no_policies_compose_to_nothing() {
        assert!(compose_filesystem_policies(Vec::new()).is_none());
    }

    #[tokio::test]
    async fn the_first_denial_wins_and_later_policies_are_not_asked() {
        let allow = Verdict::new(FilesystemPolicyDecision::Allow);
        let deny = Verdict::new(FilesystemPolicyDecision::deny("first denial"));
        let later = Verdict::new(FilesystemPolicyDecision::deny("never asked"));
        let composed = compose_filesystem_policies(vec![
            allow.clone() as Arc<dyn FilesystemPolicy>,
            deny.clone() as Arc<dyn FilesystemPolicy>,
            later.clone() as Arc<dyn FilesystemPolicy>,
        ])
        .unwrap();
        let decision = composed.check(&request()).await;
        assert_eq!(decision, FilesystemPolicyDecision::deny("first denial"));
        assert_eq!((allow.asked(), deny.asked(), later.asked()), (1, 1, 0));
    }

    #[tokio::test]
    async fn all_allowing_policies_allow() {
        let one = Verdict::new(FilesystemPolicyDecision::Allow);
        let two = Verdict::new(FilesystemPolicyDecision::Allow);
        let composed = compose_filesystem_policies(vec![
            one.clone() as Arc<dyn FilesystemPolicy>,
            two.clone() as Arc<dyn FilesystemPolicy>,
        ])
        .unwrap();
        assert!(composed.check(&request()).await.is_allowed());
        assert_eq!((one.asked(), two.asked()), (1, 1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_missing_tail_is_appended_to_the_canonical_parent_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(base.join("real")).unwrap();
        std::os::unix::fs::symlink("real", base.join("link")).unwrap();
        let canonical = canonicalize_filesystem_path(&base.join("link/new/file.txt"))
            .await
            .unwrap();
        assert_eq!(canonical, base.join("real/new/file.txt"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_symlink_is_followed_to_where_a_write_would_land() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real/not-yet.txt"), base.join("dangling")).unwrap();
        let canonical = canonicalize_filesystem_path(&base.join("dangling"))
            .await
            .unwrap();
        assert_eq!(canonical, base.join("real/not-yet.txt"));
    }
}
