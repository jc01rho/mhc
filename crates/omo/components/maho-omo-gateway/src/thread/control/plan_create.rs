//! Where a thread_create lands, on this device.
//!
//! Upstream counterpart: `thread/remote/plan-create.ts` (`planCreate`, its `remote === undefined` branches), the folder and
//! fork steps of `remote/cross-device.ts` `planCreate`, and `existingHere` of `control/create.ts`. An id opens that session
//! (answered without an open when it is live already); no id creates here, a fork is refused until the engine forks on
//! open (#13031). Placement is the only step that reads the device; this device has no account, so a `device` is refused
//! `host_unavailable` and an alias is only the thread's local name.

use std::path::Path;

use serde_json::{Value, json};

use crate::thread::address::{
    ResolveContext, ResolveOptions as AddressResolveOptions, resolve_address,
};
use crate::thread::address_book::{AddressEntry, DiskSession, find_disk_sessions};
use crate::thread::addressing::{
    ResolveOptions, ThreadAddressEntry, resolve_target, to_thread_address_entries,
    workspace_directories,
};
use crate::thread::contracts_create_params::ThreadCreateInput;
use crate::thread::contracts_results::ThreadResultSummary;
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::thread_row::opened_result;

use super::create_folder::place_folder;
use super::locate::{ThreadCall, book_of};
use super::operations::error_json;
use super::port::SessionOpenRequest;
use super::session_host::session_summary;

/// The fields of an open that placement decides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenFields {
    pub cwd: Option<String>,
    pub session_path: Option<String>,
    pub fork_from: Option<String>,
}

/// What this device answers for an existing thread named to thread_create: it is live here, or its session file is here,
/// or neither. `name` is the name the thread had, so a reopen keeps it (#14274); `cwd` is the folder its session records,
/// which the reopen names so the engine does not probe the file's path on its loop (#14296).
#[derive(Debug)]
pub enum LocalExisting {
    Live(Box<ThreadResultSummary>),
    File {
        session_path: String,
        name: Option<String>,
        cwd: Option<String>,
    },
    Error(Box<ThreadToolFailure>),
}

/// `Open`: the host opens `request` (`creates` is false for an existing thread's file); `Done`: a refusal, or an existing
/// thread that is live already and needs no open (`deduplicated`), with the thread a create with a prompt sends it to
/// (#14335).
#[derive(Debug)]
pub enum CreatePlan {
    Open {
        request: OpenFields,
        creates: bool,
        previous_name: Option<String>,
        local_alias: Option<&'static str>,
    },
    Done {
        result: Value,
        thread: Option<Box<ThreadResultSummary>>,
    },
}

impl CreatePlan {
    pub fn done(result: Value) -> Self {
        Self::Done {
            result,
            thread: None,
        }
    }
}

fn invalid(message: &str, next_action: &str) -> CreatePlan {
    CreatePlan::done(error_json(thread_tool_failure(
        "invalid_arguments",
        message,
        next_action,
        None,
    )))
}

fn already_live(thread: Box<ThreadResultSummary>) -> CreatePlan {
    CreatePlan::Done {
        result: opened_result(&thread, true),
        thread: Some(thread),
    }
}

/// The address book a send resolves `address` against: this device's entries, plus (only when the address names none of
/// them) the session files on disk it names, so a thread whose process exited is still found by id or name.
fn send_address_book(call: &ThreadCall, address: &str, all_scope: bool) -> Vec<AddressEntry> {
    let Some(directory) = call.sessions_directory.as_ref() else {
        return call.book.clone();
    };
    if address == "self" || call.book.iter().any(|entry| entry.durable_id == address) {
        return call.book.clone();
    }
    let entries = to_thread_address_entries(&call.book);
    let resolved = resolve_target(
        &entries,
        address,
        &ResolveOptions {
            all_scope,
            caller_workspace_root: &call.caller_workspace_root,
            ..ResolveOptions::default()
        },
    );
    let id_only = !matches!(&resolved, Err(failure) if failure.code.as_str() == "not_found");
    let roots = workspace_directories(&call.caller_workspace_root);
    let found: Vec<DiskSession> = find_disk_sessions(directory, address, all_scope, &roots)
        .into_iter()
        .filter(|session| !id_only || session.durable_id == address)
        .filter(|session| {
            !call
                .book
                .iter()
                .any(|entry| entry.durable_id == session.durable_id)
        })
        .collect();
    if found.is_empty() {
        return call.book.clone();
    }
    book_of(&call.view, &found, &call.now)
}

