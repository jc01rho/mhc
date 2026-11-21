//! The one rename body.
//!
//! Upstream counterpart: `thread/control/rename.ts` (#13066, #13104). Every refusal runs before the name changes
//! anywhere: the target's own liveness first, then the name checks on the target's own listing, and only then the
//! name. This surface holds no account alias registry, so an alias is only the thread's local name: it becomes the name
//! unless one is given, and the result says so (`alias_scope: "local"`, `alias_unavailable: "no_remote"`).

use std::sync::Arc;

use serde_json::Value;

use crate::thread::addressing::normalize_thread_name;
use crate::thread::contracts_results::ThreadRenameOk;
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};

use super::locate::{LocatedControl, ThreadCall, locate_control};
use super::operations::{error_json, not_live, ok_json};
use super::port::{ControlResult, SessionControl};

/// Why a name cannot be taken: no live owner, an empty name, or another visible thread's name.
async fn name_refusal(
    control: Option<&Arc<dyn SessionControl>>,
    thread_id: &str,
    name: &str,
) -> ControlResult<Option<ThreadToolFailure>> {
    let Some(control) = control else {
        return Ok(Some(not_live()));
    };
    if name.is_empty() {
        return Ok(Some(thread_tool_failure(
            "invalid_arguments",
            "The new thread name is empty.",
            "Pass a non-empty name.",
            None,
        )));
    }
    let wanted = normalize_thread_name(name);
    let visible = control.visible().await?;
    Ok(visible
        .iter()
        .find(|entry| entry.thread_id != thread_id && normalize_thread_name(&entry.name) == wanted)
        .map(|existing| {
            thread_tool_failure(
                "name_conflict",
                format!("A thread named \"{}\" already exists.", existing.name),
                "Call thread_list (tool.thread_list() in an eval cell) and choose another name.",
                None,
            )
        }))
}

pub async fn rename_thread(
    call: &ThreadCall,
    thread: Option<&str>,
    name: Option<&str>,
    alias: Option<&str>,
    all_scope: bool,
) -> ControlResult<Value> {
    if name.is_none() && alias.is_none() {
        return Ok(error_json(thread_tool_failure(
            "invalid_arguments",
            "Pass a new name, a new alias, or both.",
            "Retry with name or alias.",
            None,
        )));
    }
    let (thread_id, device, title, control) =
        match locate_control(call, thread.unwrap_or("self"), all_scope).await {
            LocatedControl::Error(failure) => return Ok(error_json(failure)),
            LocatedControl::NotLive {
                thread_id,
                title,
                device,
            } => (thread_id, device, title, None),
            LocatedControl::Control(control) => (
                control.thread_id().to_owned(),
                control.device().map(str::to_owned),
                control.title().to_owned(),
                Some(control),
            ),
        };
    let mut name = name.map(|name| name.trim().to_owned());
    if let Some(wanted) = &name
        && let Some(refusal) = name_refusal(control.as_ref(), &thread_id, wanted).await?
    {
        return Ok(error_json(refusal));
    }
    let local_alias = alias.is_some();
    if let Some(alias) = alias
        && name.is_none()
    {
        let wanted = alias.trim().to_owned();
        if let Some(refusal) = name_refusal(control.as_ref(), &thread_id, &wanted).await? {
            return Ok(error_json(refusal));
        }
        name = Some(wanted);
    }
    if let (Some(wanted), Some(control)) = (&name, &control) {
        control.set_name(wanted.clone()).await?;
    }
    Ok(ok_json(&ThreadRenameOk {
        thread: thread_id,
        name: name.unwrap_or(title),
        alias: None,
        device,
        alias_scope: local_alias.then(|| "local".to_owned()),
        alias_unavailable: local_alias.then(|| "no_remote".to_owned()),
    }))
}
