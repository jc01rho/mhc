//! Port of omo-cpa `src/management-key.ts`. The management key lives in a user-only file,
//! never in omo config and never in the inference-key store.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// `~/.omo/agent/cpa-management-key`, matching the TS default.
pub fn default_management_key_path() -> PathBuf {
    agent_dir().join("cpa-management-key")
}

/// ts `defaultModelsStorePath`'s directory rule: `CODING_AGENT_DIR` when set, else
/// `~/.omo/agent`. Kept local so this crate needs no core dependency.
pub fn agent_dir() -> PathBuf {
    match std::env::var("CODING_AGENT_DIR") {
        Ok(directory) if !directory.trim().is_empty() => PathBuf::from(directory.trim()),
        _ => home_dir().join(".omo").join("agent"),
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Returns the saved secret, treating a missing or unreadable file as unconfigured.
pub fn read_management_key(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// Atomically saves a non-empty secret with owner-only file permissions.
///
/// # Errors
/// Returns the underlying io error when the directory cannot be created or the rename fails, and
/// refuses an empty value exactly as the TS module throws.
pub fn save_management_key(key: &str, path: &Path) -> std::io::Result<()> {
    let value = key.trim();
    if value.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Management Key가 비어 있습니다",
        ));
    }
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
        restrict_directory(directory);
    }
    let temporary = temporary_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let written = file
        .write_all(format!("{value}\n").as_bytes())
        .map_err(cleanup(&temporary));
    drop(file);
    written?;
    if let Err(error) =
        apply_owner_only(&temporary).and_then(|()| std::fs::rename(&temporary, path))
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    apply_owner_only(path)
}

/// Removes a saved secret; clearing an already-absent key is harmless.
///
/// # Errors
/// Returns any error other than "not found", matching the TS ENOENT swallow.
pub fn clear_management_key(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let suffix = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("cpa-management-key"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(format!(".{}.{suffix}.tmp", std::process::id()));
    path.with_file_name(name)
}

fn cleanup(temporary: &Path) -> impl FnOnce(std::io::Error) -> std::io::Error + '_ {
    move |error| {
        let _ = std::fs::remove_file(temporary);
        error
    }
}

#[cfg(unix)]
fn apply_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn apply_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_secret_and_trims_surroundings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("key");
        save_management_key("  secret-value \n", &path).expect("save");
        assert_eq!(read_management_key(&path).as_deref(), Some("secret-value"));
    }

    #[test]
    fn a_missing_file_reads_as_unconfigured() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_management_key(&dir.path().join("absent")), None);
    }

    #[test]
    fn an_empty_secret_is_refused_and_keeps_the_old_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("key");
        save_management_key("first", &path).expect("save");
        assert!(save_management_key("   ", &path).is_err());
        assert_eq!(read_management_key(&path).as_deref(), Some("first"));
    }

    #[test]
    fn clearing_an_absent_key_is_harmless() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("key");
        save_management_key("gone", &path).expect("save");
        clear_management_key(&path).expect("clear");
        clear_management_key(&path).expect("clear twice");
        assert_eq!(read_management_key(&path), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("key");
        save_management_key("secret", &path).expect("save");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
