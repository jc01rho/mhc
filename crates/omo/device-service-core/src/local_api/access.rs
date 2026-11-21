//! Port of `local-api/access.ts`: a private state dir, and a 32-byte per-boot token in
//! `<state dir>/token` that both sides prove before any request is honored.
//!
//! POSIX check, before any connection: the state dir, `service.sock` and `token` exist as themselves
//! (`lstat`: a symlink is refused), belong to this uid and have no group or world bits. Anything else
//! is `insecure_state_dir`, and nothing is created.

use std::path::Path;

use super::errors::LocalApiError;
use super::proof::decoded_bytes;

pub const TOKEN_FILE: &str = "token";
pub const TOKEN_BYTES: usize = 32;
pub const SOCKET_FILE: &str = "service.sock";
/// The endpoint both proofs are bound to on POSIX.
pub const UNIX_ENDPOINT: &str = "unix";

/// The endpoint both proofs are bound to for a Windows loopback listener.
pub fn tcp_endpoint(port: u16) -> String {
    format!("tcp:{port}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Socket,
}

impl EntryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::File => "file",
            Self::Socket => "socket",
        }
    }
}

pub fn insecure(detail: impl Into<String>) -> LocalApiError {
    LocalApiError::new("insecure_state_dir", detail)
}

fn is_missing(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// This process's uid.
#[cfg(unix)]
pub fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The entry exists as itself (a symlink fails), belongs to `uid`, and (with `check_mode`) has no
/// group or world bits.
#[cfg(unix)]
pub fn private_entry(
    path: &Path,
    kind: EntryKind,
    uid: u32,
    check_mode: bool,
) -> Result<(), LocalApiError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let label = kind.label();
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if is_missing(&error) => {
            return Err(insecure(format!("the {label} is missing")));
        }
        Err(error) => return Err(LocalApiError::new("read_failed", error.to_string())),
    };
    let file_type = metadata.file_type();
    let is_kind = match kind {
        EntryKind::Dir => file_type.is_dir(),
        EntryKind::Socket => file_type.is_socket(),
        EntryKind::File => file_type.is_file(),
    };
    if !is_kind {
        return Err(insecure(format!("the {label} is not a plain {label}")));
    }
    if metadata.uid() != uid {
        return Err(insecure(format!("the {label} belongs to another user")));
    }
    if check_mode && metadata.mode() & 0o077 != 0 {
        return Err(insecure(format!("the {label} is open to group or others")));
    }
    Ok(())
}

/// The client's check of the state dir before it connects.
#[cfg(unix)]
pub fn check_posix_access(state_dir: &Path) -> Result<(), LocalApiError> {
    let uid = current_uid();
    private_entry(state_dir, EntryKind::Dir, uid, true)?;
    private_entry(&state_dir.join(SOCKET_FILE), EntryKind::Socket, uid, true)?;
    private_entry(&state_dir.join(TOKEN_FILE), EntryKind::File, uid, true)
}

/// Checks the entry as itself and as this uid's BEFORE the chmod (chmod follows a symlink, and on
/// another user's entry it would fail with a raw EPERM), then makes it private and checks again.
#[cfg(unix)]
pub fn posix_secure_file(path: &Path, kind: EntryKind) -> Result<(), LocalApiError> {
    use std::os::unix::fs::PermissionsExt;
    let uid = current_uid();
    private_entry(path, kind, uid, false)?;
    let mode = if kind == EntryKind::Dir { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|error| {
        insecure(format!(
            "the {} cannot be made private: {error}",
            kind.label()
        ))
    })?;
    private_entry(path, kind, uid, true)
}

/// Read fresh on every connection: a restarted service has a new one.
pub fn read_token(state_dir: &Path) -> Result<Vec<u8>, LocalApiError> {
    let value = match std::fs::read_to_string(state_dir.join(TOKEN_FILE)) {
        Ok(value) => value,
        Err(error) if is_missing(&error) => return Err(insecure("the token is missing")),
        Err(error) => return Err(LocalApiError::new("read_failed", error.to_string())),
    };
    decoded_bytes(&value, TOKEN_BYTES, "the token").map_err(|_| insecure("the token is malformed"))
}

/// The secret is written only after the empty file passed the private check, then renamed into
/// place: a reader never sees a partial or unprotected secret, and a failure leaves no file behind.
#[cfg(unix)]
pub fn write_secret_file(path: &Path, content: &str) -> Result<(), LocalApiError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!("{name}.{}.tmp", super::proof::mint_id()?));
    let written = (|| -> Result<(), LocalApiError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| LocalApiError::new("internal", error.to_string()))?;
        posix_secure_file(&temp, EntryKind::File)?;
        file.write_all(content.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| LocalApiError::new("internal", error.to_string()))?;
        std::fs::rename(&temp, path)
            .map_err(|error| LocalApiError::new("internal", error.to_string()))
    })();
    if written.is_err() {
        let _cleanup = std::fs::remove_file(&temp);
    }
    written
}

/// A new per-boot token in a private state dir.
#[cfg(unix)]
pub fn write_token(state_dir: &Path) -> Result<Vec<u8>, LocalApiError> {
    use base64::Engine;
    posix_secure_file(state_dir, EntryKind::Dir)?;
    let token = super::proof::random_bytes::<TOKEN_BYTES>()?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token);
    write_secret_file(&state_dir.join(TOKEN_FILE), &encoded)?;
    Ok(token.to_vec())
}

/// The private state dir itself, created 0700 when missing.
#[cfg(unix)]
pub fn ensure_private_dir(state_dir: &Path) -> Result<(), LocalApiError> {
    use std::os::unix::fs::DirBuilderExt;
    if !state_dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state_dir)
            .map_err(|error| LocalApiError::new("internal", error.to_string()))?;
    }
    posix_secure_file(state_dir, EntryKind::Dir)
}
