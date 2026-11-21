//! The few `node:path` semantics senpi's code relies on (lexical `resolve`, glob-pattern absoluteness).
//! A path is normalized lexically, never through the filesystem: symlinks are the canonicalizer's job.

use std::path::{Component, Path, PathBuf};

/// Collapse `.` and `..` lexically. At a root `..` stays at the root; a relative path keeps leading `..`.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let last_is_normal =
                    matches!(out.components().next_back(), Some(Component::Normal(_)));
                let at_root = matches!(
                    out.components().next_back(),
                    Some(Component::Prefix(_) | Component::RootDir)
                );
                if last_is_normal {
                    out.pop();
                } else if !at_root {
                    out.push("..");
                }
            }
            Component::Normal(name) => out.push(name),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `path.resolve(base, path)`: join, make absolute against the process cwd when still relative, normalize.
pub fn resolve(base: &Path, path: &Path) -> PathBuf {
    let joined = base.join(path);
    if joined.is_absolute() {
        return normalize(&joined);
    }
    match std::env::current_dir() {
        Ok(cwd) => normalize(&cwd.join(joined)),
        Err(_) => normalize(&joined),
    }
}

/// `path.isAbsolute` for a glob pattern: a leading `/` is absolute everywhere, a leading `\` on Windows.
pub fn is_absolute_pattern(pattern: &str) -> bool {
    pattern.starts_with('/')
        || (cfg!(windows) && pattern.starts_with('\\'))
        || Path::new(pattern).is_absolute()
}

/// Turn a Windows verbatim path (`\\?\C:\x`, `\\?\UNC\srv\share`) back into its plain form, so a path the
/// kernel canonicalized compares equal to one the open-free walker produced.
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn normalize_collapses_dots_and_stays_at_the_root() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/../..")), PathBuf::from("/"));
        assert_eq!(normalize(Path::new("a/../../b")), PathBuf::from("../b"));
        assert_eq!(normalize(Path::new("a/..")), PathBuf::from("."));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_joins_and_an_absolute_path_wins() {
        assert_eq!(
            resolve(Path::new("/root"), Path::new("dir/../x")),
            PathBuf::from("/root/x")
        );
        assert_eq!(
            resolve(Path::new("/root"), Path::new("/etc/passwd")),
            PathBuf::from("/etc/passwd")
        );
        assert_eq!(
            resolve(Path::new("/root"), Path::new("../../..")),
            PathBuf::from("/")
        );
    }

    #[test]
    fn a_relative_resolve_is_anchored_at_the_process_cwd() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(resolve(Path::new(""), Path::new("x")), cwd.join("x"));
    }

    #[test]
    fn a_pattern_with_a_leading_slash_is_absolute() {
        assert!(is_absolute_pattern("/a/*.ts"));
        assert!(!is_absolute_pattern("a/*.ts"));
        assert!(!is_absolute_pattern("../a"));
    }

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\C:\Users\x")),
            PathBuf::from(r"C:\Users\x")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\srv\share\f")),
            PathBuf::from(r"\\srv\share\f")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("plain/path")),
            PathBuf::from("plain/path")
        );
    }
}
