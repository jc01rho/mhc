//! `realpath(3)` semantics with one `lstat`/`readlink` per component and no `open(2)` anywhere (senpi
//! `files/realpath-without-open.ts`). It is the fallback when the kernel's `canonicalize` misses its
//! deadline on a wedged mount (an `open(2)` on an autofs trigger never returns; `lstat` needs only search
//! permission and mounts nothing).
//!
//! `..` is applied to the already-resolved prefix, not collapsed lexically, because the two differ
//! whenever a link target reaches back through a symlink: for `entry -> jump/../secret` with
//! `jump -> /elsewhere/dir`, POSIX resolves `jump` first and lands in `/elsewhere/secret`. Handing a
//! lexical answer to a containment decision would approve one directory while the I/O reached another.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::errors::{MachineOpError, OpErrorCode};
use crate::files::bounded_realpath::is_missing_path_error;
use crate::node_path::{normalize, resolve};

/// Symlink hops allowed while resolving one path; `realpath(3)` reports `ELOOP` past this.
const MAX_SYMLINK_HOPS: u32 = 40;

enum Part {
    Current,
    Parent,
    Name(OsString),
}

fn parts_of(path: &Path) -> Vec<Part> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(_) | Component::RootDir => None,
            Component::CurDir => Some(Part::Current),
            Component::ParentDir => Some(Part::Parent),
            Component::Normal(name) => Some(Part::Name(name.to_os_string())),
        })
        .collect()
}

fn root_of(path: &Path) -> PathBuf {
    path.components()
        .take_while(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
        .collect()
}

/// The root an absolute symlink target starts from: its own prefix, else the current one.
fn adopt_root(current: &Path, link: &Path) -> PathBuf {
    let link_root = root_of(link);
    let has_prefix = link_root
        .components()
        .next()
        .is_some_and(|component| matches!(component, Component::Prefix(_)));
    if has_prefix {
        return link_root;
    }
    let mut adopted: PathBuf = current
        .components()
        .take_while(|component| matches!(component, Component::Prefix(_)))
        .collect();
    adopted.push(link_root);
    adopted
}

fn join_resolved(root: &Path, resolved: &[OsString]) -> PathBuf {
    let mut path = root.to_path_buf();
    path.extend(resolved);
    path
}

struct OpenFreeResolution {
    /// The resolved path; components from an unresolvable one onward are kept verbatim.
    path: PathBuf,
    /// Set when resolution stopped early: the filesystem error, or an `ELOOP` for hop exhaustion.
    error: Option<io::Error>,
}

fn stopped_at(
    candidate: PathBuf,
    pending: &VecDeque<Part>,
    error: io::Error,
) -> OpenFreeResolution {
    let mut path = candidate;
    for part in pending {
        match part {
            Part::Current => {}
            Part::Parent => path.push(".."),
            Part::Name(name) => path.push(name),
        }
    }
    OpenFreeResolution {
        path: normalize(&path),
        error: Some(error),
    }
}

fn resolve_without_open(input: &Path) -> OpenFreeResolution {
    let absolute = if input.is_absolute() {
        input.to_path_buf()
    } else {
        resolve(Path::new(""), input)
    };
    let mut root = root_of(&absolute);
    let mut pending: VecDeque<Part> = parts_of(&absolute).into();
    let mut resolved: Vec<OsString> = Vec::new();
    let mut hops = 0u32;
    while let Some(part) = pending.pop_front() {
        let name = match part {
            Part::Current => continue,
            Part::Parent => {
                // At the root `..` is the root itself, which an empty stack already represents.
                resolved.pop();
                continue;
            }
            Part::Name(name) => name,
        };
        let mut candidate = join_resolved(&root, &resolved);
        candidate.push(&name);
        let link = match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if !metadata.file_type().is_symlink() => {
                resolved.push(name);
                continue;
            }
            Ok(_) => {
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    let error = io::Error::other(format!(
                        "ELOOP: too many levels of symbolic links resolving '{}'",
                        input.display()
                    ));
                    return stopped_at(candidate, &pending, error);
                }
                match std::fs::read_link(&candidate) {
                    Ok(link) => link,
                    Err(error) => return stopped_at(candidate, &pending, error),
                }
            }
            Err(error) => return stopped_at(candidate, &pending, error),
        };
        if link.has_root() {
            resolved.clear();
            root = adopt_root(&root, &link);
        }
        for part in parts_of(&link).into_iter().rev() {
            pending.push_front(part);
        }
    }
    OpenFreeResolution {
        path: join_resolved(&root, &resolved),
        error: None,
    }
}

