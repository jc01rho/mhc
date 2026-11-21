use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Removes abandoned `.staging-*` directories and version packages that are not current, previous,
/// or held by a live session lease, then the `versions/<v>.lock` files whose version directory is
/// gone. A held version stays until a later update.
///
/// Current and previous are kept even with no lease: a launcher may have read the pointer and not
/// acquired `versions/<v>.lock` yet. `keep` is unioned with `current.json` when that file is valid.
/// An unreadable pointer skips version removal so current and previous cannot be guessed wrong.
pub(super) fn run(root: &Path, keep: &[&str]) -> Vec<String> {
    let mut warnings = Vec::new();
    remove_abandoned_staging(root, &mut warnings);
    if let Some(protected) = protected_versions(root, keep, &mut warnings) {
        remove_unused_versions(root, &protected, &mut warnings);
    }
    remove_orphan_locks(root, &mut warnings);
    warnings
}

fn remove_abandoned_staging(root: &Path, warnings: &mut Vec<String>) {
    match fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        warnings.push(format!("cannot inspect staging entry: {error}"));
                        continue;
                    }
                };
                if entry.file_name().to_string_lossy().starts_with(".staging-")
                    && let Err(error) = fs::remove_dir_all(entry.path())
                {
                    warnings.push(format!(
                        "cannot remove stale staging {}: {error}",
                        entry.path().display()
                    ));
                }
            }
        }
        Err(error) => warnings.push(format!("cannot list {}: {error}", root.display())),
    }
}

fn protected_versions(
    root: &Path,
    keep: &[&str],
    warnings: &mut Vec<String>,
) -> Option<BTreeSet<String>> {
    let mut protected: BTreeSet<String> = keep
        .iter()
        .copied()
        .map(|version| version.to_owned())
        .collect();
    let pointer_path = root.join(super::pointer::FILE_NAME);
    if !pointer_path.is_file() {
        return Some(protected);
    }
    match super::pointer::read(root) {
        Ok(pointer) => {
            protected.insert(pointer.current);
            if let Some(previous) = pointer.previous {
                protected.insert(previous);
            }
            Some(protected)
        }
        Err(error) => {
            warnings.push(format!(
                "cannot read version pointer; leaving installed versions in place: {error}"
            ));
            None
        }
    }
}

fn remove_unused_versions(root: &Path, protected: &BTreeSet<String>, warnings: &mut Vec<String>) {
    let versions = root.join("versions");
    let entries = match fs::read_dir(&versions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            warnings.push(format!("cannot list {}: {error}", versions.display()));
            return;
        }
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("cannot inspect version entry: {error}"));
                continue;
            }
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !super::pointer::is_valid_version(&name) || protected.contains(&name) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => candidates.push(name),
            Ok(_) => {}
            Err(error) => warnings.push(format!("cannot inspect version {name}: {error}")),
        }
    }
    for name in candidates {
        let directory = versions.join(&name);
        match super::lease::try_exclusive_version(root, &name) {
            Ok(Some(lock)) => match fs::remove_dir_all(&directory) {
                Ok(()) => discard_lock(root, &name, lock, warnings),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    discard_lock(root, &name, lock, warnings);
                }
                Err(error) => {
                    warnings.push(format!(
                        "cannot remove version {}: {error}",
                        directory.display()
                    ));
                    drop(lock);
                }
            },
            Ok(None) => {}
            Err(error) => {
                warnings.push(format!(
                    "cannot check whether version {name} is in use: {error}"
                ));
            }
        }
    }
}

/// Removes `versions/<name>.lock` while `lock`, its exclusive lock, is still held, so no session can
/// take a lease on the file between the check and the removal. Where removing an open file is not
/// portable the handle is released first; a version without a directory has no payload to start.
fn discard_lock(root: &Path, name: &str, lock: fs::File, warnings: &mut Vec<String>) {
    let path = super::lease::version_lock_path(root, name);
    #[cfg(unix)]
    let removed = {
        let removed = fs::remove_file(&path);
        drop(lock);
        removed
    };
    #[cfg(not(unix))]
    let removed = {
        drop(lock);
        fs::remove_file(&path)
    };
    if let Err(error) = removed
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warnings.push(format!(
            "cannot remove version lock {}: {error}",
            path.display()
        ));
    }
}

