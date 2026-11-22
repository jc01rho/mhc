use super::bundle::{RepositoryIdentity, parse_repository_identity};
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
};

const SCAN_LIMIT_BYTES: usize = 1024 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const CHUNK_BYTES: usize = 256 * 1024;
const MARKER: &[u8] = b"\"repository-identity\"";
const UNREADABLE_MSG: &str = "move_failed: No session file of this device is at that path, or it cannot be read; nothing was read.";

fn too_large_msg() -> String {
    format!(
        "move_failed: This session file is larger than the {SCAN_LIMIT_BYTES} bytes this read scans, so its recorded repository was not read. The move can still be sent without the check."
    )
}

fn is_session_header(line: &[u8]) -> bool {
    let Ok(entry) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    entry.get("type").and_then(Value::as_str) == Some("session")
        && entry.get("id").and_then(Value::as_str).is_some()
}

fn identity_in(line: &[u8]) -> Option<RepositoryIdentity> {
    if !line.windows(MARKER.len()).any(|window| window == MARKER) {
        return None;
    }
    let entry: Value = serde_json::from_slice(line).ok()?;
    if entry.get("type").and_then(Value::as_str) != Some("custom")
        || entry.get("customType").and_then(Value::as_str) != Some("repository-identity")
    {
        return None;
    }
    parse_repository_identity(entry.get("data")?)
}

fn session_file_inside(sessions_root: &Path, session_path: &Path) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(sessions_root).map_err(|_| UNREADABLE_MSG.to_owned())?;
    let file = std::fs::canonicalize(session_path).map_err(|_| UNREADABLE_MSG.to_owned())?;
    let is_jsonl = file
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".jsonl"));
    let parent_parent = file.parent().and_then(Path::parent);
    if !is_jsonl || parent_parent != Some(root.as_path()) {
        return Err(UNREADABLE_MSG.to_owned());
    }
    Ok(file)
}

fn open_session_file(path: &Path) -> Result<std::fs::File, String> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(path).map_err(|_| UNREADABLE_MSG.to_owned())?;
    let meta = file.metadata().map_err(|_| UNREADABLE_MSG.to_owned())?;
    if !meta.is_file() {
        return Err(UNREADABLE_MSG.to_owned());
    }
    Ok(file)
}

fn scan_file(mut file: std::fs::File) -> Result<Option<RepositoryIdentity>, String> {
    let mut chunk = vec![0u8; CHUNK_BYTES];
    let mut carry = Vec::<u8>::new();
    let mut dropping = false;
    let mut header = false;
    let mut recorded: Option<RepositoryIdentity> = None;
    let mut position = 0usize;

    let mut take = |line: &[u8]| -> Result<(), String> {
        if header {
            if let Some(found) = identity_in(line) {
                recorded = Some(found);
            }
        } else if !line.is_empty() {
            if !is_session_header(line) {
                return Err(UNREADABLE_MSG.to_owned());
            }
            header = true;
        }
        Ok(())
    };

    loop {
        let bytes_read = file
            .read(&mut chunk)
            .map_err(|_| UNREADABLE_MSG.to_owned())?;
        if bytes_read == 0 {
            break;
        }
        position = position.saturating_add(bytes_read);
        if position > SCAN_LIMIT_BYTES {
            return Err(too_large_msg());
        }
        let data = &chunk[..bytes_read];
        let mut start = 0usize;
        loop {
            if let Some(rel_nl) = data[start..].iter().position(|&b| b == b'\n') {
                let newline = start + rel_nl;
                let piece = &data[start..newline];
                start = newline + 1;
                if dropping {
                    dropping = false;
                    carry.clear();
                    if !header {
                        return Err(UNREADABLE_MSG.to_owned());
                    }
                    continue;
                }
                if carry.is_empty() {
                    take(piece)?;
                } else {
                    carry.extend_from_slice(piece);
                    take(&carry)?;
                    carry.clear();
                }
            } else {
                let rest = &data[start..];
                if dropping || carry.len() + rest.len() > MAX_LINE_BYTES {
                    carry.clear();
                    dropping = true;
                } else if !rest.is_empty() {
                    carry.extend_from_slice(rest);
                }
                break;
            }
        }
    }
    if !carry.is_empty() && !dropping {
        take(&carry)?;
    }
    if !header {
        return Err(UNREADABLE_MSG.to_owned());
    }
    Ok(recorded)
}

pub async fn read_session_repository_identity(
    sessions_root: &Path,
    session_path: &Path,
) -> Result<Option<RepositoryIdentity>, String> {
    let sessions_root = sessions_root.to_owned();
    let session_path = session_path.to_owned();
    tokio::task::spawn_blocking(move || {
        let resolved = session_file_inside(&sessions_root, &session_path)?;
        let file = open_session_file(&resolved)?;
        scan_file(file)
    })
    .await
    .map_err(|_| UNREADABLE_MSG.to_owned())?
}
