//! Every way placing a new thread's folder can fail.
//!
//! Upstream counterpart: `thread/worktree/failures.ts`. Each reason maps to one existing failure code and one next
//! action the calling agent can take; `details.reason` names it. Cwd checks and worktree creation, on either device,
//! answer through here.

use serde_json::{Map, Value};

use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};

/// `FolderFailureReason`, in upstream order.
pub const FOLDER_FAILURE_REASONS: [&str; 15] = [
    "folder_missing",
    "not_a_directory",
    "relative_path",
    "not_a_git_repo",
    "invalid_branch",
    "base_not_found",
    "branch_exists",
    "branch_checked_out",
    "worktree_path_exists",
    "no_space",
    "permission_denied",
    "git_unavailable",
    "git_timeout",
    "git_failed",
    "worktree_left_behind",
];

/// The failure code and next action of one reason; a reason nothing names answers as `git_failed`.
fn table(reason: &str) -> (&'static str, &'static str) {
    match reason {
        "folder_missing" => (
            "invalid_arguments",
            "Pass an existing folder as cwd; nothing was created.",
        ),
        "not_a_directory" => (
            "invalid_arguments",
            "Pass a folder, not a file, as cwd; nothing was created.",
        ),
        "relative_path" => (
            "invalid_arguments",
            "Pass an absolute folder; on another device use a path of that device (thread_list with all_scope, tool.thread_list({ all_scope: true }) in an eval cell, shows its folders).",
        ),
        "not_a_git_repo" => (
            "invalid_arguments",
            "Pass a folder inside a git repository as cwd, or drop worktree to start the thread in that folder.",
        ),
        "invalid_branch" => (
            "invalid_arguments",
            "Pass a valid git branch name as worktree.branch.",
        ),
        "base_not_found" => (
            "invalid_arguments",
            "Pass an existing commit, branch or tag as worktree.base, or drop it to start at HEAD.",
        ),
        "branch_exists" => (
            "name_conflict",
            "Pass a new branch name as worktree.branch, or drop it to get a generated one.",
        ),
        "branch_checked_out" => (
            "name_conflict",
            "That branch is checked out in another worktree; pass a new branch name as worktree.branch.",
        ),
        "worktree_path_exists" => (
            "name_conflict",
            "A folder already exists where the worktree would go; pass another worktree.branch.",
        ),
        "no_space" => (
            "host_unavailable",
            "Free disk space on that device, then retry; nothing was created.",
        ),
        "permission_denied" => (
            "scope_denied",
            "The device user cannot write there; fix the folder's permissions or pick another cwd, then retry.",
        ),
        "git_unavailable" => (
            "unsupported",
            "Install git on that device, or drop worktree.",
        ),
        "git_timeout" => (
            "host_unavailable",
            "git did not answer in time on that device; retry once it is idle.",
        ),
        "worktree_left_behind" => (
            "partial_commit",
            "Remove what details.left names: git worktree remove --force <details.path> and git branch -D <details.branch> in details.repository; then retry.",
        ),
        _ => (
            "invalid_arguments",
            "Read details.stderr, fix the repository state, then retry.",
        ),
    }
}

/// The typed failure of one reason: `details.reason` first, then whatever `details` adds.
pub fn folder_failure(
    reason: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> ThreadToolFailure {
    let (code, next_action) = table(reason);
    let mut merged = Map::new();
    merged.insert("reason".to_owned(), Value::String(reason.to_owned()));
    if let Some(Value::Object(extra)) = details {
        merged.extend(extra);
    }
    thread_tool_failure(code, message, next_action, Some(Value::Object(merged)))
}

pub fn git_failure_reason(stderr: &str) -> &'static str {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("no space left on device") {
        "no_space"
    } else if lower.contains("permission denied")
        || lower.contains("operation not permitted")
        || lower.contains("read-only file system")
    {
        "permission_denied"
    } else if lower.contains("already checked out")
        || lower.contains("is already used by worktree")
        || lower.contains("already used by worktree")
    {
        "branch_checked_out"
    } else if lower.contains("a branch named ") && lower.contains(" already exists") {
        "branch_exists"
    } else if lower.contains("already exists") {
        "worktree_path_exists"
    } else if lower.contains("not a git repository") {
        "not_a_git_repo"
    } else if lower.contains("invalid reference")
        || lower.contains("not a valid object name")
        || lower.contains("unknown revision")
    {
        "base_not_found"
    } else {
        "git_failed"
    }
}

/// `errnoReason`: the folder-failure reason for an OS filesystem error when it is one the table names.
pub fn io_reason(error: &std::io::Error) -> Option<&'static str> {
    match error.kind() {
        std::io::ErrorKind::NotFound => Some("folder_missing"),
        std::io::ErrorKind::PermissionDenied => Some("permission_denied"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn every_reason_answers_a_known_failure_code_and_names_itself() {
        for reason in FOLDER_FAILURE_REASONS {
            let failure = folder_failure(reason, "message", None);
            let details = failure.details.expect("details");
            assert_eq!(details["reason"], reason);
            assert!(!failure.next_action.is_empty(), "{reason}");
        }
    }

    #[test]
    fn a_reason_keeps_its_code_and_the_details_it_is_given() {
        let missing = folder_failure("folder_missing", "gone", Some(json!({ "path": "/gone" })));
        assert_eq!(missing.code.as_str(), "invalid_arguments");
        assert_eq!(
            missing.details,
            Some(json!({ "reason": "folder_missing", "path": "/gone" }))
        );
        let exists = folder_failure("branch_exists", "taken", None);
        assert_eq!(exists.code.as_str(), "name_conflict");
        let left = folder_failure("worktree_left_behind", "left", None);
        assert_eq!(left.code.as_str(), "partial_commit");
        let full = folder_failure("no_space", "full", None);
        assert_eq!(full.code.as_str(), "host_unavailable");
    }

    #[test]
    fn a_missing_or_forbidden_path_names_its_reason() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let other = std::io::Error::from(std::io::ErrorKind::TimedOut);
        assert_eq!(io_reason(&missing), Some("folder_missing"));
        assert_eq!(io_reason(&denied), Some("permission_denied"));
        assert_eq!(io_reason(&other), None);
    }
}
