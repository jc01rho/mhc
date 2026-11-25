//! The worktree request and the folder check of `thread_create`.
//!
//! Upstream counterpart: `thread/worktree/thread-worktree.ts`. Only `WorktreeRequest` and `checkFolder` are ported;
//! creating the worktree (`createThreadWorktree`) is not.

use serde_json::json;

use super::failures::{folder_failure, io_reason};
use crate::thread::errors::ThreadToolFailure;

/// `worktree` of a create: the branch the new worktree checks out and the commit it starts at (default: HEAD).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeRequest {
    pub branch: Option<String>,
    pub base: Option<String>,
}

/// The failure of using `path` as a new thread's folder: it names no folder, or none this user can read.
pub fn check_folder(path: &str) -> Option<ThreadToolFailure> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => None,
        Ok(_) => Some(folder_failure(
            "not_a_directory",
            format!("{path} is not a folder."),
            Some(json!({ "path": path })),
        )),
        Err(error) => {
            let reason = io_reason(&error).unwrap_or("folder_missing");
            Some(folder_failure(
                reason,
                format!("Folder {path} cannot be used ({reason})."),
                Some(json!({ "path": path })),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_passes_and_a_file_or_a_missing_path_is_refused_with_its_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let folder = dir.path().to_string_lossy().into_owned();
        assert!(check_folder(&folder).is_none());

        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").expect("write");
        let not_a_folder = check_folder(&file.to_string_lossy()).expect("refused");
        assert_eq!(not_a_folder.code.as_str(), "invalid_arguments");
        assert_eq!(
            not_a_folder.details.expect("details")["reason"],
            "not_a_directory"
        );

        let gone = dir.path().join("gone");
        let missing = check_folder(&gone.to_string_lossy()).expect("refused");
        let details = missing.details.expect("details");
        let gone_text = gone.to_string_lossy().into_owned();
        assert_eq!(details["reason"], "folder_missing");
        assert_eq!(details["path"], gone_text);
    }
}