/// An existing thread this device is asked to open: live in the view, a session file the address book names, or neither.
/// Resolved among this device's book only (no account): placement already chose this device.
pub async fn existing_here(call: &ThreadCall, address: &str, caller_id: &str) -> LocalExisting {
    let local_for = |part: &str| -> Vec<ThreadAddressEntry> {
        to_thread_address_entries(&send_address_book(call, part, true))
    };
    let local = local_for(address);
    let context = ResolveContext {
        caller_workspace_root: &call.caller_workspace_root,
        local: &local,
        local_for: Some(&local_for),
        caller_id,
    };
    let options = AddressResolveOptions {
        all_scope: true,
        ..AddressResolveOptions::default()
    };
    let resolved = match resolve_address(&context, address, &options) {
        Ok(resolved) => resolved,
        Err(refusal) => return LocalExisting::Error(refusal),
    };
    if let Some(session) = call.view.target_session(&resolved.thread) {
        return LocalExisting::Live(Box::new(session_summary(session, &call.now)));
    }
    let book = send_address_book(call, &resolved.thread, true);
    let found = book
        .iter()
        .find(|candidate| candidate.thread_id == resolved.thread)
        .and_then(|entry| {
            entry
                .session_path
                .clone()
                .map(|session_path| (entry, session_path))
        });
    let Some((entry, session_path)) = found else {
        return LocalExisting::Error(Box::new(thread_tool_failure(
            "not_resumable",
            "The thread has no session file on this device to open.",
            "Call thread_list (tool.thread_list() in an eval cell) and pick a thread it lists.",
            None,
        )));
    };
    let name = entry
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let cwd = Some(entry.cwd.clone()).filter(|cwd| Path::new(cwd).is_absolute());
    LocalExisting::File {
        session_path,
        name,
        cwd,
    }
}

/// The open of a session file here: it names the folder the file records, so the engine resumes it there without probing
/// the file's path on its loop, a probe that fails open_failed on a loaded host (#14296).
pub fn file_open(session_path: &str, cwd: Option<&str>) -> SessionOpenRequest {
    SessionOpenRequest {
        session_path: Some(session_path.to_owned()),
        cwd: cwd.map(str::to_owned),
        ..SessionOpenRequest::default()
    }
}

async fn open_here(call: &ThreadCall, address: &str, caller_id: &str) -> CreatePlan {
    match existing_here(call, address, caller_id).await {
        LocalExisting::Error(failure) => CreatePlan::done(error_json(*failure)),
        LocalExisting::Live(thread) => already_live(thread),
        LocalExisting::File {
            session_path,
            name,
            cwd,
        } => CreatePlan::Open {
            request: OpenFields {
                cwd,
                session_path: Some(session_path),
                fork_from: None,
            },
            creates: false,
            previous_name: name,
            local_alias: None,
        },
    }
}

/// thread_create under A3: an id or alias OPENS that session wherever it is (never with a device) - answered without an
/// open when it is live there already; no id creates on this device; a fork is created on its source's device.
pub async fn plan_create(
    call: &ThreadCall,
    value: &ThreadCreateInput,
    caller_id: &str,
) -> CreatePlan {
    if value.worktree.is_some() && value.thread.is_some() {
        return invalid(
            "worktree creates a new thread's folder; thread opens an existing thread.",
            "Drop worktree to open the thread, or drop thread to create a new one in a worktree.",
        );
    }
    if value.thread.is_some() && value.device.is_some() {
        return invalid(
            "An existing thread is named by its id or alias only, never with a device.",
            "Drop device; the thread's device is found from the account's session list.",
        );
    }
    if value.thread.is_some() && value.fork_from.is_some() {
        return invalid(
            "thread opens an existing thread; fork_from creates a new one. Pass one of them.",
            "Pass thread to open it, or fork_from to fork it.",
        );
    }
    if value.device.is_some() || (value.alias.is_some() && value.thread.is_some()) {
        return CreatePlan::done(error_json(thread_tool_failure(
            "host_unavailable",
            "Aliases and other devices need this device signed in to the account's device service.",
            "Sign in, or create the thread on this device without an alias.",
            None,
        )));
    }
    let caller_cwd: Option<&str> =
        Some(call.caller_workspace_root.as_str()).filter(|root| !root.trim().is_empty());
    let plan = if let Some(thread) = value.thread.as_deref() {
        open_here(call, thread, caller_id).await
    } else {
        let cwd = if value.fork_from.is_some() {
            value.cwd.clone()
        } else {
            value.cwd.clone().or_else(|| caller_cwd.map(str::to_owned))
        };
        CreatePlan::Open {
            request: OpenFields {
                cwd,
                session_path: None,
                fork_from: value.fork_from.clone(),
            },
            creates: true,
            previous_name: None,
            local_alias: value.alias.as_ref().map(|_| "no_remote"),
        }
    };
    let plan = place_folder(plan, value.worktree.as_ref(), caller_cwd);
    let (fork_from, creates) = match &plan {
        CreatePlan::Open {
            request, creates, ..
        } => (request.fork_from.clone(), *creates),
        CreatePlan::Done { .. } => (None, false),
    };
    if let Some(source) = fork_from {
        return CreatePlan::done(error_json(thread_tool_failure(
            "unsupported",
            "fork_from cannot fork yet: the engine opens a blank session instead of a fork, so no thread was created.",
            "Create the thread without fork_from and hand it the source's context with thread_send; thread_read reads the source (tool.thread_send() and tool.thread_read() in an eval cell).",
            Some(json!({ "reason": "fork_unsupported", "thread": source })),
        )));
    }
    if creates && value.worktree.is_some() {
        return CreatePlan::done(error_json(thread_tool_failure(
            "unsupported",
            "worktree cannot be created yet: this build does not make git worktrees for a new thread, so no thread was created.",
            "Create the thread in an existing folder (pass cwd) and make the worktree yourself, or drop worktree.",
            Some(json!({ "reason": "worktree_unsupported" })),
        )));
    }
    plan
}
