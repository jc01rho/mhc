//! Where a process starts, judged where the path really leads (senpi `rpc-machine-op/real-path.ts`, the
//! path confinement only). The path is walked segment by segment: an existing symlink is resolved before
//! the next segment, `.` is skipped and `..` goes to the parent of the real directory reached so far;
//! past a missing segment the rest is appended as text. A dangling or looping link, a drive-relative
//! segment (`C:foo` names another drive's cwd, never a jailed name) or a call past the resolution
//! deadline has no real target.

use std::ffi::OsStr;
use std::future::Future;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::errors::MachineOpError;
use crate::files::automount_realpath::AutomountRealpath;
use crate::files::automount_trigger::{automount_trigger_path, realpath_under_automount};
use crate::files::bounded_realpath::{
    RESOLUTION_DEADLINE, canonicalize_path, with_resolution_deadline,
};
use crate::node_path::resolve;

enum Lookup<T> {
    Found(T),
    Missing,
    Timeout,
}

async fn bounded<T>(operation: impl Future<Output = io::Result<T>>) -> Lookup<T> {
    match with_resolution_deadline(operation, RESOLUTION_DEADLINE).await {
        Ok(Ok(value)) => Lookup::Found(value),
        Ok(Err(_)) => Lookup::Missing,
        Err(_) => Lookup::Timeout,
    }
}

fn is_drive_relative(segment: &OsStr) -> bool {
    segment.to_str().is_some_and(|text| {
        let bytes = text.as_bytes();
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
    })
}

async fn real_path_under(base: &Path, raw: &Path) -> Option<PathBuf> {
    if automount_trigger_path(resolve(base, raw)).await.is_some() {
        let unnormalized = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            base.join(raw)
        };
        return match realpath_under_automount(&unnormalized).await {
            AutomountRealpath::Resolved(path) => Some(path),
            AutomountRealpath::Failed { .. } | AutomountRealpath::Unanswered => None,
        };
    }
    let rooted = raw.has_root();
    let root: PathBuf = if rooted {
        raw.components()
            .take_while(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
            .collect()
    } else {
        base.to_path_buf()
    };
    let Lookup::Found(start) = bounded(canonicalize_path(&root)).await else {
        return None;
    };
    let mut current = start;
    let mut missing = false;
    for component in raw.components() {
        match component {
            Component::Prefix(_) => {
                if rooted {
                    continue;
                }
                return None;
            }
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if let Some(parent) = current.parent().map(Path::to_path_buf) {
                    current = parent;
                }
            }
            Component::Normal(segment) => {
                if is_drive_relative(segment) {
                    return None;
                }
                let next = current.join(segment);
                if missing {
                    current = next;
                    continue;
                }
                match bounded(tokio::fs::symlink_metadata(&next)).await {
                    Lookup::Timeout => return None,
                    Lookup::Missing => {
                        missing = true;
                        current = next;
                    }
                    Lookup::Found(metadata) if !metadata.file_type().is_symlink() => {
                        current = next;
                    }
                    Lookup::Found(_) => match bounded(canonicalize_path(&next)).await {
                        Lookup::Found(target) => current = target,
                        Lookup::Missing | Lookup::Timeout => return None,
                    },
                }
            }
        }
    }
    Some(current)
}

/// The real place a process starts, checked against a confined root (`policy_denied` when it does not
/// resolve inside); an unconfined root only anchors a relative path, and the OS decides whether the process
/// may start there.
pub async fn spawn_path_in(
    root: &Path,
    raw: &Path,
    confined: bool,
) -> Result<PathBuf, MachineOpError> {
    let real = real_path_under(root, raw).await;
    if !confined {
        return Ok(real.unwrap_or_else(|| resolve(root, raw)));
    }
    let real_root = real_path_under(root, Path::new(".")).await;
    match (real_root, real) {
        (Some(real_root), Some(real)) if real.starts_with(&real_root) => Ok(real),
        _ => Err(MachineOpError::policy_denied(format!(
            "{} does not resolve inside {}",
            raw.display(),
            root.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drive_relative_segment_is_recognized() {
        assert!(is_drive_relative(OsStr::new("C:foo")));
        assert!(is_drive_relative(OsStr::new("c:")));
        assert!(!is_drive_relative(OsStr::new("foo")));
        assert!(!is_drive_relative(OsStr::new(":a")));
    }

    #[tokio::test]
    async fn a_path_inside_the_root_resolves_to_its_real_location() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("work")).unwrap();
        let real = spawn_path_in(&root, Path::new("work/./sub/../x"), true)
            .await
            .unwrap();
        assert_eq!(real, root.join("work/x"));
    }
}
