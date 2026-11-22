use crate::session_resources::register_session_resource_cleanup;
use crate::types::{ContentBlock, Context, Message, Model};
use regex::{Regex, RegexBuilder};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

static BOUND_TO_DIFFERENT_CONVERSATION: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(
        r"Invalid `signature` in `thinking` block\. The block is bound to a different conversation",
    )
    .case_insensitive(true)
    .build()
    .unwrap_or_else(|e| panic!("valid regex: {e}"))
});

static BINDING_CONTROL_NAMED: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"block_binding|thinking-binding-controls")
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|e| panic!("valid regex: {e}"))
});

pub const THINKING_BINDING_RECOVERY_DIAGNOSTIC: &str = "anthropic_thinking_binding_recovery";

pub fn is_thinking_binding_mismatch_text(text: &str) -> bool {
    BOUND_TO_DIFFERENT_CONVERSATION.is_match(text)
}

pub fn is_thinking_binding_mismatch_error(status: Option<u16>, message: &str) -> bool {
    status == Some(400) && is_thinking_binding_mismatch_text(message)
}

pub fn is_thinking_binding_control_refused_error(status: Option<u16>, message: &str) -> bool {
    status == Some(400)
        && BINDING_CONTROL_NAMED.is_match(message)
        && !is_thinking_binding_mismatch_text(message)
}

pub fn accepts_thinking_binding(model: &Model) -> bool {
    model.id.to_ascii_lowercase().contains("claude")
}

fn signed_thinking_count(message: &Message) -> usize {
    let Message::Assistant(assistant) = message else {
        return 0;
    };
    assistant
        .content
        .iter()
        .filter(|block| {
            matches!(
                block,
                ContentBlock::Thinking(thinking)
                    if thinking
                        .thinking_signature
                        .as_deref()
                        .is_some_and(|sig| !sig.is_empty())
            )
        })
        .count()
}

pub fn replays_signed_thinking(messages: &[Message]) -> bool {
    messages
        .iter()
        .any(|message| signed_thinking_count(message) > 0)
}

pub fn bind_replayed_thinking(params: &mut Map<String, Value>) {
    let Some(Value::Object(thinking)) = params.get_mut("thinking") else {
        return;
    };
    let kind = thinking.get("type").and_then(Value::as_str);
    if !matches!(kind, Some("adaptive" | "enabled")) {
        return;
    }
    if thinking.contains_key("block_binding") {
        return;
    }
    thinking.insert(
        "block_binding".into(),
        json!({ "prefix_mismatch_behavior": "drop_block" }),
    );
}

static REJECTED_THINKING_TURNS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static BINDING_REFUSED_SESSIONS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

static THINKING_BINDING_CLEANUP: LazyLock<()> = LazyLock::new(|| {
    let _ = register_session_resource_cleanup(Arc::new(|session_id: Option<&str>| {
        let mut rejected = REJECTED_THINKING_TURNS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut refused = BINDING_REFUSED_SESSIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match session_id {
            None => {
                rejected.clear();
                refused.clear();
            }
            Some(id) => {
                let prefix = format!("{id}\0");
                rejected.retain(|k| !k.starts_with(&prefix));
                refused.retain(|k| !k.starts_with(&prefix));
            }
        }
        Ok(())
    }));
});

fn turn_key(message: &Message) -> String {
    match message {
        Message::Assistant(assistant) => assistant
            .response_id
            .clone()
            .unwrap_or_else(|| format!("{}@{}", assistant.model, assistant.timestamp)),
        _ => String::new(),
    }
}

pub struct ThinkingBindingRecovery {
    local_turns: HashSet<String>,
    local_binding_refused: bool,
    session_key: Option<String>,
}

impl ThinkingBindingRecovery {
    pub fn new(session_key: Option<String>) -> Self {
        let _ = LazyLock::force(&THINKING_BINDING_CLEANUP);
        Self {
            local_turns: HashSet::new(),
            local_binding_refused: false,
            session_key,
        }
    }

    pub fn binding_allowed(&self) -> bool {
        if self.local_binding_refused {
            return false;
        }
        let Some(session_key) = &self.session_key else {
            return true;
        };
        !BINDING_REFUSED_SESSIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(session_key)
    }

    pub fn refuse_binding(&mut self) {
        self.local_binding_refused = true;
        if let Some(session_key) = &self.session_key {
            BINDING_REFUSED_SESSIONS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(session_key.clone());
        }
    }

