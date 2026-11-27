//! Undoes a worktree THIS call created (omo `thread/worktree/discard-worktree.ts`, #13450 review).

use std::path::{Component, Path, PathBuf};

use serde_json::json;

use super::failures::folder_failure;
use super::thread_worktree::{GitRun, run_git};
use crate::thread::errors::ThreadToolFailure;

fn normalize_path(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut normal = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other.as_os_str()),
        }
    }
    normal
}

fn is_strictly_inside(root: &Path, target: &Path) -> bool {
    target != root && target.starts_with(root)
}

/// Whether `path` lies in the worktrees root, by spelling and, once it exists, by realpath (no symlink escapes).
pub fn inside_root(root: &str, path: &str) -> bool {
    let path_ref = Path::new(path);
    if !path_ref.is_absolute() {
        return false;
    }
    let norm_root = normalize_path(Path::new(root));
    let norm_path = normalize_path(path_ref);
    if !is_strictly_inside(&norm_root, &norm_path) {
        return false;
    }
    if !path_ref.exists() {
        return true;
    }
    let real_root = std::fs::canonicalize(root).unwrap_or(norm_root);
    let Ok(real_path) = std::fs::canonicalize(path) else {
        return false;
    };
    is_strictly_inside(&real_root, &real_path)
}

pub struct DiscardWorktreeOptions<'a> {
    pub repository: &'a str,
    pub path: &'a str,
    pub branch: &'a str,
    pub root: &'a str,
    pub git: Option<&'a GitRun>,
    pub created_folder: Option<&'a str>,
    pub remove_unregistered_folder: bool,
}

/// Undoes a worktree this call created: `git worktree remove --force`, the folder only when git does not know it as a
/// worktree and it lies in the worktrees root, then the branch, then the per-repository folder when empty.
pub fn discard_worktree(options: DiscardWorktreeOptions<'_>) -> Option<ThreadToolFailure> {
    let default_git: GitRun = Box::new(run_git);
    let git = options.git.unwrap_or(&default_git);
    let DiscardWorktreeOptions {
        repository,
        path,
        branch,
        root,
        created_folder,
        remove_unregistered_folder,
        ..
    } = options;
    if !inside_root(root, path) {
        return Some(folder_failure(
            "worktree_left_behind",
            format!("{path} is outside OmO's worktrees folder; it was not removed."),
            Some(json!({ "path": path, "branch": branch })),
        ));
    }
    let canonical = |spelled: &str| -> PathBuf {
        std::fs::canonicalize(spelled).unwrap_or_else(|_| normalize_path(Path::new(spelled)))
    };
    let registered = || -> bool {
        let list = git(&["worktree", "list", "--porcelain"], repository);
        if list.exit != Some(0) {
            return true;
        }
        let target = canonical(path);
        list.stdout
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(canonical)
            .any(|entry| entry == target)
    };
    if Path::new(path).exists() {
        let _ = git(&["worktree", "remove", "--force", path], repository);
    }
    if remove_unregistered_folder && Path::new(path).exists() && !registered() {
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = git(&["worktree", "prune"], repository);
    let has_branch = || -> bool {
        git(
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            repository,
        )
        .exit
            == Some(0)
    };
    if has_branch() && !registered() {
        let _ = git(&["branch", "-D", branch], repository);
    }
    if let Some(folder) = created_folder {
        let _ = std::fs::remove_dir(folder);
    }
    let left_folder = Path::new(path).exists();
    let left_branch = has_branch();
    if !left_folder && !left_branch {
        return None;
    }
    Some(folder_failure(
        "worktree_left_behind",
        "The worktree this call created could not be fully removed.",
        Some(json!({
            "path": path,
            "branch": branch,
            "repository": repository,
            "left": {
                "folder": left_folder,
                "branch": left_branch,
            },
        })),
    ))
}
