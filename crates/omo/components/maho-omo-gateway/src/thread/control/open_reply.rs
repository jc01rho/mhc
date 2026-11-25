//! Why an open was refused.
//!
//! Upstream counterpart: `thread/control/open-reply.ts` (`modelRefusal`). `openedEngineId` and `openRefusal` read another
//! device's `open_session` reply and belong to the remote leg, which is not ported.

use serde_json::json;

use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};

const MODEL_UNAVAILABLE: &str = "model_unavailable";

fn is_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn mentions_model_unavailable(text: &str) -> bool {
    text.match_indices(MODEL_UNAVAILABLE).any(|(start, found)| {
        let before = text[..start].chars().next_back();
        let after = text[start + found.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// An open the engine refused because it cannot run the model the create named: senpi answers
/// `model_unavailable: <why>` as the error text and creates no session.
pub fn model_refusal(text: &str, model: Option<&str>) -> Option<ThreadToolFailure> {
    let model = model?;
    if !mentions_model_unavailable(text) {
        return None;
    }
    Some(thread_tool_failure(
        "model_not_found",
        format!("No thread was created: this device cannot run {model}."),
        "Pass a provider/id this device has (thread_set_model, tool.thread_set_model() in an eval cell, lists them), or leave model unset for its default.",
        Some(json!({ "reason": "model_unavailable", "model": model })),
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
}
