//! Why an open was refused.
//!
//! Upstream counterpart: `thread/control/open-reply.ts` (`modelRefusal`, `forkRefusal`). `openedEngineId` and
//! `openRefusal` read another device's `open_session` reply and belong to the remote open, which is not ported, so a fork
//! refusal here always names this device.

use serde_json::json;

use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};

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

/// An open the engine refused because it cannot run the model the create named: senpi answers
/// `model_unavailable: <why>` as the error text and creates no session.
pub fn model_refusal(text: &str, model: Option<&str>) -> Option<ThreadToolFailure> {
    let model = model?;
    word_at(text, MODEL_UNAVAILABLE)?;
    Some(thread_tool_failure(
        "model_not_found",
        format!("No thread was created: this device cannot run {model}."),
        "Pass a provider/id this device has (thread_set_model, tool.thread_set_model() in an eval cell, lists them), or leave model unset for its default.",
        Some(json!({ "reason": "model_unavailable", "model": model })),
    ))
}

/// An open the engine refused because it cannot fork the source `fork_from` named (#13031): the source is not a session
/// file of that engine (`fork_source_not_local`: it lives on another device or in another agent directory), or no
/// readable session file is there (`fork_source_not_found`). No thread was created.
pub fn fork_refusal(text: &str, fork_from: Option<&str>) -> Option<ThreadToolFailure> {
    let fork_from = fork_from?;
    let reason = [FORK_SOURCE_NOT_LOCAL, FORK_SOURCE_NOT_FOUND]
        .into_iter()
        .filter_map(|reason| word_at(text, reason).map(|at| (at, reason)))
        .min()
        .map(|(_, reason)| reason)?;
    let (message, next_action) = if reason == FORK_SOURCE_NOT_LOCAL {
        (
            format!(
                "No thread was created: {fork_from} is not a session this device holds, so its engine cannot fork it."
            ),
            "A fork is made on the device that holds its source: call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to find that device, and fork the thread by the id listed there.",
        )
    } else {
        (
            format!(
                "No thread was created: this device has no readable session file for {fork_from} to fork."
            ),
            "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) and fork a thread it lists.",
        )
    };
    Some(thread_tool_failure(
        "not_found",
        message,
        next_action,
        Some(json!({ "reason": reason, "thread": fork_from })),
    ))
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
