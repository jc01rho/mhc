//! Where a thread_create lands, on this device.
//!
//! Upstream counterpart: `thread/remote/plan-create.ts` (`planCreate`, its `remote === undefined` branches), the folder and
//! fork steps of `remote/cross-device.ts` `planCreate`, and `existingHere` of `control/create.ts`. An id opens that session
//! (answered without an open when it is live already); no id creates here; a fork opens on the engine that holds its
//! source, when that engine forks on open (#13031, decided in `create.rs` for this device). Placement is the only step
//! that reads the device; this device has no account, so a `device` is refused `host_unavailable` and an alias is only
//! the thread's local name.

use std::path::Path;

use maho_thread_address_core::{HERE, ParsedAddress, parse_address};
use serde_json::{Value, json};

use crate::thread::address::{
    ResolveContext, ResolveOptions as AddressResolveOptions, ResolvedWhere, ladder, resolve_address,
};
use crate::thread::addressing::{ThreadAddressEntry, to_thread_address_entries};
use crate::thread::contracts_create_params::ThreadCreateInput;
use crate::thread::contracts_results::{ThreadResultSummary, ThreadStatus};
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::remote::ports::{RemoteCommandOutcome, RemoteThreadSurface, row_status};
use crate::thread::remote::remote_ops::reply_data;
use crate::thread::thread_row::opened_result;

use super::create_folder::place_folder;
use super::locate::{RemoteControlLocator, ThreadCall, send_address_book};
use super::operations::error_json;
use super::port::SessionOpenRequest;
use super::relay_session_host::displayed_thread;
use super::session_host::{OPEN_SESSION_FORK, session_summary};

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

