//! The socket secret file's mechanics (senpi `modes/rpc/socket-transport.ts`: `ensureSocketSecret`,
//! `createSocketSecret` and their helpers; sisyphuslabs/omo-desktop-app#4199 design 4.2-4.5).
//!
//! A socket secret is 32 random bytes in `<socket>.secret`, readable by its owner only. This crate owns what happens
//! to that file, so the RPC transport and the terminal's control endpoint cannot disagree about it:
//!
//! - an entry is only ever read as a REGULAR file: a terminal symlink or any other entry type is refused, never
//!   followed or written through ([`read_valid_socket_secret`]);
//! - the first secret is published through a private same-directory staging file and `link(2)`, which never replaces
//!   an existing entry, so two creators agree on one winner and the final path is never opened for writing
//!   ([`create_socket_secret`]);
//! - an invalid secret is replaced by one `rename(2)`, and the caller holds the per-secret lock for it
//!   ([`socket_secret_lock_path`], [`provision_socket_secret_locked`]), so the old entry stays until that rename.
//!
//! ADAPTATION: senpi keeps the in-place writer on win32 until its native no-replace bridge (`crates/senpi-socket-secret`,
//! senpi#4310 and #4312) is wired, and so does this crate: there the secret is written in place. The Windows adapter
//! itself (owner-only DACL, `CREATE_NEW` staging, presence guards, volume locality) is not ported: nothing in senpi
//! calls it yet, so there is no behavior to match and nothing to test it against.
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

/// The secret's length in bytes.
pub const SOCKET_SECRET_BYTES: usize = 32;

/// The directory a path lives in; a bare file name lives in the working directory.
fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn not_a_regular_file(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "RPC socket secret is not a regular file: {}",
            path.display()
        ),
    )
}

#[cfg(unix)]
fn invalid_and_not_replaced(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "RPC socket secret is invalid and was not replaced: {}",
            path.display()
        ),
    )
}

fn random_secret() -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    let mut secret = [0u8; SOCKET_SECRET_BYTES];
    getrandom::fill(&mut secret).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(secret)
}

/// The owner-only directory a secret lives in, created with its missing parents (senpi `mkdir(..., { mode: 0o700 })`).
fn create_private_directory(directory: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)
}

/// Creates the directory a secret at `path` lives in, owner-only where the platform has modes.
pub fn prepare_socket_secret_parent(path: &Path) -> io::Result<()> {
    create_private_directory(parent_of(path))
}

/// The bytes of the regular file at `path`, read up to one byte past the valid length, or `None` when nothing is
/// there. A terminal symlink or another entry type is an error, not followed and not written through: only a
/// regular file is a credential this process may reuse or repair (senpi `readSecretEntry`).
fn read_secret_entry(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(not_a_regular_file(path));
        }
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(not_a_regular_file(path));
    }
    let mut buffer = Vec::with_capacity(SOCKET_SECRET_BYTES + 1);
    let mut limited = file.take(SOCKET_SECRET_BYTES as u64 + 1);
    limited.read_to_end(&mut buffer)?;
    Ok(Some(buffer))
}

fn valid_secret(bytes: &[u8]) -> Option<[u8; SOCKET_SECRET_BYTES]> {
    <[u8; SOCKET_SECRET_BYTES]>::try_from(bytes).ok()
}

/// The valid secret at `path`: `None` when nothing is there or the entry is not exactly [`SOCKET_SECRET_BYTES`] long.
/// A symlink or another non-regular entry is an error.
pub fn read_valid_socket_secret(path: &Path) -> io::Result<Option<[u8; SOCKET_SECRET_BYTES]>> {
    Ok(read_secret_entry(path)?.and_then(|bytes| valid_secret(&bytes)))
}

/// How a first publication ended.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Publication {
    Published,
    Collision,
}

/// This attempt's private staging file, beside the final path so the publication never crosses a file system.
#[cfg(unix)]
fn staging_path(path: &Path) -> io::Result<PathBuf> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(parent_of(path).join(format!(
        ".{}.stage-{}-{suffix}",
        file_name_of(path),
        std::process::id()
    )))
}

#[cfg(unix)]
fn write_synced(file: &mut std::fs::File, candidate: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    // The creation mode is filtered by the umask; set it on the descriptor before any byte is written.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(candidate)?;
    file.sync_all()
}

