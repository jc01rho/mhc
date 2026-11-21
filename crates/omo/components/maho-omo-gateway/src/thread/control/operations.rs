//! The one body of each control tool, over the control `locate_control` found.
//!
//! Upstream counterpart: `thread/control/operations.ts` and `thread/tools/archive-ops.ts`. A control's typed refusal
//! propagates as `ControlError::Refused`; the tool answers it as data. Every other answer is the tool's result JSON.

use serde::Serialize;
use serde_json::{Value, json};

use crate::thread::contracts_results::{
    ThreadArchiveOk, ThreadCloseOk, ThreadCloseReason, ThreadInterruptOk, ThreadSetModelOk,
    ThreadSetReasoningOk,
};
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::model_match::{ModelMatch, exact_model, match_model, model_pattern_failure};

use super::port::{CloseOutcome, ControlResult, SessionControl, control_identity};

/// A result's ok branch: the value with `kind: "ok"`.
pub fn ok_json<T: Serialize>(value: &T) -> Value {
    match serde_json::to_value(value) {
        Ok(Value::Object(mut object)) => {
            object.insert("kind".into(), json!("ok"));
            Value::Object(object)
        }
        Ok(other) => json!({ "kind": "ok", "value": other }),
        Err(error) => error_json(thread_tool_failure(
            "internal_error",
            format!("The thread result could not be encoded: {error}"),
            "Retry the call.",
            None,
        )),
    }
}

/// A result's error branch: the failure as data.
pub fn error_json(failure: ThreadToolFailure) -> Value {
    json!({ "kind": "error", "error": failure })
}

/// A control's answer for a thread no engine holds live, on this device or another.
pub fn not_live() -> ThreadToolFailure {
    thread_tool_failure(
        "not_resumable",
        "The thread has no live owner.",
        "Retry when the target is live.",
        None,
    )
}

pub async fn interrupt_thread(
    control: &dyn SessionControl,
    turn_id: Option<String>,
) -> ControlResult<Value> {
    let outcome = control.interrupt(turn_id).await?;
    let (thread, device) = control_identity(control);
    Ok(ok_json(&ThreadInterruptOk {
        thread,
        turn_id: outcome.turn_id,
        interrupted: outcome.interrupted,
        background: outcome.background,
        device,
    }))
}

/// Nothing live holds the thread: there is nothing to close, and it stays resumable from its session file.
pub fn close_not_live(thread_id: &str, device: Option<&str>) -> Value {
    ok_json(&ThreadCloseOk {
        thread: thread_id.to_owned(),
        closed: false,
        reason: ThreadCloseReason::NotLive,
        dropped_deliveries: Vec::new(),
        device: device.map(str::to_owned),
    })
}

pub async fn close_thread(control: &dyn SessionControl, force: bool) -> ControlResult<Value> {
    let answer = control.close(force).await?;
    let (thread, device) = control_identity(control);
    if answer.outcome == CloseOutcome::HeldElsewhere {
        return Ok(error_json(thread_tool_failure(
            "foreign_live_owner",
            format!(
                "Thread {thread} has no session file yet and another client still holds it, so only this tool's hold was released."
            ),
            "Close it from the client that holds it, or send it a message first so it can be closed for every holder.",
            device.map(|device| json!({ "device": device })),
        )));
    }
    let closed = answer.outcome == CloseOutcome::Closed;
    Ok(ok_json(&ThreadCloseOk {
        thread,
        closed,
        reason: if closed {
            ThreadCloseReason::Closed
        } else {
            ThreadCloseReason::NotLive
        },
        dropped_deliveries: answer.dropped_deliveries,
        device,
    }))
}