    fn is_rejected(&self, message: &Message) -> bool {
        let key = turn_key(message);
        if self.local_turns.contains(&key) {
            return true;
        }
        let Some(session_key) = &self.session_key else {
            return false;
        };
        REJECTED_THINKING_TURNS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&format!("{session_key}\0{key}"))
    }

    pub fn reject_replayed_thinking(&mut self, messages: &[Message]) -> usize {
        let mut blocks = 0;
        for message in messages {
            let count = signed_thinking_count(message);
            if count == 0 || self.is_rejected(message) {
                continue;
            }
            blocks += count;
            let key = turn_key(message);
            self.local_turns.insert(key.clone());
            if let Some(session_key) = &self.session_key {
                REJECTED_THINKING_TURNS
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(format!("{session_key}\0{key}"));
            }
        }
        blocks
    }

    pub fn strip(&self, context: &Context) -> (Context, bool) {
        let mut removed = false;
        let messages: Vec<Message> = context
            .messages
            .iter()
            .map(|message| {
                if signed_thinking_count(message) == 0 || !self.is_rejected(message) {
                    return message.clone();
                }
                let Message::Assistant(assistant) = message else {
                    return message.clone();
                };
                removed = true;
                let mut next = (**assistant).clone();
                next.content.retain(|block| {
                    !matches!(
                        block,
                        ContentBlock::Thinking(thinking)
                            if thinking
                                .thinking_signature
                                .as_deref()
                                .is_some_and(|sig| !sig.is_empty())
                    )
                });
                Message::Assistant(Arc::new(next))
            })
            .collect();
        if removed {
            let mut next = context.clone();
            next.messages = messages;
            (next, true)
        } else {
            (context.clone(), false)
        }
    }
}

pub fn thinking_binding_unrecoverable_message(cause_message: &str) -> String {
    format!(
        "Anthropic rejected this session's earlier reasoning as bound to a different conversation, and the request rebuilt without it was rejected too. Start a new session to continue. ({cause_message})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AssistantMessage;

    fn assistant_with_signed_thinking(id: &str, signature: Option<&str>) -> Message {
        let mut msg: AssistantMessage = serde_json::from_value(json!({
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "reasoning", "thinkingSignature": signature },
                { "type": "text", "text": "answer" }
            ],
            "api": "anthropic-messages",
            "provider": "anthropic",
            "model": "claude-test",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "stop",
            "timestamp": 0
        }))
        .expect("assistant message fixture");
        msg.response_id = Some(id.to_owned());
        Message::Assistant(Arc::new(msg))
    }

    #[test]
    fn classifies_binding_mismatch_and_control_refusal_400_errors() {
        let mismatch = "400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation.\"}}";
        assert!(is_thinking_binding_mismatch_error(Some(400), mismatch));
        assert!(!is_thinking_binding_mismatch_error(Some(500), mismatch));
        assert!(!is_thinking_binding_control_refused_error(
            Some(400),
            mismatch
        ));

        let control_refused = "400 thinking.block_binding: Extra inputs are not permitted";
        assert!(is_thinking_binding_control_refused_error(
            Some(400),
            control_refused
        ));
        assert!(!is_thinking_binding_mismatch_error(
            Some(400),
            control_refused
        ));
    }

    #[test]
    fn binds_and_strips_replayed_signed_thinking_per_session() {
        let mut params = json!({
            "thinking": { "type": "enabled", "budget_tokens": 2048 }
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        bind_replayed_thinking(&mut params);
        assert_eq!(
            params["thinking"]["block_binding"],
            json!({ "prefix_mismatch_behavior": "drop_block" })
        );

        let context = Context {
            messages: vec![
                assistant_with_signed_thinking("resp-1", Some("sig-1")),
                assistant_with_signed_thinking("resp-2", None),
            ],
            ..Default::default()
        };
        assert!(replays_signed_thinking(&context.messages));

        let mut recovery = ThinkingBindingRecovery::new(Some("sess-bind-1\0model".into()));
        assert!(recovery.binding_allowed());
        let removed_count = recovery.reject_replayed_thinking(&context.messages);
        assert_eq!(removed_count, 1);

        let (stripped, removed) = recovery.strip(&context);
        assert!(removed);
        assert!(!replays_signed_thinking(&stripped.messages));
        let Message::Assistant(second) = &stripped.messages[1] else {
            panic!("expected assistant");
        };
        assert_eq!(second.content.len(), 2);

        recovery.refuse_binding();
        assert!(!recovery.binding_allowed());
        let _ = crate::session_resources::cleanup_session_resources(Some("sess-bind-1"));
    }
}
