//! The thread tools that read and control sessions on this device: `thread_list`, `thread_read`, `thread_interrupt`,
//! `thread_close`, `thread_rename`, `thread_set_model`, `thread_set_reasoning` and `thread_archive`.
//!
//! Upstream counterpart: the control half of `thread/tools.ts` (v5.1.79). Each tool takes one host view, resolves its
//! thread through `control::locate`, and runs the one body in `control::operations`. A control's typed refusal is the
//! call's answer as data; a transport failure maps to `host_unavailable`, `unsupported` or `internal_error`, never a
//! thrown error. Without a host surface every tool answers `host_unavailable` instead of disappearing.

use std::sync::Arc;

use serde_json::{Value, json};

use super::adapter::BoxFuture;
use super::contracts_state::ThreadState;
use super::control::locate::{LocatedControl, ThreadCall, book_of, locate_control};
use super::control::operations::{
    archive_not_live, archive_on, close_not_live, close_options, close_thread_with_options,
    error_json, interrupt_thread, not_live, ok_json, set_model_on, set_thinking_level_on,
};
use super::control::port::{ControlError, ControlResult};
use super::control::rename::rename_thread;
use super::errors::thread_tool_failure;
use super::read_ops::{
    ScannedSessions, list_threads, read_thread, scan_listed_sessions, with_backlog,
};
use super::reader::THREAD_READ_MAX_BYTES;
use super::remote::cross_device::list_across;
use super::remote::receipt::read_receipt;
use super::store_ops::rfc3339;
use super::tools::{ThreadTool, ThreadToolsOptions};

/// The thinking levels `thread_set_reasoning` accepts.
pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

const ADDRESS: &str = "Recipient thread: the thread value of a thread_list row (tool.thread_list() in an eval cell) or of any thread tool result, a unique name, or the first 8+ characters of a thread value when only one thread starts with them. A name or prefix shared by several threads returns ambiguous_target with candidates. self addresses the caller's own session.";
const SELF_OR_THREAD: &str = "Thread to act on. Omit or pass self for this session. Accepts the thread value of a thread_list row (tool.thread_list() in an eval cell) or a unique name. A miss names the thread value to pass.";
const ALL_SCOPE: &str = "Address threads in every workspace on this machine instead of only the caller's workspace; left unset, an exact id from another workspace returns scope_denied.";
const ROUTE_TO_TASK: &str = "; to spawn a child task instead, use task.";
const DELIVERY_ID_MAX_CHARS: usize = 128;
const IDEMPOTENCY_KEY: &str = "Caller-chosen key that makes a retried call return the earlier result instead of repeating the side effect; reusing the key with different arguments returns idempotency_conflict.";

/// A transport failure as the call's answer, carrying the message it was thrown with so the call's receipt settles
/// uncertain under it, as a throw's does upstream.
pub fn transport_failure(message: &str) -> Value {
    let answer = mapped_transport_failure(message);
    super::tool_receipts::thrown(answer, message)
}

fn mapped_transport_failure(message: &str) -> Value {
    if let Some(socket) = message.strip_prefix("host_unavailable:") {
        return error_json(thread_tool_failure(
            "host_unavailable",
            format!("The thread host is unavailable at {socket}."),
            "Retry when the session host is running.",
            None,
        ));
    }
    if let Some(command) = message.strip_prefix("unsupported:") {
        return error_json(thread_tool_failure(
            "unsupported",
            format!(
                "The target's endpoint does not accept {command}: that terminal runs an engine from before terminal session controls."
            ),
            "Update the engine where that terminal runs and restart it, or use thread_send and thread_read (tool.thread_send() and tool.thread_read() in an eval cell); thread_list (tool.thread_list() in an eval cell) shows each thread's controls.",
            Some(json!({ "command": command })),
        ));
    }
    error_json(thread_tool_failure(
        "internal_error",
        format!("Thread operation failed: {message}"),
        "Call thread_list (tool.thread_list() in an eval cell) and retry after checking the target.",
        None,
    ))
}

/// A control body's answer: its result, its typed refusal as data, or its transport failure mapped.
pub fn finish(result: ControlResult<Value>) -> Value {
    match result {
        Ok(value) => value,
        Err(ControlError::Refused(failure)) => error_json(*failure),
        Err(ControlError::Transport(message)) => transport_failure(&message),
    }
}

