//! Creates and removes a thread's worktree on another device (omo `thread/worktree/remote-worktree.ts`).

use serde_json::{Map, Value, json};

use super::failures::folder_failure;
use super::thread_worktree::{ThreadWorktree, WorktreeOutcome, WorktreeRequest};
use super::worktree_rpc::THREAD_WORKTREE_RPC;
use crate::thread::control::close_ladder::{RecordAnswer, SESSION_GONE, close_ladder};
use crate::thread::control::open_reply::{open_refusal, opened_engine_id};
use crate::thread::control::port::{CloseOptions, CloseOutcome, ControlError};
use crate::thread::errors::{ThreadToolFailure, is_thread_error_code, thread_tool_failure};
use crate::thread::remote::outcome::relay_outcome_failure;
use crate::thread::remote::ports::{
    RemoteCommandOutcome, RemoteLocateOutcome, RemoteThreadSurface, SessionCommandOptions,
};
use crate::thread::remote::remote_ops::{not_signed_in_failure, reply_data};

fn unsupported(device: &str, message: impl Into<String>, next: &str) -> ThreadToolFailure {
    thread_tool_failure(
        "unsupported",
        message,
        next,
        Some(json!({ "device": device })),
    )
}

fn far_outcome(data: &Map<String, Value>, device: &str) -> WorktreeOutcome {
    if data.get("kind").and_then(Value::as_str) == Some("ok")
        && let Some(Value::Object(wt)) = data.get("worktree")
    {
        let text = |key: &str| wt.get(key).and_then(Value::as_str).map(str::to_owned);
        if let (Some(path), Some(branch), Some(base), Some(repository)) = (
            text("path"),
            text("branch"),
            text("base"),
            text("repository"),
        ) {
            return Ok(ThreadWorktree {
                path,
                branch,
                base,
                repository,
                setup_session_left_open: None,
            });
        }
    }
    if data.get("kind").and_then(Value::as_str) == Some("error")
        && let Some(Value::Object(error)) = data.get("error")
        && let Some(code) = error.get("code").and_then(Value::as_str)
        && is_thread_error_code(code)
    {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let next_action = error
            .get("next_action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut details = error
            .get("details")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        details.insert("device".to_owned(), json!(device));
        return Err(Box::new(thread_tool_failure(
            code,
            message,
            next_action,
            Some(Value::Object(details)),
        )));
    }
    Err(Box::new(unsupported(
        device,
        format!("Device {device} answered the worktree request with an unexpected reply."),
        "Update omo on that device.",
    )))
}

struct FarAnswered {
    data: Map<String, Value>,
    left_open: Option<String>,
}

async fn far_worktree_request(
    remote: &RemoteThreadSurface,
    device: &str,
    cwd: &str,
    request: Value,
) -> Result<FarAnswered, Box<ThreadToolFailure>> {
    let opened = remote
        .sessions
        .endpoint_command(
            device.to_owned(),
            json!({ "type": "open_session", "retain_on_disconnect": true, "cwd": cwd }),
        )
        .await;
    let (response, located_hint) = match &opened {
        RemoteCommandOutcome::Ok {
            response, located, ..
        } => (response, located.clone()),
        _ => {
            return Err(Box::new(open_refusal(
                &opened,
                device,
                Some(cwd),
                None,
                None,
            )));
        }
    };
    let data = reply_data(response);
    let state = data
        .get("state")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let Some(engine_id) = opened_engine_id(&data, &state) else {
        return Err(Box::new(unsupported(
            device,
            format!("Device {device} opened no session for the worktree request."),
            "Update omo on that device.",
        )));
    };
    let key = match located_hint {
        Some(key) => key,
        None => match remote.sessions.locate(device.to_owned(), engine_id).await {
            RemoteLocateOutcome::Located { session_id } => session_id,
            RemoteLocateOutcome::Refused { reason } => {
                return Err(not_signed_in_failure(&reason, device).unwrap_or_else(|| {
                    Box::new(unsupported(
                        device,
                        format!(
                            "Device {device} could not address its worktree session ({reason})."
                        ),
                        "Update omo on that device.",
                    ))
                }));
            }
        },
    };
    let answered = remote
        .sessions
        .session_command(
            device.to_owned(),
            key.clone(),
            json!({ "type": "extension_request", "sessionId": key, "name": THREAD_WORKTREE_RPC, "data": request }),
            SessionCommandOptions::default(),
        )
        .await;
    let result: Result<Map<String, Value>, Box<ThreadToolFailure>> = match &answered {
        RemoteCommandOutcome::Ok { response, .. } => Ok(reply_data(response)),
        RemoteCommandOutcome::Refused { reason, .. } if reason.contains(THREAD_WORKTREE_RPC) => {
            Err(Box::new(unsupported(
                device,
                format!("omo on device {device} cannot create worktrees yet."),
                "Update omo on that device, then retry.",
            )))
        }
        other => Err(relay_outcome_failure(other, device)),
    };
    let closed = close_ladder(
        &key,
        |rpc, _wait_ms| {
            let key = key.clone();
            async move {
                let outcome = remote
                    .sessions
                    .session_command(
                        device.to_owned(),
                        key,
                        rpc,
                        SessionCommandOptions::default(),
                    )
                    .await;
                match outcome {
                    RemoteCommandOutcome::Ok { response, .. } => {
                        Ok(RecordAnswer::Ok(reply_data(&response)))
                    }
                    RemoteCommandOutcome::Refused { reason, .. }
                        if SESSION_GONE.contains(&reason.as_str()) =>
                    {
                        Ok(RecordAnswer::Gone(reason))
                    }
                    other => Err(ControlError::Refused(relay_outcome_failure(&other, device))),
                }
            }
        },
        CloseOptions::default(),
        None,
    )
    .await;
    let data = result?;
    let left_open = match closed {
        Ok(answer) if matches!(answer.outcome, CloseOutcome::Closed | CloseOutcome::NotLive) => {
            None
        }
        Ok(_) => Some(format!("{key} (held_elsewhere)")),
        Err(ControlError::Refused(failure)) => Some(format!("{key} ({})", failure.code.as_str())),
        Err(ControlError::Transport(message)) => Some(format!("{key} ({message})")),
    };
    Ok(FarAnswered { data, left_open })
}

/// Creates a thread's worktree on another device (`far_worktree_request`).
pub async fn prepare_remote_worktree(
    remote: &RemoteThreadSurface,
    device: &str,
    cwd: &str,
    request: &WorktreeRequest,
) -> WorktreeOutcome {
    let mut payload = Map::new();
    payload.insert("cwd".to_owned(), json!(cwd));
    if let Some(branch) = &request.branch {
        payload.insert("branch".to_owned(), json!(branch));
    }
    if let Some(base) = &request.base {
        payload.insert("base".to_owned(), json!(base));
    }
    let answered = far_worktree_request(remote, device, cwd, Value::Object(payload)).await?;
    let mut worktree = far_outcome(&answered.data, device)?;
    worktree.setup_session_left_open = answered.left_open;
    Ok(worktree)
}

/// Removes the worktree `prepare_remote_worktree` made on `device` for a thread whose open then failed (#13450 review).
pub async fn remove_remote_worktree(
    remote: &RemoteThreadSurface,
    device: &str,
    worktree: &ThreadWorktree,
) -> Option<ThreadToolFailure> {
    let request = json!({ "remove": { "path": worktree.path, "branch": worktree.branch } });
    let answered = far_worktree_request(remote, device, &worktree.repository, request).await;
    if let Ok(ref ans) = answered
        && ans.data.get("kind").and_then(Value::as_str) == Some("removed")
    {
        return None;
    }
    let cause_code = match answered {
        Ok(ans) => far_outcome(&ans.data, device)
            .err()
            .map(|e| e.code.as_str().to_owned()),
        Err(err) => Some(err.code.as_str().to_owned()),
    };
    let mut details = Map::new();
    details.insert("device".to_owned(), json!(device));
    details.insert("path".to_owned(), json!(worktree.path));
    details.insert("branch".to_owned(), json!(worktree.branch));
    details.insert("repository".to_owned(), json!(worktree.repository));
    if let Some(code) = cause_code {
        details.insert("cause".to_owned(), json!(code));
    }
    Some(folder_failure(
        "worktree_left_behind",
        format!("The worktree made on device {device} could not be removed."),
        Some(Value::Object(details)),
    ))
}