/// Resolve symlinks without ever `open(2)`-ing a component, tolerating every error. Components from the
/// first unresolvable one onward are kept verbatim, so a file that does not exist yet still lands where
/// its symlinked parent points. Never fails, so the answer may be incomplete: use it where an
/// approximate answer beats a blocked thread, and [`realpath_without_open_strict`] where it must not.
pub fn realpath_without_open(input: &Path) -> PathBuf {
    resolve_without_open(input).path
}

/// [`realpath_without_open`] that refuses to answer with a guess: a missing component is still tolerated
/// (its name is appended verbatim, which a not-yet-created target needs), but `EACCES`, `EIO`, `ELOOP`
/// and hop exhaustion are errors. A containment or identity decision needs this contract, because an
/// unresolved path can name a different file than the one the I/O will reach.
pub fn realpath_without_open_strict(input: &Path) -> io::Result<PathBuf> {
    let resolution = resolve_without_open(input);
    match resolution.error {
        Some(error) if !is_missing_path_error(&error) => Err(io::Error::new(
            error.kind(),
            format!(
                "cannot resolve path '{}' without opening it: {error}",
                input.display()
            ),
        )),
        _ => Ok(resolution.path),
    }
}

/// [`realpath_without_open_strict`] off the async executor's threads.
pub(crate) async fn realpath_without_open_strict_blocking(
    input: PathBuf,
) -> Result<PathBuf, MachineOpError> {
    match tokio::task::spawn_blocking(move || realpath_without_open_strict(&input)).await {
        Ok(resolved) => resolved.map_err(MachineOpError::from),
        Err(join_error) => Err(MachineOpError::new(
            OpErrorCode::IoError,
            format!("path resolution task failed: {join_error}"),
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn base() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
        (dir, canonical)
    }

    #[test]
    fn a_relative_and_an_absolute_link_are_followed() {
        let (_guard, base) = base();
        std::fs::create_dir_all(base.join("real/dir")).unwrap();
        symlink("real/dir", base.join("rel")).unwrap();
        symlink(base.join("real"), base.join("abs")).unwrap();
        assert_eq!(
            realpath_without_open(&base.join("rel")),
            base.join("real/dir")
        );
        assert_eq!(
            realpath_without_open(&base.join("abs/dir")),
            base.join("real/dir")
        );
    }

    #[test]
    fn a_missing_tail_is_kept_verbatim_behind_its_symlinked_parent() {
        let (_guard, base) = base();
        std::fs::create_dir_all(base.join("real")).unwrap();
        symlink("real", base.join("link")).unwrap();
        let resolved = realpath_without_open_strict(&base.join("link/new/file.txt")).unwrap();
        assert_eq!(resolved, base.join("real/new/file.txt"));
    }

    #[test]
    fn dotdot_goes_through_the_resolved_prefix_not_the_lexical_one() {
        let (_guard, base) = base();
        std::fs::create_dir_all(base.join("real/dir")).unwrap();
        symlink("real/dir", base.join("jump")).unwrap();
        symlink("jump/../secret", base.join("entry")).unwrap();
        // Lexically `entry` would answer `<base>/secret`; resolving `jump` first lands next to it in `real`.
        assert_eq!(
            realpath_without_open(&base.join("entry")),
            base.join("real/secret")
        );
    }

    #[test]
    fn a_symlink_loop_fails_closed_in_strict_mode_and_still_answers_in_lenient_mode() {
        let (_guard, base) = base();
        symlink("b", base.join("a")).unwrap();
        symlink("a", base.join("b")).unwrap();
        let error = realpath_without_open_strict(&base.join("a")).unwrap_err();
        assert!(error.to_string().contains("ELOOP"), "{error}");
        assert!(realpath_without_open(&base.join("a")).starts_with(&base));
    }
}
