//! Port of `fs/remove-tree.ts` (omo #12517, H2): removes a file or directory tree, tolerating
//! read-only content. Reflection and facts runs chmod their payload files to 0o400; on POSIX that
//! never blocks an unlink, but on Windows it sets the read-only attribute and the unlink fails, and
//! a 0o500 directory blocks removal on every platform (darwin reports it as ENOTEMPTY). On a
//! permission failure the tree is made writable (files 0o600, directories 0o700) and removed once
//! more. A missing path is success.

use std::io::{self, ErrorKind};
use std::path::Path;

use super::resilient::{read_dir_names, remove_dir_all, remove_file, symlink_metadata};

const WINDOWS_BUSY_RETRIES: u32 = 5;

pub fn remove_tree(path: &Path) -> io::Result<()> {
    remove_tree_on(path, cfg!(windows))
}

fn remove_tree_on(path: &Path, windows: bool) -> io::Result<()> {
    match remove_forced(path) {
        Ok(()) => return Ok(()),
        Err(error) if !is_retryable(&error, windows) => return Err(error),
        Err(_) => {}
    }
    make_writable(path)?;
    let attempts = if windows { WINDOWS_BUSY_RETRIES } else { 1 };
    let mut attempt = 1;
    loop {
        match remove_forced(path) {
            Ok(()) => return Ok(()),
            Err(error) if attempt >= attempts || error.kind() != ErrorKind::ResourceBusy => {
                return Err(error);
            }
            Err(_) => attempt += 1,
        }
    }
}

fn is_retryable(error: &io::Error, windows: bool) -> bool {
    match error.kind() {
        ErrorKind::PermissionDenied | ErrorKind::DirectoryNotEmpty => true,
        ErrorKind::ResourceBusy => windows,
        _ => false,
    }
}

fn remove_forced(path: &Path) -> io::Result<()> {
    let metadata = match symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let removed = if metadata.is_dir() {
        remove_dir_all(path)
    } else {
        remove_file(path).or_else(|error| {
            if metadata.file_type().is_symlink() && cfg!(windows) {
                std::fs::remove_dir(path)
            } else {
                Err(error)
            }
        })
    };
    match removed {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn make_writable(path: &Path) -> io::Result<()> {
    let metadata = match symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !metadata.is_dir() {
        return set_owner_mode(path, 0o600);
    }
    set_owner_mode(path, 0o700)?;
    for name in read_dir_names(path)? {
        make_writable(&path.join(name))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    super::retry::retry_on_eintr(|| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    })
}

#[cfg(not(unix))]
fn set_owner_mode(path: &Path, _mode: u32) -> io::Result<()> {
    let mut permissions = super::resilient::metadata(path)?.permissions();
    // Windows has no mode bits: clearing the read-only attribute is what makes the entry removable.
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)
}

#[cfg(test)]
#[path = "remove_tree_tests.rs"]
mod tests;
