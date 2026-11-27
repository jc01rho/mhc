//! The delivery leg over agentd: this device's gateway pushes a delivery to the device that holds the target, and reads
//! back what became of it (`mhc.delivery.push`, `mhc.delivery.outcome`).
//!
//! Upstream counterpart: the sender half of omo#13037's intake, which the relay never carried: a queued delivery for a
//! session on another device was refused there, so `thread_send` to a remote thread could not be answered with the
//! holder's own row. The daemon hands the record to its local gateway and answers from the row it wrote.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;

use super::link::{AgentdError, DEFAULT_CALL_TIMEOUT};
use super::ports::AgentdClient;

/// What the holding device answered about one pushed delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryPushOutcome {
    /// The holder queued it: `row_id` names its row, `queue_position` its place in the target's queue.
    Accepted {
        row_id: Option<String>,
        queue_position: Option<i64>,
        deduplicated: bool,
    },
    /// The holder refused it; `reason` is its own text.
    Refused { reason: String },
}

/// What became of a delivery the holder already took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The holder settled it: `accepted`, `delivered`, `applied`, ... as its own row names the state.
    Settled {
        outcome: String,
        reason: Option<String>,
        settled_ms: Option<i64>,
    },
    /// The holder holds no such delivery: it never took one under this id.
    Unknown,
}

/// The sender of one delivery record to the device that holds its target.
pub struct AgentdDeliveryPort {
    client: Arc<AgentdClient>,
}

impl AgentdDeliveryPort {
    pub fn new(client: Arc<AgentdClient>) -> Self {
        Self { client }
    }

    /// Pushes one delivery to `device`, for the session `target` there.
    #[allow(clippy::too_many_arguments)]
    pub async fn push(
        &self,
        device: &str,
        delivery_id: &str,
        sender: &Value,
        target: &str,
        record: &Value,
        ledger_id: &str,
    ) -> Result<DeliveryPushOutcome, AgentdError> {
        let args = json!({
            "delivery_id": delivery_id,
            "sender": sender,
            "target": target,
            "record": record,
            "ledger_id": ledger_id,
        });
        let data = self
            .client
            .call(device, "mhc.delivery.push", args, DEFAULT_CALL_TIMEOUT)
            .await?;
        Ok(push_outcome(&data))
    }

    /// What became of a delivery the holder took, or `Unknown` when it holds no such row.
    pub async fn outcome(
        &self,
        device: &str,
        delivery_id: &str,
    ) -> Result<DeliveryOutcome, AgentdError> {
        let args = json!({ "delivery_id": delivery_id });
        let data = self
            .client
            .call(device, "mhc.delivery.outcome", args, DEFAULT_CALL_TIMEOUT)
            .await?;
        Ok(outcome_of(&data))
    }
}

/// The delivery leg as a port the send path and the consumer hold.
pub trait DeliveryPushPort: Send + Sync {
    fn push_delivery(
        &self,
        device: String,
        delivery_id: String,
        sender: Value,
        target: String,
        record: Value,
        ledger_id: String,
    ) -> BoxFuture<Result<DeliveryPushOutcome, AgentdError>>;

    fn delivery_outcome(
        &self,
        device: String,
        delivery_id: String,
    ) -> BoxFuture<Result<DeliveryOutcome, AgentdError>>;
}

impl DeliveryPushPort for AgentdDeliveryPort {
    fn push_delivery(
        &self,
        device: String,
        delivery_id: String,
        sender: Value,
        target: String,
        record: Value,
        ledger_id: String,
    ) -> BoxFuture<Result<DeliveryPushOutcome, AgentdError>> {
        let port = AgentdDeliveryPort::new(Arc::clone(&self.client));
        Box::pin(async move {
            port.push(&device, &delivery_id, &sender, &target, &record, &ledger_id)
                .await
        })
    }

