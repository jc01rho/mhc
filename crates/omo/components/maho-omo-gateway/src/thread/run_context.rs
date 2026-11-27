//! The run's reply allowance (omo `thread/component.ts` `RunContext`, `noteInput`; #13042, #13056, #12317).
//!
//! A delivery a run consumes grants it a reply to that sender for the work the delivery started - the consuming turn
//! and every later turn the engine starts on its own (a reminder, a goal continuation, a monitor wake: a custom message
//! that is no delivery). It ends when a new message starts work after a final answer: a person's message (`user`, which
//! is also how a loop tick or a schedule arrives) or any delivery, whose own sender the new group then holds. A delivery
//! that joins running work adds its sender. An engine-started message after the answer and before the settle keeps it.
//!
//! The turn, cause and hop are recorded as upstream records them, but mhc's `thread_send` sends no `turn_id` and no
//! `cause_delivery_id` (`tools.rs`), so only `reply_to`, `reply_senders` and `answered` are read here: the reply
//! allowance is this module's whole job.

use serde_json::Value;

use super::constants::SESSION_CONTROL_DELIVERY_TYPE;
use super::delivery_ledger::ledger_device;
use super::reply_permission::ReplySender;

/// The newest delivery whose message the model has taken into its context, and the senders this run may answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunContext {
    pub turn: u64,
    pub cause: Option<String>,
    pub chain_hop: u64,
    pub reply_to: Vec<String>,
    pub reply_senders: Vec<ReplySender>,
    pub answered: bool,
}

impl RunContext {
    /// A final answer was given: the next input starts a new group, and nothing of the earlier one survives.
    pub fn start_group(&mut self) {
        self.cause = None;
        self.reply_to.clear();
        self.reply_senders.clear();
        self.chain_hop = 0;
        self.answered = false;
    }

    /// The message the engine just started (`message_start`), as a delivery, an engine-started message, or a person's.
    pub fn note_input(&mut self, message: &Value) {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role != "user" && role != "custom" {
            return;
        }
        let delivery = delivery_of(message);
        if matches!(delivery, Delivery::Malformed) {
            // Like a delivery it ends an answered group (nothing of the earlier group survives) and, unlike a valid
            // one, adds no sender; while work is running it joins it as a valid delivery would.
            if self.answered {
                self.start_group();
            }
            return;
        }
        if role == "custom" && matches!(delivery, Delivery::None) {
            // An engine-started message that is no delivery (a reminder, a goal continuation, a monitor wake) keeps
            // the earlier group's allowance.
            self.answered = false;
            return;
        }
        if self.answered {
            self.start_group();
        }
        let Delivery::Consumed {
            id,
            chain_count,
            agent_sender,
            sender_name,
            device,
            rpc_input,
        } = delivery
        else {
            return;
        };
        // RPC input is typed input, never a delivery this session received: no cause, no reply sender (#12317).
        if rpc_input {
            return;
        }
        self.cause = Some(id);
        self.chain_hop = self.chain_hop.max(chain_count);
        if let Some(sender) = &agent_sender {
            if !self.reply_to.contains(sender) {
                self.reply_to.push(sender.clone());
            }
            let stated = ReplySender {
                session_id: sender.clone(),
                name: sender_name,
                device,
            };
            if !self.reply_senders.iter().any(|existing| {
                existing.session_id == stated.session_id
                    && existing.device == stated.device
                    && existing.name == stated.name
            }) {
                self.reply_senders.push(stated);
            }
        }
    }

    /// A message the engine finished (`message_end`): a final answer (an assistant message without a tool call) makes
    /// the next input start a new group.
    pub fn note_finished(&mut self, message: &Value) {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        self.answered = !calls_tools(message);
    }
}

enum Delivery {
    /// Not a delivery message at all.
    None,
    /// A delivery message without a string id: it starts a new group with nobody approved (#13056).
    Malformed,
    Consumed {
        id: String,
        chain_count: u64,
        agent_sender: Option<String>,
        sender_name: Option<String>,
        device: Option<String>,
        rpc_input: bool,
    },
}

/// senpi `external-admission.js` `deliveryIdOf`: the delivery a `session_control_delivery` message carries.
fn delivery_of(message: &Value) -> Delivery {
    if message.get("role").and_then(Value::as_str) != Some("custom")
        || message.get("customType").and_then(Value::as_str) != Some(SESSION_CONTROL_DELIVERY_TYPE)
    {
        return Delivery::None;
    }
    let details = message.get("details").unwrap_or(&Value::Null);
    let Some(id) = details.get("delivery_id").and_then(Value::as_str) else {
        return Delivery::Malformed;
    };
    let chain_count = details
        .get("chain_count")
        .and_then(Value::as_u64)
        .filter(|count| *count >= 1)
        .unwrap_or(0);
    let sender = details.get("sender").unwrap_or(&Value::Null);
    let agent_sender = (sender.get("kind").and_then(Value::as_str) == Some("agent"))
        .then(|| sender.get("session_id").and_then(Value::as_str))
        .flatten()
        .map(str::to_owned);
    let sender_name = sender
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    Delivery::Consumed {
        id: id.to_owned(),
        chain_count,
        agent_sender,
        sender_name,
        device: ledger_device(id),
        // senpi admits a prompt/steer/follow_up sent over the session's RPC socket as this same message under the
        // client's `clientMessageId`, with this sender it stamps itself.
        rpc_input: sender.get("kind").and_then(Value::as_str) == Some("external")
            && sender.get("platform").and_then(Value::as_str) == Some("rpc"),
    }
}

