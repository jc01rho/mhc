//! The worktree request, the folder check and worktree creation of `thread_create`.
//!
//! Upstream counterpart: `thread/worktree/thread-worktree.ts`.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::discard_worktree::{DiscardWorktreeOptions, discard_worktree};
use super::failures::{folder_failure, git_failure_reason, io_reason};
use crate::thread::errors::ThreadToolFailure;

/// `worktree` of a create: the branch the new worktree checks out and the commit it starts at (default: HEAD).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// A git worktree created for a thread on this device or another.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadWorktree {
    pub path: String,
    pub branch: String,
    pub base: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_session_left_open: Option<String>,
}

pub type WorktreeOutcome = Result<ThreadWorktree, Box<ThreadToolFailure>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitResult {
    /// `Some(code)` when git exited, `None` when spawning failed.
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

pub type GitRun = Box<dyn Fn(&[&str], &str) -> GitResult + Send + Sync>;

pub fn run_git(args: &[&str], cwd: &str) -> GitResult {
    match Command::new("git").args(args).current_dir(cwd).output() {
        Ok(output) => GitResult {
            exit: Some(output.status.code().unwrap_or(1)),
            timed_out: false,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(error) => GitResult {
            exit: None,
            timed_out: false,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    }
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

fn generated_branch() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("omo/thread-{}", &id[..12])
}

fn failed_git(result: &GitResult, details: serde_json::Value) -> ThreadToolFailure {
    if result.exit.is_none() {
        return folder_failure(
            "git_unavailable",
            "git is not installed here.",
            Some(details),
        );
    }
    if result.timed_out {
        return folder_failure("git_timeout", "git did not answer in time.", Some(details));
    }
    let reason = git_failure_reason(&result.stderr);
    let mut map = details.as_object().cloned().unwrap_or_default();
    map.insert("stderr".to_owned(), json!(result.stderr.trim()));
    folder_failure(
        reason,
        format!("git refused the worktree ({reason})."),
        Some(serde_json::Value::Object(map)),
    )
}

/// Creates the git worktree a new thread starts in: a new `branch` at `base` (default HEAD) of the repository holding
/// `repository`, in `<root>/<repo name>-<hash of its path>/<branch>`.
pub fn create_thread_worktree(
    repository: &str,
    request: &WorktreeRequest,
    root: &str,
    custom_git: Option<&GitRun>,
) -> WorktreeOutcome {
    let default_git: GitRun = Box::new(run_git);
    let git = custom_git.unwrap_or(&default_git);
    if let Some(failure) = check_folder(repository) {
        return Err(Box::new(failure));
    }
    let top = git(&["rev-parse", "--show-toplevel"], repository);
    if top.exit != Some(0) {
        return Err(Box::new(failed_git(&top, json!({ "path": repository }))));
    }
    let repo_root = top.stdout.trim().to_owned();
    let branch = request.branch.clone().unwrap_or_else(generated_branch);
    let base = request.base.as_deref().unwrap_or("HEAD");
    if branch.starts_with('-')
        || git(&["check-ref-format", "--branch", &branch], &repo_root).exit != Some(0)
    {
        return Err(Box::new(folder_failure(
            "invalid_branch",
            format!("{branch} is not a valid branch name."),
            Some(json!({ "branch": branch })),
        )));
    }
    let commit = if base.starts_with('-') {
        None
    } else {
        Some(git(
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{base}^{{commit}}"),
            ],
            &repo_root,
        ))
    };
    let Some(commit) = commit.filter(|res| res.exit == Some(0)) else {
        return Err(Box::new(folder_failure(
            "base_not_found",
            format!("{base} names no commit in {repo_root}."),
            Some(json!({ "base": base, "repository": repo_root })),
        )));
    };
    if git(
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        &repo_root,
    )
    .exit
        == Some(0)
    {
        return Err(Box::new(folder_failure(
            "branch_exists",
            format!("Branch {branch} already exists in {repo_root}."),
            Some(json!({ "branch": branch, "repository": repo_root })),
        )));
    }
    let mut hasher = Sha256::new();
    hasher.update(repo_root.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let repo_name = Path::new(&repo_root)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo");
    let repo_key = format!("{repo_name}-{}", &digest[..8]);
    let target_path = Path::new(root)
        .join(&repo_key)
        .join(branch.replace('/', "-"));
    let target_str = target_path.to_string_lossy().into_owned();
    if target_path.exists() {
        return Err(Box::new(folder_failure(
            "worktree_path_exists",
            format!("{target_str} already exists."),
            Some(json!({ "path": target_str, "branch": branch })),
        )));
    }
    let parent = target_path
        .parent()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent_existed = Path::new(&parent).exists();
    if let Err(error) = std::fs::create_dir_all(&parent) {
        let reason = io_reason(&error).unwrap_or("permission_denied");
        return Err(Box::new(folder_failure(
            reason,
            format!("The worktrees folder {parent} cannot be created ({reason})."),
            Some(json!({ "path": target_str })),
        )));
    }
    let commit_sha = commit.stdout.trim().to_owned();
    let added = git(
        &["worktree", "add", "-b", &branch, &target_str, &commit_sha],
        &repo_root,
    );
    if added.exit != Some(0) {
        let left = discard_worktree(DiscardWorktreeOptions {
            repository: &repo_root,
            path: &target_str,
            branch: &branch,
            root,
            git: Some(git),
            created_folder: (!parent_existed).then_some(parent.as_str()),
            remove_unregistered_folder: true,
        });
        return Err(Box::new(left.unwrap_or_else(|| {
            failed_git(
                &added,
                json!({ "path": target_str, "branch": branch, "repository": repo_root }),
            )
        })));
    }
    Ok(ThreadWorktree {
        path: target_str,
        branch,
        base: commit_sha,
        repository: repo_root,
        setup_session_left_open: None,
    })
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