    fn delivery_outcome(
        &self,
        device: String,
        delivery_id: String,
    ) -> BoxFuture<Result<DeliveryOutcome, AgentdError>> {
        let port = AgentdDeliveryPort::new(Arc::clone(&self.client));
        Box::pin(async move { port.outcome(&device, &delivery_id).await })
    }
}

/// A `mhc.delivery.push` answer as the outcome it names. An answer naming no known outcome is a refusal, never an
/// acceptance: a sender that guessed would report a delivery the holder may not hold.
pub fn push_outcome(data: &Value) -> DeliveryPushOutcome {
    let outcome = data
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match outcome {
        "accepted" => DeliveryPushOutcome::Accepted {
            row_id: data
                .get("row_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            queue_position: data.get("queue_position").and_then(Value::as_i64),
            deduplicated: data
                .get("deduplicated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        _ => DeliveryPushOutcome::Refused {
            reason: data
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown_outcome")
                .to_owned(),
        },
    }
}

/// A `mhc.delivery.outcome` answer. The daemon answers `unknown` (or nothing at all) for a delivery it never took.
pub fn outcome_of(data: &Value) -> DeliveryOutcome {
    let outcome = data
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if outcome.is_empty() || outcome == "unknown" || outcome == "not_found" {
        return DeliveryOutcome::Unknown;
    }
    DeliveryOutcome::Settled {
        outcome: outcome.to_owned(),
        reason: data
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        settled_ms: data.get("settled_ms").and_then(Value::as_i64),
    }
}

/// The `sender` object a push carries: this device, and the session that sent when the caller names one.
pub fn sender_of(device: &str, session: Option<&str>) -> Value {
    let mut sender = Map::new();
    sender.insert("device".to_owned(), json!(device));
    if let Some(session) = session {
        sender.insert("session".to_owned(), json!(session));
    }
    Value::Object(sender)
}

/// How long a push waits for the holder's row: the call's own bound, kept short because a queue answer is immediate.
pub const PUSH_TIMEOUT: Duration = DEFAULT_CALL_TIMEOUT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accepted_push_carries_the_holders_row_and_queue_place() {
        assert_eq!(
            push_outcome(&json!({
                "outcome": "accepted",
                "row_id": "row-7",
                "queue_position": 2,
                "deduplicated": true,
            })),
            DeliveryPushOutcome::Accepted {
                row_id: Some("row-7".to_owned()),
                queue_position: Some(2),
                deduplicated: true,
            }
        );
    }

    #[test]
    fn a_refusal_keeps_the_holders_reason_and_an_unknown_answer_is_a_refusal() {
        assert_eq!(
            push_outcome(&json!({ "outcome": "refused", "reason": "queue_full" })),
            DeliveryPushOutcome::Refused {
                reason: "queue_full".to_owned(),
            }
        );
        assert_eq!(
            push_outcome(&json!({ "outcome": "queued" })),
            DeliveryPushOutcome::Refused {
                reason: "unknown_outcome".to_owned(),
            }
        );
    }

    #[test]
    fn an_outcome_names_the_holders_state_or_says_it_holds_none() {
        assert_eq!(
            outcome_of(&json!({ "outcome": "delivered", "settled_ms": 1_700_000_000_000i64 })),
            DeliveryOutcome::Settled {
                outcome: "delivered".to_owned(),
                reason: None,
                settled_ms: Some(1_700_000_000_000),
            }
        );
        assert_eq!(
            outcome_of(&json!({ "outcome": "unknown" })),
            DeliveryOutcome::Unknown
        );
        assert_eq!(outcome_of(&json!({})), DeliveryOutcome::Unknown);
    }

    #[test]
    fn a_sender_names_its_device_and_the_sending_session_when_there_is_one() {
        assert_eq!(sender_of("a", None), json!({ "device": "a" }));
        assert_eq!(
            sender_of("a", Some("rk-1")),
            json!({ "device": "a", "session": "rk-1" })
        );
    }
}
