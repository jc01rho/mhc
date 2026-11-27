//! The folder a new thread is created in, settled before anything opens.
//!
//! Upstream counterpart: `thread/remote/create-folder.ts` (`placeFolder`). A relative `cwd` (and `~`) resolves
//! against the caller's folder on this device, and is refused on another device, whose folders this device cannot see;
//! a worktree needs a repository folder, so it is refused when none is known. An open of an existing thread is left as it is.

use std::path::{Component, Path, PathBuf};

use serde_json::json;

use crate::thread::worktree::failures::folder_failure;
use crate::thread::worktree::thread_worktree::WorktreeRequest;

use super::operations::error_json;
use super::plan_create::CreatePlan;

fn home_directory() -> Option<String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| home.to_string_lossy().into_owned())
}

fn absolute_on_any_device(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("\\")
        || (path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && matches!(path.as_bytes()[2], b'/' | b'\\'))
}

/// `path.resolve(base, relative)`: joined, then `.` and `..` folded away; an empty `base` is the process folder.
pub fn resolve_path(base: &str, relative: &str) -> String {
    let joined = if base.is_empty() {
        std::env::current_dir().unwrap_or_default().join(relative)
    } else {
        Path::new(base).join(relative)
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
    normal.to_string_lossy().into_owned()
}

pub fn place_folder(
    plan: CreatePlan,
    worktree: Option<&WorktreeRequest>,
    caller_cwd: Option<&str>,
) -> CreatePlan {
    let (mut request, previous_name, reopened_id, local_alias) = match plan {
        CreatePlan::Open {
            request,
            creates: true,
            previous_name,
            reopened_id,
            local_alias,
        } => (request, previous_name, reopened_id, local_alias),
        other => return other,
    };
    let here = request.remote.is_none();
    if let Some(asked) = request.cwd.clone() {
        if here && !Path::new(&asked).is_absolute() {
            let home = match asked.strip_prefix('~') {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                    home_directory().map(|home| format!("{home}{rest}"))
                }
                _ => None,
            };
            if home.is_none() && caller_cwd.is_none() {
                return CreatePlan::done(error_json(folder_failure(
                    "relative_path",
                    format!("cwd {asked} is relative and the caller's folder is unknown."),
                    Some(json!({ "path": asked })),
                )));
            }
            let resolved = home.unwrap_or_else(|| resolve_path(caller_cwd.unwrap_or(""), &asked));
            request.cwd = Some(resolved);
        } else if !here && !absolute_on_any_device(&asked) {
            let device = request.remote.as_deref().unwrap_or_default();
            return CreatePlan::done(error_json(folder_failure(
                "relative_path",
                format!("cwd {asked} must be an absolute folder of device {device}."),
                Some(json!({ "path": asked, "device": device })),
            )));
        }
    }
    if worktree.is_some() && request.cwd.is_none() {
        let (message, details) = match request.remote.as_deref() {
            None => (
                "A worktree needs the repository folder and the caller's folder is unknown; pass cwd.".to_owned(),
                None,
            ),
            Some(device) => (
                format!("A worktree on device {device} needs its repository folder there; pass cwd."),
                Some(json!({ "device": device })),
            ),
        };
        return CreatePlan::done(error_json(folder_failure(
            "relative_path",
            message,
            details,
        )));
    }
    CreatePlan::Open {
        request,
        creates: true,
        previous_name,
        reopened_id,
        local_alias,
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan_create::OpenFields;
    use super::*;

    fn creating(cwd: Option<&str>) -> CreatePlan {
        CreatePlan::Open {
            request: OpenFields {
                cwd: cwd.map(str::to_owned),
                ..OpenFields::default()
            },
            creates: true,
            previous_name: None,
            reopened_id: None,
            local_alias: None,
        }
    }

    fn folder_of(plan: &CreatePlan) -> Option<&str> {
        match plan {
            CreatePlan::Open { request, .. } => request.cwd.as_deref(),
            CreatePlan::Done { .. } => None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_relative_folder_resolves_against_the_callers_folder() {
        assert_eq!(resolve_path("/work/api", "../web/./src"), "/work/web/src");
        assert_eq!(resolve_path("/work/api", "docs"), "/work/api/docs");
        let placed = place_folder(creating(Some("../web")), None, Some("/work/api"));
        assert_eq!(folder_of(&placed), Some("/work/web"));
    }

    #[cfg(unix)]
    #[test]
    fn an_absolute_folder_is_left_as_it_is() {
        let placed = place_folder(creating(Some("/srv/app")), None, Some("/work/api"));
        assert_eq!(folder_of(&placed), Some("/srv/app"));
        let unknown_caller = place_folder(creating(Some("/srv/app")), None, None);
        assert_eq!(folder_of(&unknown_caller), Some("/srv/app"));
    }

    #[test]
    fn a_relative_folder_with_no_known_caller_folder_is_refused() {
        let CreatePlan::Done { result, thread } = place_folder(creating(Some("docs")), None, None)
        else {
            panic!("expected a refusal");
        };
        assert!(thread.is_none());
        assert_eq!(result["kind"], "error");
        assert_eq!(result["error"]["code"], "invalid_arguments");
        assert_eq!(result["error"]["details"]["reason"], "relative_path");
        assert_eq!(result["error"]["details"]["path"], "docs");
    }

    #[test]
    fn a_worktree_needs_a_repository_folder() {
        let request = WorktreeRequest::default();
        let CreatePlan::Done { result, .. } = place_folder(creating(None), Some(&request), None)
        else {
            panic!("expected a refusal");
        };
        assert_eq!(result["error"]["details"]["reason"], "relative_path");
    }

    #[test]
    fn opening_an_existing_thread_leaves_its_folder_alone() {
        let reopening = CreatePlan::Open {
            request: OpenFields {
                cwd: Some("relative".into()),
                session_path: Some("/sessions/a.jsonl".into()),
                fork_from: None,
                remote: None,
                remote_session: None,
            },
            creates: false,
            previous_name: Some("kept".into()),
            reopened_id: None,
            local_alias: None,
        };
        let placed = place_folder(reopening, None, None);
        assert_eq!(folder_of(&placed), Some("relative"));
    }
}
