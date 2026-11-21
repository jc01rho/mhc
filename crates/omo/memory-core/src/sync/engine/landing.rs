//! Whether a received key can land in this device's working tree, decided before its render record
//! is written (#12147, apply.rs invariants 2-4). A key the grammar accepts can still be unusable
//! here: it names an existing directory, sits under a file, is longer than any path this OS or its
//! git opens, has two on-disk spellings here, names a path that exists as a non-regular file, or
//! its path goes through or names a symlink (inside the root or not, dangling or not: git refuses
//! to stage a path beyond one, so landing it would wedge). Such a key is refused for this device
//! with the file-system reason, never thrown, because a throw inside the render would leave the
//! durable cycle record failing on every resume. A FIFO leaf is refused before it is read: reading
//! one blocks until a writer opens it, which hangs the cycle (#12158).

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use super::error::EngineResult;
use crate::fs::resilient;
use crate::sync::item::case_fold;

/// The longest path git for Windows stages without `core.longpaths`: MAX_PATH, in wide characters.
const MINGW_MAX_PATH: usize = 260;

/// Why this device's git cannot stage the repository-relative `path`, or `None` when it can. git
/// resolves each pathspec against the repository root and opens the resolved path, so the length
/// that decides is the absolute one, measured in UTF-16 units as git for Windows measures it.
///
/// Only Windows has a limit the OS probe below cannot see (#12199): a key this long is refused
/// with a receipt instead of wedging the commit stage on every resume. POSIX decides through its
/// own probe (PATH_MAX surfaces there as ENAMETOOLONG).
pub fn commit_path_problem(root: &Path, path: &str, windows: bool) -> Option<&'static str> {
    if !windows {
        return None;
    }
    let root = root.to_string_lossy();
    let base = root.trim_end_matches(['\\', '/']);
    let length = base.encode_utf16().count() + 1 + path.encode_utf16().count();
    (length >= MINGW_MAX_PATH).then_some("git-path-too-long")
}

/// The refusal code when `error` says a path cannot be used on this device, else `None`.
pub fn fs_refusal(error: &std::io::Error) -> Option<&'static str> {
    #[cfg(unix)]
    {
        let named = match error.raw_os_error() {
            Some(libc::EISDIR) => Some("eisdir"),
            Some(libc::ENOTDIR) => Some("enotdir"),
            Some(libc::ENAMETOOLONG) => Some("enametoolong"),
            Some(libc::EACCES) => Some("eacces"),
            Some(libc::EPERM) => Some("eperm"),
            Some(libc::ELOOP) => Some("eloop"),
            Some(libc::EEXIST) => Some("eexist"),
            Some(libc::EINVAL) => Some("einval"),
            _ => None,
        };
        if named.is_some() {
            return named;
        }
    }
    match error.kind() {
        ErrorKind::PermissionDenied => Some("eacces"),
        ErrorKind::AlreadyExists => Some("eexist"),
        ErrorKind::InvalidInput => Some("einval"),
        _ => None,
    }
}

/// A path component that does not exist (or sits under a file) reads as absent, not as an error.
fn missing(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::NotFound || under_a_file(error)
}

#[cfg(unix)]
fn under_a_file(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOTDIR)
}

#[cfg(not(unix))]
fn under_a_file(_error: &std::io::Error) -> bool {
    false
}

/// The working text at `root/path`, `None` when no file is there.
pub fn read_working(root: &Path, path: &str) -> std::io::Result<Option<String>> {
    match resilient::read_to_string(&root.join(path)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Where a received key lands, or why it cannot land on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landing {
    Ready {
        path: String,
        working: Option<String>,
    },
    Refused(String),
}

/// Whether a component of `path` that exists under `root`, the last one included, is a symlink.
pub fn through_symlink(root: &Path, path: &str) -> std::io::Result<bool> {
    let mut at = root.to_path_buf();
    for segment in path.split('/') {
        at.push(segment);
        match resilient::symlink_metadata(&at) {
            Ok(entry) if entry.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if missing(&error) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn nearest_existing(root: &Path, dir: &Path) -> PathBuf {
    let mut at = dir.to_path_buf();
    while at != root && !resilient::exists(&at) {
        match at.parent() {
            Some(parent) => at = parent.to_path_buf(),
            None => break,
        }
    }
    at
}

/// Whether `target` (canonical) is `root` (canonical) or inside it, and outside its `.git`.
fn stays_inside(root: &Path, target: &Path) -> bool {
    let Ok(relative) = target.strip_prefix(root) else {
        return false;
    };
    match relative.components().next() {
        None => true,
        Some(Component::Normal(first)) => {
            case_fold(&first.to_string_lossy()).trim_end_matches(['.', ' ']) != ".git"
        }
        Some(_) => false,
    }
}

fn probe(root: &Path, path: &str) -> std::io::Result<Landing> {
    if through_symlink(root, path)? {
        return Ok(Landing::Refused("symlink".into()));
    }
    let target = root.join(path);
    let real_root = resilient::canonicalize(root)?;
    let parent_dir = target.parent().unwrap_or(root);
    let parent = resilient::canonicalize(&nearest_existing(root, parent_dir))?;
    if !stays_inside(&real_root, &parent) {
        return Ok(Landing::Refused("outside-root".into()));
    }
    match resilient::symlink_metadata(&target) {
        Ok(leaf) if !leaf.file_type().is_file() => {
            return Ok(Landing::Refused("non-regular-file".into()));
        }
        Ok(_) => {}
        Err(error) if missing(&error) => {}
        Err(error) => return Err(error),
    }
    let working = read_working(root, path)?;
    resilient::create_dir_all(parent_dir)?;
    Ok(Landing::Ready {
        path: path.to_string(),
        working,
    })
}

/// The working text at a key's on-disk `path` (`None`: ambiguous), after its parent directory
/// exists inside the root.
pub fn prepare_landing(root: &Path, path: Option<&str>) -> EngineResult<Landing> {
    let Some(path) = path else {
        return Ok(Landing::Refused("ambiguous-normalization".into()));
    };
    if let Some(problem) = commit_path_problem(root, path, cfg!(windows)) {
        return Ok(Landing::Refused(problem.into()));
    }
    match probe(root, path) {
        Ok(landing) => Ok(landing),
        Err(error) => match fs_refusal(&error) {
            Some(code) => Ok(Landing::Refused(code.into())),
            None => Err(error.into()),
        },
    }
}

#[cfg(test)]
#[path = "landing_tests.rs"]
mod tests;