/// One call's reading of the host, or the failure the call answers when no host is connected or none answered.
pub async fn thread_call(options: &ThreadToolsOptions) -> Result<ThreadCall, Value> {
    let Some(host) = options.host.clone() else {
        return Err(error_json(thread_tool_failure(
            "host_unavailable",
            "No thread host is connected to this session, so no session can be listed or controlled from here.",
            "Retry from a session whose host serves thread controls.",
            None,
        )));
    };
    let view = host
        .list_view()
        .await
        .map_err(|error| transport_failure(&error))?;
    let now = rfc3339(options.store.now());
    let book = book_of(&view, &[], &now);
    Ok(ThreadCall {
        host,
        view,
        book,
        caller_id: (options.caller_session_id)(),
        caller_workspace_root: (options.caller_workspace_root)(),
        sessions_directory: options.sessions_directory.clone(),
        remote: options.remote.clone(),
        now,
    })
}

fn invalid(message: impl Into<String>) -> Value {
    error_json(thread_tool_failure(
        "invalid_arguments",
        message,
        "Fix the arguments and call again.",
        None,
    ))
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn flag(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

type ControlBody = fn(Arc<ThreadToolsOptions>, Value) -> BoxFuture<Value>;

fn tool(
    options: &Arc<ThreadToolsOptions>,
    name: &str,
    description: String,
    parameters: Value,
    body: ControlBody,
) -> ThreadTool {
    let options = options.clone();
    ThreadTool {
        name: name.to_owned(),
        description,
        parameters,
        execute: Arc::new(move |_call_id: &str, args: Value| body(options.clone(), args)),
    }
}

fn list_body(options: Arc<ThreadToolsOptions>, args: Value) -> BoxFuture<Value> {
    Box::pin(async move {
        let all_scope = flag(&args, "all_scope").unwrap_or(false);
        let states = match args.get("state") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) if !items.is_empty() => {
                let parsed: Option<Vec<ThreadState>> = items
                    .iter()
                    .map(|item| item.as_str().and_then(ThreadState::parse))
                    .collect();
                match parsed {
                    Some(parsed) => Some(parsed),
                    None => {
                        return invalid(
                            "state lists opening, running, idle, inactive or archived.",
                        );
                    }
                }
            }
            Some(_) => return invalid("state must be a non-empty list of thread states."),
        };
        let call = match thread_call(&options).await {
            Ok(call) => call,
            Err(result) => return result,
        };
        let scanned = match &call.sessions_directory {
            Some(directory) => {
                scan_listed_sessions(directory, all_scope, &call.caller_workspace_root)
            }
            None => ScannedSessions {
                sessions: Vec::new(),
                unread: Vec::new(),
            },
        };
        let listed = list_threads(
            &call.view,
            &call.now,
            &call.caller_workspace_root,
            &scanned,
            all_scope,
            states.as_deref(),
        );
        let with_bl = with_backlog(options.store.as_ref(), listed);
        let surface = call
            .remote
            .as_deref()
            .and_then(|remote| remote.remote_surface());
        ok_json(&list_across(surface, with_bl, all_scope, states.as_deref()).await)
    })
}

fn read_body(options: Arc<ThreadToolsOptions>, args: Value) -> BoxFuture<Value> {
    Box::pin(async move {
        let Some(thread) = text(&args, "thread") else {
            return invalid("thread must be a thread address.");
        };
        let max_bytes = match args.get("max_bytes") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_u64() {
                Some(bytes) if (1..=THREAD_READ_MAX_BYTES as u64).contains(&bytes) => Some(bytes),
                _ => {
                    return invalid(format!(
                        "max_bytes must be a whole number from 1 to {THREAD_READ_MAX_BYTES}."
                    ));
                }
            },
        };
        match args.get("delivery_id") {
            None | Some(Value::Null) => {}
            Some(Value::String(id))
                if (1..=DELIVERY_ID_MAX_CHARS).contains(&id.chars().count()) =>
            {
                return read_receipt(
                    options.store.as_ref(),
                    &(options.caller_session_id)(),
                    thread,
                    id,
                );
            }
            Some(_) => {
                return invalid(format!(
                    "delivery_id must be a string of 1 to {DELIVERY_ID_MAX_CHARS} characters."
                ));
            }
        }
        let call = match thread_call(&options).await {
            Ok(call) => call,
            Err(result) => return result,
        };
        finish(
            read_thread(
                &call,
                thread,
                flag(&args, "all_scope").unwrap_or(false),
                max_bytes,
                text(&args, "cursor"),
            )
            .await,
        )
    })
}

