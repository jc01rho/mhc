//! One transcript entry as a tool result shows it.
//!
//! Upstream counterpart: `thread/tools/read-item.ts` (omo#14283). An assistant message a provider error ended carries no
//! content, only its `errorMessage`; rendering the content alone showed a 429 storm as a row of `[]`, so a reader could
//! not tell a rate-limit wait from an empty reply. The error text becomes an `error` part of the item, capped, so the
//! whole message (which carries the uncapped error) is never rendered.

use serde_json::{Value, json};

use super::contracts_results::ThreadTranscriptItem;
use super::read_ops::transcript_role;

/// The most of a provider error message one item shows: a 429 storm must stay readable.
pub const ERROR_TEXT_MAX_CHARS: usize = 500;

/// The item's `content`, as the JSON text a reader sees.
fn content_of(message: &Value) -> String {
    let capped = message
        .get("errorMessage")
        .and_then(Value::as_str)
        .filter(|_| message.get("stopReason").and_then(Value::as_str) == Some("error"))
        .map(|error| error.chars().take(ERROR_TEXT_MAX_CHARS).collect::<String>());
    let Some(capped) = capped else {
        return serde_json::to_string(message.get("content").unwrap_or(message))
            .unwrap_or_default();
    };
    let mut parts: Vec<Value> = match message.get("content") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(parts)) => parts.clone(),
        Some(content) => vec![content.clone()],
    };
    parts.push(json!({ "type": "error", "text": capped }));
    serde_json::to_string(&parts).unwrap_or_default()
}

/// One transcript entry as a tool result item, numbered from 1.
pub fn read_item(message: &Value, index: usize) -> ThreadTranscriptItem {
    ThreadTranscriptItem {
        seq: i64::try_from(index).map_or(i64::MAX, |index| index + 1),
        role: transcript_role(message.get("role")).to_owned(),
        content: content_of(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_errored_message_with_no_content_shows_its_error_instead_of_an_empty_list() {
        let message = json!({
            "role": "assistant",
            "content": [],
            "stopReason": "error",
            "errorMessage": "429 rate limit from provider"
        });
        let item = read_item(&message, 0);
        assert_eq!(item.seq, 1);
        assert_eq!(item.role, "assistant");
        let parsed: Value = serde_json::from_str(&item.content).expect("json");
        assert_eq!(
            parsed,
            json!([{ "type": "error", "text": "429 rate limit from provider" }])
        );
        assert!(!item.content.contains("stopReason"));
    }

    #[test]
    fn a_long_provider_error_is_capped_so_the_item_stays_readable() {
        let long = "x".repeat(ERROR_TEXT_MAX_CHARS + 200);
        let message = json!({
            "role": "assistant",
            "stopReason": "error",
            "errorMessage": long
        });
        let item = read_item(&message, 2);
        assert_eq!(item.seq, 3);
        let expected = serde_json::to_string(&json!([{
            "type": "error",
            "text": "x".repeat(ERROR_TEXT_MAX_CHARS),
        }]))
        .expect("json");
        assert_eq!(item.content, expected);
    }

    #[test]
    fn an_ordinary_message_keeps_its_content_and_reports_no_error() {
        let message = json!({
            "role": "toolResult",
            "content": [{ "type": "text", "text": "ok" }]
        });
        let item = read_item(&message, 0);
        assert_eq!(item.role, "tool");
        let parsed: Value = serde_json::from_str(&item.content).expect("json");
        assert_eq!(parsed, json!([{ "type": "text", "text": "ok" }]));
        let stops = json!({
            "role": "assistant",
            "content": "done",
            "stopReason": "endTurn"
        });
        assert_eq!(read_item(&stops, 1).content, "\"done\"");
        assert_eq!(read_item(&json!({ "role": "system" }), 0).role, "system");
    }

    #[test]
    fn an_errored_message_keeps_its_content_and_ignores_an_error_that_is_not_text() {
        let message = json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "partial" }],
            "stopReason": "error",
            "errorMessage": "connection reset"
        });
        let item = read_item(&message, 0);
        let parsed: Value = serde_json::from_str(&item.content).expect("json");
        assert_eq!(parsed.as_array().map(Vec::len), Some(2));
        assert_eq!(parsed[0]["text"], "partial");
        assert_eq!(parsed[1]["text"], "connection reset");
        let no_text = json!({
            "role": "assistant",
            "content": [],
            "stopReason": "error",
            "errorMessage": { "code": 429 }
        });
        assert_eq!(read_item(&no_text, 0).content, "[]");
    }
}