/// One matching rule on the thread's own catalog. A catalog the control holds decides only an exact provider/id, at
/// once; any other pattern is matched against a fresh catalog. A set the engine refuses is answered as a fresh catalog
/// answers it: its own miss, the model it matches set, or the set's refusal when it matches the same model.
pub async fn set_model_on(
    control: &dyn SessionControl,
    model: &str,
    provider: Option<&str>,
) -> ControlResult<Value> {
    if let Some(failure) = model_pattern_failure(model) {
        return Ok(error_json(failure));
    }
    let first = control.get_available_models(false).await?;
    let matched = match (first.held, exact_model(&first.models, model, provider)) {
        (true, Some(held)) => ModelMatch::Match(held),
        (true, None) => match_model(
            &control.get_available_models(true).await?.models,
            model,
            provider,
        ),
        (false, _) => match_model(&first.models, model, provider),
    };
    let chosen = match matched {
        ModelMatch::Match(chosen) => chosen,
        ModelMatch::Failure(failure) => return Ok(error_json(failure)),
    };
    let selected = match control
        .set_model(chosen.provider.clone(), chosen.id.clone())
        .await
    {
        Ok(selected) => selected,
        Err(error) => {
            let Some(refusal) = error.refusal() else {
                return Err(error);
            };
            if refusal.code.as_str() == "idempotency_uncertain" {
                return Err(error);
            }
            let Ok(fresh) = control.get_available_models(true).await else {
                return Err(error);
            };
            match match_model(&fresh.models, model, provider) {
                ModelMatch::Failure(failure) => return Ok(error_json(failure)),
                ModelMatch::Match(rematched)
                    if rematched.provider == chosen.provider && rematched.id == chosen.id =>
                {
                    return Err(error);
                }
                ModelMatch::Match(rematched) => {
                    control.set_model(rematched.provider, rematched.id).await?
                }
            }
        }
    };
    let (thread, device) = control_identity(control);
    Ok(ok_json(&ThreadSetModelOk {
        thread,
        model: selected,
        device,
    }))
}

pub async fn set_thinking_level_on(
    control: &dyn SessionControl,
    level: &str,
    scope: Option<&str>,
) -> ControlResult<Value> {
    if let Err(error) = control.set_thinking_level(level.to_owned()).await {
        if error.refusal().map(|refusal| refusal.code.as_str())
            != Some("thinking_level_unsupported")
        {
            return Err(error);
        }
        let supported = control.get_available_thinking_levels().await?;
        return Ok(error_json(thread_tool_failure(
            "thinking_level_unsupported",
            format!("Thinking level \"{level}\" is not supported by the active model."),
            "Choose a level from the supported list and retry.",
            Some(json!({ "supported": supported })),
        )));
    }
    let (thread, device) = control_identity(control);
    Ok(ok_json(&ThreadSetReasoningOk {
        thread,
        level: level.to_owned(),
        scope: scope.unwrap_or("session").to_owned(),
        device,
    }))
}

/// `thread_archive` for a thread no engine holds: its session file cannot be changed now.
pub fn archive_not_live(thread_id: &str, archived: bool) -> Value {
    error_json(thread_tool_failure(
        "not_resumable",
        format!(
            "Thread {thread_id} has no engine holding it, so its session file cannot be {} now.",
            if archived { "archived" } else { "unarchived" }
        ),
        "Open it with thread_create (tool.thread_create({ thread }) in an eval cell), then retry.",
        None,
    ))
}

/// `thread_archive`: sets the archived flag in the thread's session file and nothing else; the session keeps running.
/// The caller's own thread is refused `turn_active` while its turn runs, so nothing is archived mid-turn.
pub async fn archive_on(
    control: &dyn SessionControl,
    archived: bool,
    caller_id: &str,
) -> ControlResult<Value> {
    if archived
        && control.device().is_none()
        && control.thread_id() == caller_id
        && control.running().await?
    {
        return Ok(error_json(thread_tool_failure(
            "turn_active",
            "This is the calling thread and its turn is still running; archiving it mid-turn is refused.",
            "Archive it from another thread, or after this turn ends.",
            Some(json!({ "thread": control.thread_id() })),
        )));
    }
    control.set_archived(archived).await?;
    Ok(ok_json(&ThreadArchiveOk {
        thread: control.listed(archived).await?,
        archived,
    }))
}