/// Writes `candidate` to a new owner-only staging file, exclusively created and synced. A failure removes the file
/// again: this attempt's staging file never outlives it.
#[cfg(unix)]
fn write_staging(staging: &Path, candidate: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(staging)?;
    let written = write_synced(&mut file, candidate);
    drop(file);
    if let Err(error) = written {
        let _ = std::fs::remove_file(staging);
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    std::fs::File::open(directory)?.sync_all()
}

/// Publishes `candidate` at `path` unless an entry already exists there (senpi `publishSocketSecretIfAbsent`): the
/// candidate goes to a private staging file and `link(2)` publishes it, which never replaces an existing entry. The
/// final path is never opened for writing, and cleanup only ever removes this attempt's own staging file.
#[cfg(unix)]
fn publish_secret_if_absent(path: &Path, candidate: &[u8]) -> io::Result<Publication> {
    let staging = staging_path(path)?;
    write_staging(&staging, candidate)?;
    let outcome = match std::fs::hard_link(&staging, path) {
        Ok(()) => sync_directory(parent_of(path)).map(|()| Publication::Published),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(Publication::Collision),
        Err(error) => Err(error),
    };
    match std::fs::remove_file(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    outcome
}

/// Replaces an invalid secret with `candidate` in one rename; the old entry stays until that rename (design 4.4).
#[cfg(unix)]
fn replace_secret(path: &Path, candidate: &[u8]) -> io::Result<()> {
    let staging = staging_path(path)?;
    write_staging(&staging, candidate)?;
    if let Err(error) = std::fs::rename(&staging, path) {
        let _ = std::fs::remove_file(&staging);
        return Err(error);
    }
    sync_directory(parent_of(path))
}

/// The winner of a first-publication race: the valid secret now at `path`, or an error when the entry is invalid.
#[cfg(unix)]
fn winner_of(path: &Path) -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    read_valid_socket_secret(path)?.ok_or_else(|| invalid_and_not_replaced(path))
}

/// senpi `writeSocketSecretInPlace`: the win32 writer, which truncates and rewrites the file.
#[cfg(not(unix))]
fn write_secret_in_place(path: &Path) -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    create_private_directory(parent_of(path))?;
    let secret = random_secret()?;
    std::fs::write(path, secret)?;
    Ok(secret)
}

/// Creates the secret at `path` unless one is already there (senpi `createSocketSecret`, design 4.3): a valid regular
/// file already at the path, or one published by a concurrent creator, is returned unchanged; an invalid one is
/// refused, never replaced.
#[cfg(unix)]
pub fn create_socket_secret(path: &Path) -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    create_private_directory(parent_of(path))?;
    let candidate = random_secret()?;
    if read_secret_entry(path)?.is_none()
        && publish_secret_if_absent(path, &candidate)? == Publication::Published
    {
        return Ok(candidate);
    }
    winner_of(path)
}

/// Creates the secret at `path`, win32: written in place, as senpi does until its no-replace bridge is wired.
#[cfg(not(unix))]
pub fn create_socket_secret(path: &Path) -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    write_secret_in_place(path)
}

/// The locked half of senpi `ensureSocketSecret`: the caller holds the lock of [`socket_secret_lock_path`]. The
/// caller's first read is only a hint (design 4.4): a contender may have published or repaired since, so the entry
/// is read again here, and a valid secret is returned as it is. A missing one is published without replacing
/// anything; an invalid one is replaced by one rename.
#[cfg(unix)]
pub fn provision_socket_secret_locked(path: &Path) -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    let current = read_secret_entry(path)?;
    if let Some(secret) = current.as_deref().and_then(valid_secret) {
        return Ok(secret);
    }
    let candidate = random_secret()?;
    if current.is_none() {
        if publish_secret_if_absent(path, &candidate)? == Publication::Published {
            return Ok(candidate);
        }
        return winner_of(path);
    }
    replace_secret(path, &candidate)?;
    Ok(candidate)
}

/// The locked half of senpi `ensureSocketSecret`, win32: a valid secret is returned as it is, anything else is
/// written in place.
#[cfg(not(unix))]
pub fn provision_socket_secret_locked(path: &Path) -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    if let Some(secret) = read_valid_socket_secret(path)? {
        return Ok(secret);
    }
    write_secret_in_place(path)
}

/// `\\?\C:\dir` as `C:\dir`: node's `realpath` never spells a Windows path with the verbatim prefix, and the lock
/// database below is opened by this name. UNC verbatim paths are left as they are.
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let stripped = path
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        .filter(|rest| !rest.starts_with("UNC\\"))
        .map(PathBuf::from);
    stripped.unwrap_or(path)
}

/// The per-secret mutation lock (design 4.2): `<canonical parent>/<name>.publish.lock`, so every spelling of one
/// physical secret takes the same lock. The file is created owner-only, must be a regular file, and is never removed;
/// the caller opens it as the lock database (senpi `socketSecretLockPath`).
pub fn socket_secret_lock_path(path: &Path) -> io::Result<PathBuf> {
    let parent = without_verbatim_prefix(std::fs::canonicalize(parent_of(path))?);
    let lock = parent.join(format!("{}.publish.lock", file_name_of(path)));
    let mut options = OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&lock)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "RPC socket secret lock is not a regular file: {}",
                lock.display()
            ),
        ));
    }
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbatim_drive_prefix_is_dropped_and_other_paths_are_kept() {
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\C:\agent\rpc")),
            PathBuf::from(r"C:\agent\rpc")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\rpc")),
            PathBuf::from(r"\\?\UNC\server\share\rpc")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from("/agent/rpc")),
            PathBuf::from("/agent/rpc")
        );
    }

    #[test]
    fn a_bare_file_name_lives_in_the_working_directory() {
        assert_eq!(parent_of(Path::new("rpc.sock.secret")), Path::new("."));
        assert_eq!(
            parent_of(Path::new("dir/rpc.sock.secret")),
            Path::new("dir")
        );
        assert_eq!(
            file_name_of(Path::new("dir/rpc.sock.secret")),
            "rpc.sock.secret"
        );
    }

    #[test]
    fn only_a_secret_of_exactly_the_valid_length_is_valid() {
        assert!(valid_secret(&[1u8; SOCKET_SECRET_BYTES]).is_some());
        assert!(valid_secret(&[1u8; SOCKET_SECRET_BYTES - 1]).is_none());
        assert!(valid_secret(&[1u8; SOCKET_SECRET_BYTES + 1]).is_none());
        assert!(valid_secret(&[]).is_none());
    }
}