fn interrupt_body(options: Arc<ThreadToolsOptions>, args: Value) -> BoxFuture<Value> {
    Box::pin(async move {
        let Some(thread) = text(&args, "thread") else {
            return invalid("thread must be a thread address.");
        };
        let call = match thread_call(&options).await {
            Ok(call) => call,
            Err(result) => return result,
        };
        match locate_control(&call, thread, flag(&args, "all_scope").unwrap_or(false)).await {
            LocatedControl::Error(failure) => error_json(failure),
            LocatedControl::NotLive { .. } => error_json(not_live()),
            LocatedControl::Control(control) => finish(
                interrupt_thread(control.as_ref(), text(&args, "turn_id").map(str::to_owned)).await,
            ),
        }
    })
}

fn close_body(options: Arc<ThreadToolsOptions>, args: Value) -> BoxFuture<Value> {
    Box::pin(async move {
        let thread = text(&args, "thread").unwrap_or("self");
        let wait_ms = match args.get("wait_ms") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_i64() {
                Some(ms) if (1..=600_000).contains(&ms) => Some(ms),
                _ => return invalid("wait_ms must be an integer from 1 to 600000."),
            },
        };
        let opts = close_options(flag(&args, "force"), flag(&args, "graceful"), wait_ms);
        if opts.graceful && opts.force {
            return invalid(
                "graceful and force exclude each other: a graceful close lets the turn end, a forced one stops it.",
            );
        }
        if opts.wait_ms.is_some() && !opts.graceful {
            return invalid(
                "wait_ms only bounds a graceful close: a close without graceful does not wait for the turn.",
            );
        }
        let call = match thread_call(&options).await {
            Ok(call) => call,
            Err(result) => return result,
        };
        match locate_control(&call, thread, flag(&args, "all_scope").unwrap_or(false)).await {
            LocatedControl::Error(failure) => error_json(failure),
            LocatedControl::NotLive {
                thread_id, device, ..
            } => close_not_live(&thread_id, device.as_deref()),
            LocatedControl::Control(control) => {
                finish(close_thread_with_options(control.as_ref(), opts).await)
            }
        }
    })
}

fn rename_body(options: Arc<ThreadToolsOptions>, args: Value) -> BoxFuture<Value> {
    Box::pin(async move {
        let name = text(&args, "name");
        let alias = text(&args, "alias");
        if name.is_some_and(|name| name.chars().count() > 200) {
            return invalid("name is at most 200 characters.");
        }
        if alias.is_some_and(|alias| alias.chars().count() > 64) {
            return invalid("alias is at most 64 characters.");
        }
        let call = match thread_call(&options).await {
            Ok(call) => call,
            Err(result) => return result,
        };
        finish(
            rename_thread(
                &call,
                text(&args, "thread"),
                name,
                alias,
                flag(&args, "all_scope").unwrap_or(false),
            )
            .await,
        )
    })
}

fn set_model_body(options: Arc<ThreadToolsOptions>, args: Value) -> BoxFuture<Value> {
    Box::pin(async move {
        let (Some(thread), Some(model)) = (text(&args, "thread"), text(&args, "model")) else {
            return invalid("thread and model are required.");
        };
        let call = match thread_call(&options).await {
            Ok(call) => call,
            Err(result) => return result,
        };
        match locate_control(&call, thread, flag(&args, "all_scope").unwrap_or(false)).await {
            LocatedControl::Error(failure) => error_json(failure),
            LocatedControl::NotLive { .. } => error_json(not_live()),
            LocatedControl::Control(control) => {
                finish(set_model_on(control.as_ref(), model, text(&args, "provider")).await)
            }
        }
    })
}

fn set_reasoning_body(options: Arc<ThreadToolsOptions>, args: Value) -> BoxFuture<Value> {
    Box::pin(async move {
        let (Some(thread), Some(level)) = (text(&args, "thread"), text(&args, "level")) else {
            return invalid("thread and level are required.");
        };
        if !THINKING_LEVELS.contains(&level) {
            return invalid(format!(
                "level must be one of {}.",
                THINKING_LEVELS.join(", ")
            ));
        }
        let scope = text(&args, "scope");
        if scope.is_some_and(|scope| scope != "session" && scope != "turn") {
            return invalid("scope must be session or turn.");
        }
        let call = match thread_call(&options).await {
            Ok(call) => call,
            Err(result) => return result,
        };
        match locate_control(&call, thread, flag(&args, "all_scope").unwrap_or(false)).await {
            LocatedControl::Error(failure) => error_json(failure),
            LocatedControl::NotLive { .. } => error_json(not_live()),
            LocatedControl::Control(control) => {
                finish(set_thinking_level_on(control.as_ref(), level, scope).await)
            }
        }
    })
}