/// An existing thread this device is asked to open: live in the view, a session file the address book names, or neither.
/// Resolved among this device's book only (no account): placement already chose this device. This runs the resolver's
/// exact ladder the way `resolve_address` does without an account: a `here/<thread>` address asks this device's book for
/// the thread part alone.
pub async fn existing_here(call: &ThreadCall, address: &str, caller_id: &str) -> LocalExisting {
    let book_for = |part: &str| -> Vec<ThreadAddressEntry> {
        to_thread_address_entries(&send_address_book(call, part, true))
    };
    let text = match parse_address(address) {
        ParsedAddress::Device { device, thread, .. } if device == HERE => thread,
        _ => address.to_owned(),
    };
    let local = book_for(&text);
    let context = ResolveContext {
        remote: None,
        caller_workspace_root: &call.caller_workspace_root,
        local: &local,
        local_for: None,
        caller_id,
    };
    let options = AddressResolveOptions {
        all_scope: true,
        ..AddressResolveOptions::default()
    };
    let resolved = match ladder(&context, &local, &text, &options) {
        Ok((entry, _)) => entry,
        Err(refusal) => return LocalExisting::Error(refusal),
    };
    if let Some(session) = call.view.target_session(&resolved.thread_id) {
        return LocalExisting::Live(Box::new(session_summary(session, &call.now)));
    }
    let book = send_address_book(call, &resolved.thread_id, true);
    let found = book
        .iter()
        .find(|candidate| candidate.thread_id == resolved.thread_id)
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

async fn resolve_across_account(
    call: &ThreadCall,
    remote: &RemoteThreadSurface,
    address: &str,
    caller_id: &str,
) -> Result<crate::thread::address::Resolved, ThreadToolFailure> {
    let book_for = |part: &str| -> Vec<ThreadAddressEntry> {
        to_thread_address_entries(&send_address_book(call, part, true))
    };
    let local = book_for(address);
    let context = ResolveContext {
        remote: Some(remote),
        caller_workspace_root: &call.caller_workspace_root,
        local: &local,
        local_for: Some(&book_for),
        caller_id,
    };
    let options = AddressResolveOptions {
        all_scope: true,
        probe: true,
        ..AddressResolveOptions::default()
    };
    resolve_address(&context, address, &options)
        .await
        .map_err(|refusal| *refusal)
}

fn relay_host_unsupported(
    message: impl Into<String>,
    next_action: &str,
    device: Option<&str>,
) -> CreatePlan {
    let mut details = serde_json::Map::new();
    details.insert("reason".to_owned(), json!("relay_session_host_unsupported"));
    if let Some(device) = device {
        details.insert("device".to_owned(), json!(device));
    }
    CreatePlan::done(error_json(thread_tool_failure(
        "unsupported",
        message,
        next_action,
        Some(Value::Object(details)),
    )))
}

/// `fork_from` on an engine that does not fork on open (#13031): an open carrying it would start a blank thread, so it is
/// refused before anything opens, naming the source and the device that holds it when that is another one.
pub fn fork_unsupported(source: &str, device: Option<&str>) -> Value {
    let mut details = serde_json::Map::new();
    details.insert("reason".to_owned(), json!("fork_unsupported"));
    details.insert("thread".to_owned(), json!(source));
    if let Some(device) = device {
        details.insert("device".to_owned(), json!(device));
    }
    error_json(thread_tool_failure(
        "unsupported",
        "fork_from cannot fork here: this engine opens a blank session instead of a fork, so no thread was created.",
        "Update omo on the device that holds the source so its engine forks, or create the thread without fork_from and hand it the source's context with thread_send; thread_read reads the source (tool.thread_send() and tool.thread_read() in an eval cell).",
        Some(Value::Object(details)),
    ))
}

/// Whether `device`'s engine forks on open (`relay-session-host.ts` `forks`): asked of that device's engine endpoint, as
/// every capability is (an observing read); an engine that does not answer is taken not to fork.
async fn relay_forks(remote: &RemoteThreadSurface, device: &str) -> bool {
    let info = remote
        .sessions
        .endpoint_command(
            device.to_owned(),
            json!({ "type": "get_protocol_info", "observe": true }),
        )
        .await;
    let RemoteCommandOutcome::Ok { response, .. } = info else {
        return false;
    };
    reply_data(&response)
        .get("capabilities")
        .and_then(Value::as_array)
        .is_some_and(|names| {
            names
                .iter()
                .any(|name| name.as_str() == Some(OPEN_SESSION_FORK))
        })
}

/// thread_create under A3: an id or alias OPENS that session wherever it is (never with a device) - answered without an
/// open when it is live there already (#14335); no id creates on this device; a fork is created on its source's device.
/// When a `RemoteThreadSurface` is attached, live threads on another device resolve via `resolve_address` and return
/// `already_live(displayed_thread(...))` so a create with a prompt sends to them; opening a non-live remote session or
/// creating on another device remains refused until `RelaySessionHost` `open_session`/`find_session` lands. A fork whose
/// source is on another device asks that device's engine whether it forks: one that does not is refused
/// `fork_unsupported`, as upstream; one that does stays refused `relay_session_host_unsupported`, the open this build
/// cannot carry there.
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
    let caller_cwd: Option<&str> =
        Some(call.caller_workspace_root.as_str()).filter(|root| !root.trim().is_empty());
    let remote_surface = call
        .remote
        .as_deref()
        .and_then(RemoteControlLocator::remote_surface);
    let plan = if let Some(remote) = remote_surface {
        let mut local_alias: Option<&'static str> = None;
        let mut cached_target: Option<Result<crate::thread::address::Resolved, ThreadToolFailure>> =
            None;
        if let Some(alias) = value.alias.as_deref() {
            if let Some(registry) = &remote.aliases {
                match registry.resolve(alias.to_owned()).await {
                    Ok(holder) => {
                        let opens_holder = match value.thread.as_deref() {
                            None => false,
                            Some(thread) if thread == alias => true,
                            Some(thread) => {
                                let resolved =
                                    resolve_across_account(call, remote, thread, caller_id).await;
                                let matches = match &resolved {
                                    Ok(hit) => match &hit.where_ {
                                        ResolvedWhere::Device { row } => {
                                            row.device == holder.device
                                                && (row.durable_id.as_deref()
                                                    == Some(holder.session_id.as_str())
                                                    || row.session_id == holder.session_id)
                                        }
                                        ResolvedWhere::Here { .. } => {
                                            hit.thread == holder.session_id
                                        }
                                    },
                                    Err(_) => thread == holder.session_id,
                                };
                                cached_target = Some(resolved);
                                matches
                            }
                        };
                        if !opens_holder {
                            return CreatePlan::done(error_json(thread_tool_failure(
                                "alias_taken",
                                format!("The alias {alias} is held by another thread."),
                                "Choose another alias; the holder is named in details.",
                                Some(json!({
                                    "holder": {
                                        "thread": holder.session_id,
                                        "device": holder.device,
                                    }
                                })),
                            )));
                        }
                    }
                    Err(err) if err.code != "alias_not_found" => {
                        if value.thread.is_some() || !err.signed_out() {
                            let code = if err.signed_out() {
                                "host_unavailable"
                            } else {
                                err.code.as_str()
                            };
                            return CreatePlan::done(error_json(thread_tool_failure(
                                code,
                                err.message,
                                "Retry when the account service is reachable.",
                                None,
                            )));
                        }
                        local_alias = Some("sign_in");
                    }
                    Err(_) => {}
                }
            } else {
                if value.thread.is_some() {
                    return CreatePlan::done(error_json(thread_tool_failure(
                        "host_unavailable",
                        "Thread aliases need the account's alias registry, which this device cannot reach.",
                        "Create or address the thread by its id; retry the alias later.",
                        None,
                    )));
                }
                local_alias = Some("no_registry");
            }
        }
        if let Some(thread) = value.thread.as_deref() {
            let resolved = match cached_target {
                Some(cached) => cached,
                None => resolve_across_account(call, remote, thread, caller_id).await,
            };
            let hit = match resolved {
                Ok(hit) => hit,
                Err(failure) => return CreatePlan::done(error_json(failure)),
            };
            match hit.where_ {
                ResolvedWhere::Here { .. } => open_here(call, &hit.thread, caller_id).await,
                ResolvedWhere::Device { row } => {
                    if row_status(&row) == ThreadStatus::Live {
                        return already_live(Box::new(displayed_thread(&row, None)));
                    }
                    return relay_host_unsupported(
                        format!(
                            "Reopening a non-live thread on remote device {} needs RelaySessionHost and find_session/open_session RPCs, which are not wired on this build.",
                            row.device
                        ),
                        "Open the thread on that device.",
                        Some(&row.device),
                    );
                }
            }
        } else if let Some(fork_from) = value.fork_from.as_deref() {
            let hit = match resolve_across_account(call, remote, fork_from, caller_id).await {
                Ok(hit) => hit,
                Err(failure) => return CreatePlan::done(error_json(failure)),
            };
            match hit.where_ {
                ResolvedWhere::Device { row } => {
                    // The far engine forks by the id it knows the source under: its durable id (the listing key is
                    // the relay's).
                    let source = row
                        .durable_id
                        .clone()
                        .unwrap_or_else(|| row.session_id.clone());
                    if !relay_forks(remote, &row.device).await {
                        return CreatePlan::done(fork_unsupported(&source, Some(&row.device)));
                    }
                    return relay_host_unsupported(
                        format!(
                            "Forking a thread on remote device {} needs RelaySessionHost and remote open_session RPCs, which are not wired on this build.",
                            row.device
                        ),
                        "Fork the thread on that device.",
                        Some(&row.device),
                    );
                }
                ResolvedWhere::Here { .. } => CreatePlan::Open {
                    request: OpenFields {
                        cwd: value.cwd.clone(),
                        session_path: None,
                        fork_from: Some(hit.thread),
                    },
                    creates: true,
                    previous_name: None,
                    local_alias,
                },
            }
        } else if let Some(device) = value.device.as_deref() {
            return relay_host_unsupported(
                format!(
                    "Creating a new thread on remote device {device} needs RelaySessionHost and remote open_session RPCs, which are not wired on this build."
                ),
                "Create the thread on that device directly.",
                Some(device),
            );
        } else {
            CreatePlan::Open {
                request: OpenFields {
                    cwd: value.cwd.clone().or_else(|| caller_cwd.map(str::to_owned)),
                    session_path: None,
                    fork_from: None,
                },
                creates: true,
                previous_name: None,
                local_alias,
            }
        }
    } else {
        if value.device.is_some() || (value.alias.is_some() && value.thread.is_some()) {
            return CreatePlan::done(error_json(thread_tool_failure(
                "host_unavailable",
                "Aliases and other devices need this device signed in to the account's device service.",
                "Sign in, or create the thread on this device without an alias.",
                None,
            )));
        }
        if let Some(thread) = value.thread.as_deref() {
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
        }
    };
    let plan = place_folder(plan, value.worktree.as_ref(), caller_cwd);
    let creates = matches!(plan, CreatePlan::Open { creates: true, .. });
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
