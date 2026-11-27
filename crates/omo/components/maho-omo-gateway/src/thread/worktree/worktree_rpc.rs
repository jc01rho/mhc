//! The `extension_request` another device's `thread_create` sends this device's omo to create (or undo) a worktree
//! here (omo `thread/worktree/worktree-rpc.ts`, `worktree-rpc-name.ts`).

use std::path::Path;

use serde_json::{Value, json};

use super::discard_worktree::{DiscardWorktreeOptions, discard_worktree, inside_root};
use super::failures::folder_failure;
use super::thread_worktree::{GitRun, WorktreeRequest, create_thread_worktree, run_git};
use crate::thread::errors::ThreadToolFailure;

pub const THREAD_WORKTREE_RPC: &str = "omo.thread.worktree";

fn not_absolute(path: Option<&str>) -> Value {
    let details = path.map(|p| json!({ "path": p }));
    let failure = folder_failure(
        "relative_path",
        "The worktree request must name an absolute folder of this device.",
        details,
    );
    json!({ "kind": "error", "error": failure })
}

fn error_value(failure: ThreadToolFailure) -> Value {
    json!({ "kind": "error", "error": failure })
}

fn is_linked_worktree_top(path: &str, git: &GitRun) -> bool {
    let Ok(real) = std::fs::canonicalize(path) else {
        return false;
    };
    let top = git(&["rev-parse", "--show-toplevel"], path);
    if top.exit != Some(0) {
        return false;
    }
    let Ok(top_real) = std::fs::canonicalize(top.stdout.trim()) else {
        return false;
    };
    if top_real != real {
        return false;
    }
    let list = git(&["worktree", "list", "--porcelain"], path);
    if list.exit != Some(0) {
        return false;
    }
    list.stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .skip(1)
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .any(|p| p == real)
}

fn remove_requested(record: &Value, root: &str, git: &GitRun) -> Value {
    let path = record
        .get("path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let branch = record
        .get("branch")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let (Some(path), Some(branch)) = (path, branch) else {
        return not_absolute(path);
    };
    if !Path::new(path).is_absolute() {
        return not_absolute(Some(path));
    }
    if !inside_root(root, path) {
        return error_value(folder_failure(
            "worktree_left_behind",
            format!("{path} is not a worktree OmO made here; it was not removed."),
            Some(json!({ "path": path, "branch": branch })),
        ));
    }
    if !is_linked_worktree_top(path, git) {
        return error_value(folder_failure(
            "worktree_left_behind",
            format!("{path} is not the top folder of a worktree; it was not removed."),
            Some(json!({ "path": path, "branch": branch })),
        ));
    }
    let head = git(&["symbolic-ref", "--short", "HEAD"], path);
    let common = git(
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        path,
    );
    if head.exit != Some(0) || head.stdout.trim() != branch || common.exit != Some(0) {
        return error_value(folder_failure(
            "worktree_left_behind",
            format!("{path} is not the worktree of branch {branch}; it was not removed."),
            Some(json!({ "path": path, "branch": branch })),
        ));
    }
    let status = git(
        &[
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=all",
        ],
        path,
    );
    if status.exit != Some(0) || !status.stdout.trim().is_empty() {
        return error_value(folder_failure(
            "worktree_left_behind",
            format!("{path} has uncommitted work; it was not removed."),
            Some(json!({ "path": path, "branch": branch })),
        ));
    }
    let own = git(
        &[
            "reflog",
            "show",
            "--format=%H",
            &format!("refs/heads/{branch}"),
        ],
        path,
    );
    let own_commits: Option<usize> = (own.exit == Some(0)).then(|| {
        own.stdout
            .trim()
            .lines()
            .filter(|line| !line.is_empty())
            .count()
    });
    if own_commits != Some(1) {
        let why = match own_commits {
            None => format!(
                "the reflog of branch {branch} could not be read, so its work cannot be ruled out"
            ),
            Some(0) => format!(
                "branch {branch} has no reflog entry (reflogs may be off in this repository), so its work cannot be ruled out"
            ),
            Some(_) => format!("branch {branch} has commits of its own"),
        };
        return error_value(folder_failure(
            "worktree_left_behind",
            format!("{path} was not removed: {why}."),
            Some(json!({ "path": path, "branch": branch })),
        ));
    }
    let common_dir = common.stdout.trim();
    let common_path = Path::new(common_dir);
    let repository = if common_path.file_name().and_then(|n| n.to_str()) == Some(".git") {
        common_path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| common_dir.to_owned())
    } else {
        common_dir.to_owned()
    };
    match discard_worktree(DiscardWorktreeOptions {
        repository: &repository,
        path,
        branch,
        root,
        git: Some(git),
        created_folder: None,
        remove_unregistered_folder: false,
    }) {
        Some(failure) => error_value(failure),
        None => json!({ "kind": "removed" }),
    }
}

fn optional_non_empty(record: &Value, key: &str) -> Result<Option<String>, ()> {
    match record.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        _ => Err(()),
    }
}

/// Answers one worktree `extension_request` (`omo.thread.worktree`) on the target device.
pub fn answer_worktree_request(data: &Value, root: &str, custom_git: Option<&GitRun>) -> Value {
    let default_git: GitRun = Box::new(run_git);
    let git = custom_git.unwrap_or(&default_git);
    if let Some(remove) = data.get("remove") {
        return remove_requested(remove, root, git);
    }
    let cwd = data
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let branch = optional_non_empty(data, "branch");
    let base = optional_non_empty(data, "base");
    let (Some(cwd), Ok(branch), Ok(base)) = (cwd, branch, base) else {
        return not_absolute(data.get("cwd").and_then(Value::as_str));
    };
    if !Path::new(cwd).is_absolute() {
        return not_absolute(Some(cwd));
    }
    match create_thread_worktree(cwd, &WorktreeRequest { branch, base }, root, Some(git)) {
        Ok(worktree) => json!({ "kind": "ok", "worktree": worktree }),
        Err(failure) => error_value(*failure),
    }
}
