//! Why an open was refused, and the engine session id an `open_session` reply names.
//!
//! Upstream counterpart: `thread/control/open-reply.ts` (`openedEngineId`, `modelRefusal`, `forkRefusal`,
//! `openRefusal`).

use serde_json::{Map, Value, json};

use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::remote::outcome::endpoint_outcome_failure;
use crate::thread::remote::ports::RemoteCommandOutcome;
use crate::thread::worktree::failures::folder_failure;

const MODEL_UNAVAILABLE: &str = "model_unavailable";
const FORK_SOURCE_NOT_LOCAL: &str = "fork_source_not_local";
const FORK_SOURCE_NOT_FOUND: &str = "fork_source_not_found";

fn is_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// Where `word` first stands as a whole word in `text` (the regex `\bword\b`).
fn word_at(text: &str, word: &str) -> Option<usize> {
    text.match_indices(word)
        .find(|(start, found)| {
            let before = text[..*start].chars().next_back();
            let after = text[start + found.len()..].chars().next();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
        .map(|(start, _)| start)
}

fn non_empty_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The engine session id the `open_session` reply names: its durable session id, else the relay session id.
pub fn opened_engine_id(data: &Map<String, Value>, state: &Map<String, Value>) -> Option<String> {
    non_empty_str(state, "durableSessionId")
        .or_else(|| non_empty_str(data, "durableSessionId"))
        .or_else(|| non_empty_str(data, "durable_id"))
        .or_else(|| non_empty_str(state, "sessionId"))
        .or_else(|| non_empty_str(data, "sessionId"))
        .or_else(|| non_empty_str(data, "session_id"))
}

/// An open the engine refused because it cannot run the model the create named.
pub fn model_refusal(text: &str, model: Option<&str>) -> Option<ThreadToolFailure> {
    model_refusal_on(text, model, None)
}

pub fn model_refusal_on(
    text: &str,
    model: Option<&str>,
    device: Option<&str>,
) -> Option<ThreadToolFailure> {
    let model = model?;
    word_at(text, MODEL_UNAVAILABLE)?;
    let where_text = device.map_or_else(|| "this device".to_owned(), |d| format!("device {d}"));
    let mut details = Map::new();
    details.insert("reason".to_owned(), json!("model_unavailable"));
    details.insert("model".to_owned(), json!(model));
    if let Some(d) = device {
        details.insert("device".to_owned(), json!(d));
    }
    Some(thread_tool_failure(
        "model_not_found",
        format!("No thread was created: {where_text} cannot run {model}."),
        format!(
            "Pass a provider/id {where_text} has (thread_set_model, tool.thread_set_model() in an eval cell, lists them), or leave model unset for its default."
        ),
        Some(Value::Object(details)),
    ))
}

/// An open the engine refused because it cannot fork the source `fork_from` named (#13031).
pub fn fork_refusal(text: &str, fork_from: Option<&str>) -> Option<ThreadToolFailure> {
    fork_refusal_on(text, fork_from, None)
}

pub fn fork_refusal_on(
    text: &str,
    fork_from: Option<&str>,
    device: Option<&str>,
) -> Option<ThreadToolFailure> {
    let fork_from = fork_from?;
    let reason = [FORK_SOURCE_NOT_LOCAL, FORK_SOURCE_NOT_FOUND]
        .into_iter()
        .filter_map(|reason| word_at(text, reason).map(|at| (at, reason)))
        .min()
        .map(|(_, reason)| reason)?;
    let where_text = device.map_or_else(|| "this device".to_owned(), |d| format!("device {d}"));
    let (message, next_action) = if reason == FORK_SOURCE_NOT_LOCAL {
        (
            format!(
                "No thread was created: {fork_from} is not a session {where_text} holds, so its engine cannot fork it."
            ),
            "A fork is made on the device that holds its source: call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to find that device, and fork the thread by the id listed there.",
        )
    } else {
        (
            format!(
                "No thread was created: {where_text} has no readable session file for {fork_from} to fork."
            ),
            "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) and fork a thread it lists.",
        )
    };
    let mut details = Map::new();
    details.insert("reason".to_owned(), json!(reason));
    details.insert("thread".to_owned(), json!(fork_from));
    if let Some(d) = device {
        details.insert("device".to_owned(), json!(d));
    }
    Some(thread_tool_failure(
        "not_found",
        message,
        next_action,
        Some(Value::Object(details)),
    ))
}

/// Why another device's engine refused an `open_session`.
pub fn open_refusal(
    outcome: &RemoteCommandOutcome,
    device: &str,
    cwd: Option<&str>,
    model: Option<&str>,
    fork_from: Option<&str>,
) -> ThreadToolFailure {
    if let RemoteCommandOutcome::Refused { reason, .. } = outcome {
        if let Some(unrunnable) = model_refusal_on(reason, model, Some(device)) {
            return unrunnable;
        }
        if let Some(unforkable) = fork_refusal_on(reason, fork_from, Some(device)) {
            return unforkable;
        }
        let lower = reason.to_ascii_lowercase();
        if lower.contains("missing_session_cwd")
            || lower.contains("working directory does not exist")
        {
            let mut details = Map::new();
            details.insert("device".to_owned(), json!(device));
            if let Some(cwd) = cwd {
                details.insert("path".to_owned(), json!(cwd));
            }
            return folder_failure(
                "folder_missing",
                format!(
                    "Folder {} does not exist on device {device}.",
                    cwd.unwrap_or("(default)")
                ),
                Some(Value::Object(details)),
            );
        }
        if reason == "session_cwd_not_directory" {
            let mut details = Map::new();
            details.insert("device".to_owned(), json!(device));
            if let Some(cwd) = cwd {
                details.insert("path".to_owned(), json!(cwd));
            }
            return folder_failure(
                "not_a_directory",
                format!(
                    "{} on device {device} is not a folder.",
                    cwd.unwrap_or("(default)")
                ),
                Some(Value::Object(details)),
            );
        }
    }
    *endpoint_outcome_failure(outcome, device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_the_engine_cannot_run_is_model_not_found_and_names_the_model() {
        let refused = model_refusal(
            "model_unavailable: no model anthropic/nope",
            Some("anthropic/nope"),
        )
        .expect("refused");
        assert_eq!(refused.code.as_str(), "model_not_found");
        assert_eq!(
            refused.details.expect("details"),
            json!({ "reason": "model_unavailable", "model": "anthropic/nope" })
        );
    }

    #[test]
    fn other_failures_and_creates_without_a_model_are_not_a_model_refusal() {
        assert!(model_refusal("model_unavailable: x", None).is_none());
        assert!(model_refusal("open_failed: disk", Some("a/b")).is_none());
        assert!(model_refusal("not_model_unavailable_really", Some("a/b")).is_none());
        assert!(model_refusal("Error: model_unavailable.", Some("a/b")).is_some());
    }

    #[test]
    fn an_engine_that_cannot_fork_the_source_is_not_found_naming_the_reason_and_the_source() {
        let not_local = fork_refusal(
            "thread RPC request failed: \"fork_source_not_local\"",
            Some("dur-1"),
        )
        .expect("refused");
        assert_eq!(not_local.code.as_str(), "not_found");
        assert_eq!(
            not_local.details.expect("details"),
            json!({ "reason": "fork_source_not_local", "thread": "dur-1" })
        );
        assert!(
            not_local
                .next_action
                .contains("device that holds its source")
        );
        let not_found =
            fork_refusal("fork_source_not_found: dur-1", Some("dur-1")).expect("refused");
        assert_eq!(
            not_found.details.expect("details")["reason"],
            "fork_source_not_found"
        );
        assert_eq!(
            fork_refusal(
                "fork_source_not_found, then fork_source_not_local",
                Some("dur-1")
            )
            .and_then(|refused| refused.details)
            .map(|details| details["reason"].clone()),
            Some(json!("fork_source_not_found"))
        );
    }

    #[test]
    fn other_failures_and_opens_without_a_source_are_not_a_fork_refusal() {
        assert!(fork_refusal("fork_source_not_local", None).is_none());
        assert!(fork_refusal("open_failed: disk", Some("dur-1")).is_none());
        assert!(fork_refusal("xfork_source_not_local", Some("dur-1")).is_none());
    }
}