fn archive_body(options: Arc<ThreadToolsOptions>, args: Value) -> BoxFuture<Value> {
    Box::pin(async move {
        let Some(thread) = text(&args, "thread") else {
            return invalid("thread must be a thread address.");
        };
        let archived = flag(&args, "archived").unwrap_or(true);
        let call = match thread_call(&options).await {
            Ok(call) => call,
            Err(result) => return result,
        };
        match locate_control(&call, thread, flag(&args, "all_scope").unwrap_or(false)).await {
            LocatedControl::Error(failure) => error_json(failure),
            LocatedControl::NotLive { thread_id, .. } => archive_not_live(&thread_id, archived),
            LocatedControl::Control(control) => {
                finish(archive_on(control.as_ref(), archived, &call.caller_id).await)
            }
        }
    })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// The eight session-control tools over `options.host`.
pub fn create_control_tools(options: &Arc<ThreadToolsOptions>) -> Vec<ThreadTool> {
    vec![
        tool(
            options,
            "thread_list",
            format!("Shows every addressable session on this device with its thread value, name, state and controls, for when the user needs a valid target address before acting on a session{ROUTE_TO_TASK}"),
            object(
                json!({
                    "all_scope": { "type": "boolean", "description": "List this machine's threads from every workspace; the default returns only the caller's workspace." },
                    "state": {
                        "type": "array",
                        "minItems": 1,
                        "items": { "type": "string", "enum": ["opening", "running", "idle", "inactive", "archived"] },
                        "description": "Thread states to list: opening (not yet ready for controls), running, idle, inactive (no engine holds it) or archived; the default lists every state but archived.",
                    },
                    "idempotency_key": { "type": "string", "description": IDEMPOTENCY_KEY },
                }),
                &[],
            ),
            list_body,
        ),
        tool(
            options,
            "thread_read",
            format!("Returns the transcript and latest output of another session, for when the user asks what another session said or produced{ROUTE_TO_TASK}"),
            object(
                json!({
                    "thread": { "type": "string", "description": ADDRESS },
                    "delivery_id": { "type": "string", "minLength": 1, "maxLength": DELIVERY_ID_MAX_CHARS, "description": "Query only the outcome of your original delivery to this exact thread ID. Reads no conversation, sends no message and wakes no session; after evidence expires answers outcome_unavailable." },
                    "cursor": { "type": "string", "description": "Opaque cursor from an earlier thread_read (tool.thread_read() in an eval cell) to page the older messages of a truncated read; a transcript that moved past it returns cursor_stale." },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": THREAD_READ_MAX_BYTES, "description": "Byte budget for the returned transcript slice, default 131072 and capped at 1048576; a longer transcript returns truncated with next_cursor." },
                    "all_scope": { "type": "boolean", "description": ALL_SCOPE },
                    "idempotency_key": { "type": "string", "description": IDEMPOTENCY_KEY },
                }),
                &["thread"],
            ),
            read_body,
        ),
        tool(
            options,
            "thread_interrupt",
            format!("Stops the turn running in another session, and the background jobs that turn started, for when the user wants to halt a session that is going down the wrong path; the result's background lists what was stopped{ROUTE_TO_TASK}"),
            object(
                json!({
                    "thread": { "type": "string", "description": ADDRESS },
                    "turn_id": { "type": "string", "description": "Running turn to stop, defaulting to the newest running turn; interrupting an idle thread returns success with interrupted false." },
                    "all_scope": { "type": "boolean", "description": ALL_SCOPE },
                    "idempotency_key": { "type": "string", "description": IDEMPOTENCY_KEY },
                }),
                &["thread"],
            ),
            interrupt_body,
        ),
        tool(
            options,
            "thread_close",
            format!("Closes a session. Omit thread or pass self to close this session. thread accepts the thread value of a thread_list row (tool.thread_list() in an eval cell) or a unique name; the result is closed true or false plus a reason. A thread with messages it has not read is refused, naming them; pass force: true to close it anyway (dropped_deliveries lists them){ROUTE_TO_TASK}"),
            object(
                json!({
                    "thread": { "type": "string", "minLength": 1, "description": SELF_OR_THREAD },
                    "all_scope": { "type": "boolean", "description": ALL_SCOPE },
                    "force": { "type": "boolean", "description": "Close even when the thread has messages it has not read yet; they are dropped and listed in dropped_deliveries." },
                    "graceful": { "type": "boolean", "description": "Let the running turn end instead of stopping it; new messages are refused and unread ones go back to their senders. Monitors, loops and the goal stop cleanly. Also closes a terminal's thread, which then exits." },
                    "wait_ms": { "type": "integer", "minimum": 1, "maximum": 600_000, "description": "How long a graceful close waits for the turn (default 30000, cut to what the call has left)." },
                    "idempotency_key": { "type": "string", "description": IDEMPOTENCY_KEY },
                }),
                &[],
            ),
            close_body,
        ),
        tool(
            options,
            "thread_rename",
            "Renames a session's label, for when the user asks to call a session something clearer. Omit thread or pass self to rename this session. thread accepts the thread value of a thread_list row (tool.thread_list() in an eval cell) or a unique name; a miss names the thread value to pass".to_owned(),
            object(
                json!({
                    "thread": { "type": "string", "minLength": 1, "description": SELF_OR_THREAD },
                    "name": { "type": "string", "minLength": 1, "maxLength": 200, "description": "New display label for the thread; the thread keeps its id as its address, and a label already used by another visible thread returns name_conflict." },
                    "alias": { "type": "string", "minLength": 1, "maxLength": 64, "description": "Alias for the thread. With no account alias registry it becomes the thread's local name instead (unless name is set) and the result carries alias_scope \"local\"." },
                    "all_scope": { "type": "boolean", "description": ALL_SCOPE },
                    "idempotency_key": { "type": "string", "description": IDEMPOTENCY_KEY },
                }),
                &[],
            ),
            rename_body,
        ),
        tool(
            options,
            "thread_set_model",
            "Moves a session onto a different model, for when the user asks to switch that session to another model or a cheaper one".to_owned(),
            object(
                json!({
                    "thread": { "type": "string", "description": ADDRESS },
                    "model": { "type": "string", "minLength": 1, "description": "Model to switch the thread to, as provider/id, an exact model id, or a case-insensitive fragment of the id or display name, resolved against the target's catalog. Append a thinking level such as :max to apply it after switching. No match returns model_not_found with the available list; an absent provider adds details.reason = provider_not_configured and lists the target's available providers. Several matches return model_ambiguous with candidates." },
                    "provider": { "type": "string", "description": "Restricts resolution to one provider when the fragment alone would match models from several providers." },
                    "all_scope": { "type": "boolean", "description": ALL_SCOPE },
                    "idempotency_key": { "type": "string", "description": IDEMPOTENCY_KEY },
                }),
                &["thread", "model"],
            ),
            set_model_body,
        ),
        tool(
            options,
            "thread_set_reasoning",
            "Sets how much reasoning a session applies, for when the user asks to raise or lower the thinking effort of a session".to_owned(),
            object(
                json!({
                    "thread": { "type": "string", "description": ADDRESS },
                    "level": { "type": "string", "enum": THINKING_LEVELS, "description": "Thinking level to apply; a level the thread's active model cannot run returns thinking_level_unsupported with the supported list and leaves the thread unchanged." },
                    "scope": { "type": "string", "enum": ["session", "turn"], "description": "session (default) or turn; either changes only this thread's level and never the level the device remembers for the model." },
                    "all_scope": { "type": "boolean", "description": ALL_SCOPE },
                    "idempotency_key": { "type": "string", "description": IDEMPOTENCY_KEY },
                }),
                &["thread", "level"],
            ),
            set_reasoning_body,
        ),
        tool(
            options,
            "thread_archive",
            "Archives or unarchives a session so the session list hides or shows it again, for when the user is finished with a session but wants to keep it".to_owned(),
            object(
                json!({
                    "thread": { "type": "string", "description": ADDRESS },
                    "archived": { "type": "boolean", "description": "true (the default) archives the thread, so thread_list (tool.thread_list() in an eval cell) hides it unless state includes archived; false unarchives it." },
                    "all_scope": { "type": "boolean", "description": ALL_SCOPE },
                    "idempotency_key": { "type": "string", "description": IDEMPOTENCY_KEY },
                }),
                &["thread"],
            ),
            archive_body,
        ),
    ]
}