fn calls_tools(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts
                .iter()
                .any(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn delivery(id: &str, sender: Option<&str>, name: Option<&str>) -> Value {
        json!({
            "role": "custom",
            "customType": SESSION_CONTROL_DELIVERY_TYPE,
            "details": {
                "delivery_id": id,
                "chain_count": 2,
                "sender": match sender {
                    Some(session_id) => json!({
                        "kind": "agent",
                        "session_id": session_id,
                        "name": name,
                    }),
                    None => json!({ "kind": "external", "platform": "rpc" }),
                },
            },
        })
    }

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": text })
    }

    fn final_answer() -> Value {
        json!({ "role": "assistant", "content": [{ "type": "text", "text": "done" }] })
    }

    fn tool_call() -> Value {
        json!({ "role": "assistant", "content": [{ "type": "toolCall", "name": "thread_send" }] })
    }

    const SENDER: &str = "01a1f000-0000-7000-8000-00000000000a";

    #[test]
    fn a_consumed_delivery_grants_its_sender_and_cause() {
        let mut run = RunContext::default();
        run.note_input(&delivery(
            &format!("device:b/{SENDER}"),
            Some(SENDER),
            Some("sender lane"),
        ));
        assert_eq!(
            run.cause.as_deref(),
            Some(&format!("device:b/{SENDER}")[..])
        );
        assert_eq!(run.chain_hop, 2);
        assert_eq!(run.reply_to, vec![SENDER.to_owned()]);
        assert_eq!(
            run.reply_senders,
            vec![ReplySender {
                session_id: SENDER.into(),
                name: Some("sender lane".into()),
                device: Some("b".into()),
            }]
        );
    }

    #[test]
    fn a_persons_message_after_a_final_answer_starts_a_new_group() {
        let mut run = RunContext::default();
        run.note_input(&delivery(&format!("device:b/{SENDER}"), Some(SENDER), None));
        run.note_finished(&final_answer());
        assert!(run.answered);
        run.note_input(&user("a new question"));
        assert!(run.cause.is_none());
        assert!(run.reply_to.is_empty());
        assert!(run.reply_senders.is_empty());
        assert_eq!(run.chain_hop, 0);
    }

    #[test]
    fn an_engine_started_message_after_a_final_answer_keeps_the_allowance() {
        let mut run = RunContext::default();
        run.note_input(&delivery(&format!("device:b/{SENDER}"), Some(SENDER), None));
        run.note_finished(&final_answer());
        run.note_input(&json!({
            "role": "custom",
            "customType": "goal_continuation",
            "content": "continue",
        }));
        assert_eq!(run.reply_to, vec![SENDER.to_owned()]);
        assert!(!run.answered);
    }

    #[test]
    fn a_tool_calling_assistant_message_is_not_a_final_answer() {
        let mut run = RunContext::default();
        run.note_finished(&tool_call());
        assert!(!run.answered);
    }

    #[test]
    fn a_delivery_that_joins_running_work_adds_its_sender() {
        let other = "01a1f000-0000-7000-8000-00000000000b";
        let mut run = RunContext::default();
        run.note_input(&delivery(&format!("device:b/{SENDER}"), Some(SENDER), None));
        run.note_input(&delivery(&format!("device:b/{other}"), Some(other), None));
        assert_eq!(run.reply_to, vec![SENDER.to_owned(), other.to_owned()]);
    }

    #[test]
    fn a_delivery_after_a_final_answer_starts_a_new_group_holding_only_its_own_sender() {
        let other = "01a1f000-0000-7000-8000-00000000000b";
        let mut run = RunContext::default();
        run.note_input(&delivery(&format!("device:b/{SENDER}"), Some(SENDER), None));
        run.note_finished(&final_answer());
        run.note_input(&delivery(&format!("device:b/{other}"), Some(other), None));
        assert_eq!(run.reply_to, vec![other.to_owned()]);
    }

    #[test]
    fn rpc_input_is_no_cause_and_no_reply_sender() {
        let mut run = RunContext::default();
        run.note_input(&delivery("rpc:local/1", None, None));
        assert!(run.cause.is_none());
        assert!(run.reply_to.is_empty());
    }

    #[test]
    fn a_malformed_delivery_ends_an_answered_group_and_adds_nobody() {
        let mut run = RunContext::default();
        run.note_input(&delivery(&format!("device:b/{SENDER}"), Some(SENDER), None));
        run.note_finished(&final_answer());
        run.note_input(&json!({
            "role": "custom",
            "customType": SESSION_CONTROL_DELIVERY_TYPE,
            "details": {},
        }));
        assert!(run.reply_to.is_empty());
        assert!(run.cause.is_none());
    }
}