/// A version's lock outlives the directory it guarded, so one lock file is left behind for every
/// version ever installed. A lock whose directory is gone and that no session holds is removed; one a
/// session still holds (its directory deleted by hand) stays until that session ends.
fn remove_orphan_locks(root: &Path, warnings: &mut Vec<String>) {
    let versions = root.join("versions");
    let entries = match fs::read_dir(&versions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            warnings.push(format!("cannot list {}: {error}", versions.display()));
            return;
        }
    };
    let mut orphans = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("cannot inspect version lock entry: {error}"));
                continue;
            }
        };
        let Ok(file_name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(version) = file_name.strip_suffix(".lock") else {
            continue;
        };
        if !super::pointer::is_valid_version(version)
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
            || versions.join(version).symlink_metadata().is_ok()
        {
            continue;
        }
        orphans.push(version.to_owned());
    }
    for version in orphans {
        match super::lease::try_exclusive_version(root, &version) {
            Ok(Some(lock)) => discard_lock(root, &version, lock, warnings),
            Ok(None) => {}
            Err(error) => {
                warnings.push(format!(
                    "cannot check whether version {version} is in use: {error}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_unheld_versions_and_defers_a_live_lease() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            fs::create_dir_all(root.path().join("versions").join(version))?;
        }
        fs::create_dir(root.path().join(".staging-abandoned"))?;
        let session = crate::native_update::install::SessionLease::acquire(root.path(), "1.0.0")?;
        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(
            root.path().join("versions/1.0.0").is_dir(),
            "live lease defers cleanup"
        );
        assert!(
            !root.path().join("versions/1.1.0").exists(),
            "unheld version is removed"
        );
        assert!(
            root.path().join("versions/1.2.0").is_dir(),
            "kept version stays"
        );
        assert!(!root.path().join(".staging-abandoned").exists());
        drop(session);
        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(
            !root.path().join("versions/1.0.0").exists(),
            "released version is removed on the next update"
        );
        assert!(root.path().join("versions/1.2.0").is_dir());
        Ok(())
    }

    #[test]
    fn keeps_pointer_versions_and_skips_removal_when_the_pointer_is_unreadable()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            fs::create_dir_all(root.path().join("versions").join(version))?;
        }
        super::super::pointer::publish(
            root.path(),
            &super::super::pointer::Pointer::new("1.2.0".into(), Some("1.1.0".into())),
        )?;
        assert!(run(root.path(), &[]).is_empty());
        assert!(root.path().join("versions/1.2.0").is_dir(), "current stays");
        assert!(
            root.path().join("versions/1.1.0").is_dir(),
            "previous stays"
        );
        assert!(!root.path().join("versions/1.0.0").exists());

        fs::create_dir_all(root.path().join("versions/1.0.0"))?;
        fs::write(
            root.path().join(super::super::pointer::FILE_NAME),
            b"not json",
        )?;
        let warnings = run(root.path(), &[]);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("version pointer")),
            "{warnings:?}"
        );
        assert!(
            root.path().join("versions/1.0.0").is_dir(),
            "unreadable pointer blocks version removal"
        );
        assert!(root.path().join("versions/1.2.0").is_dir());
        Ok(())
    }

    #[test]
    fn removes_the_lock_with_its_version_and_the_orphans_older_runs_left()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let versions = root.path().join("versions");
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            fs::create_dir_all(versions.join(version))?;
        }
        fs::write(versions.join("0.8.0.lock"), b"")?;
        fs::write(versions.join("0.9.0.lock"), b"")?;
        fs::write(versions.join("1.2.0.lock"), b"")?;
        let session = crate::native_update::install::SessionLease::acquire(root.path(), "1.0.0")?;

        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(!versions.join("0.8.0.lock").exists(), "orphan lock");
        assert!(!versions.join("0.9.0.lock").exists(), "orphan lock");
        assert!(!versions.join("1.1.0").exists());
        assert!(
            !versions.join("1.1.0.lock").exists(),
            "a removed version leaves no lock"
        );
        assert!(versions.join("1.0.0").is_dir(), "live lease defers cleanup");
        assert!(versions.join("1.0.0.lock").is_file(), "held lock stays");
        assert!(
            versions.join("1.2.0.lock").is_file(),
            "an installed version keeps its lock"
        );

        drop(session);
        assert!(run(root.path(), &["1.2.0"]).is_empty());
        assert!(!versions.join("1.0.0").exists());
        assert!(!versions.join("1.0.0.lock").exists());
        assert!(versions.join("1.2.0.lock").is_file());
        Ok(())
    }

    #[test]
    fn keeps_an_orphan_lock_a_session_still_holds_and_unrelated_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let versions = root.path().join("versions");
        fs::create_dir_all(&versions)?;
        fs::write(versions.join("notes.txt"), b"keep")?;
        let session = crate::native_update::install::SessionLease::acquire(root.path(), "1.0.0")?;

        assert!(run(root.path(), &[]).is_empty());
        assert!(
            versions.join("1.0.0.lock").is_file(),
            "a held lock survives its directory"
        );
        assert!(versions.join("notes.txt").is_file());

        drop(session);
        assert!(run(root.path(), &[]).is_empty());
        assert!(!versions.join("1.0.0.lock").exists());
        assert!(versions.join("notes.txt").is_file());
        Ok(())
    }
}
