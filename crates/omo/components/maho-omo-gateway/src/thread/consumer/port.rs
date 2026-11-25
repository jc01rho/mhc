//! The device service's delivery protocol as an engine process consumes it (omo#13037 design 5.4, receiving half).
//!
//! Upstream counterpart: `thread/consumer/port.ts`. The consumer is written against this port, so it is tested with an
//! in-memory service; the local-API client behind it (`consumer/device-service-port.ts`) is not ported, since the
//! device service in this repository serves no `deliveries.*` protocol. A refused request carries the local-API error
//! code.

use std::sync::Arc;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryInboundEvent {
    pub command_id: String,
    pub sender_device: String,
    pub durable_id: String,
    pub record: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryHoldsEvent {
    pub ask_id: String,
    pub durable_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryPendingPair {
    pub sender_device: String,
    pub session_id: String,
    pub last_command_id: String,
}

/// The acknowledgement of `deliveries.serve` (A7.2.3): `wake` is the receiving device's effective wake switch then;
/// an acknowledgement without the field reads off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeliveryServeAck {
    pub wake: Option<bool>,
}

/// `kept` with `expecting`: the service keeps the mark and names the command it still expects (the backstop, A5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainedReply {
    Released,
    Kept { expecting: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PortRefusal {
    pub code: Option<String>,
    pub message: String,
}

impl PortRefusal {
    pub fn coded(code: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            message: format!("local API refused: {code}"),
            code: Some(code),
        }
    }
}

pub type InboundListener = Arc<dyn Fn(&DeliveryInboundEvent) + Send + Sync>;
pub type HoldsListener = Arc<dyn Fn(&DeliveryHoldsEvent) + Send + Sync>;
pub type WakeSwitchListener = Arc<dyn Fn(&Value) + Send + Sync>;
pub type PortUnsubscribe = Box<dyn FnOnce() + Send>;
pub type PortClosed = Box<dyn FnOnce() + Send>;

pub trait DeliveryServicePort: Send + Sync {
    fn serve(&self, agent_dir: &str, version: i64) -> Result<Value, PortRefusal>;
    /// Echoes the inbound's own pair: the service keys a waiting hand-off by (sender device, command id) (A5.1).
    fn answer(
        &self,
        sender_device: &str,
        command_id: &str,
        reply: &Value,
    ) -> Result<(), PortRefusal>;
    fn held(&self, ask_id: &str, holds: bool) -> Result<(), PortRefusal>;
    fn peer_admitted(&self, device: &str) -> Result<bool, PortRefusal>;
    fn pending(&self, pairs: &[DeliveryPendingPair]) -> Result<(), PortRefusal>;
    fn drained(&self, pair: &DeliveryPendingPair) -> Result<DrainedReply, PortRefusal>;
    fn on_inbound(&self, listener: InboundListener) -> PortUnsubscribe;
    fn on_holds(&self, listener: HoldsListener) -> PortUnsubscribe;
    /// `delivery.wake { enabled }` on this serving connection: the device's wake switch changed (A7.2.3).
    fn on_wake(&self, listener: WakeSwitchListener) -> PortUnsubscribe;
    /// Runs once the serving connection ended (the declaration ends with it); at once when it already has.
    fn on_closed(&self, callback: PortClosed);
    fn close(&self);
}

/// The closed decoder of the serve acknowledgement: only the absent case and a boolean `wake` are accepted, and any
/// other field makes it `None`, so the consumer serves nothing rather than guessing (A7.2.3).
pub fn decode_serve_ack(ack: &Value) -> Option<DeliveryServeAck> {
    let fields = ack.as_object()?;
    if fields.keys().any(|key| key != "wake") {
        return None;
    }
    match fields.get("wake") {
        None => Some(DeliveryServeAck { wake: None }),
        Some(Value::Bool(wake)) => Some(DeliveryServeAck { wake: Some(*wake) }),
        Some(_) => None,
    }
}

pub fn refusal_code(error: &PortRefusal) -> Option<&str> {
    error.code.as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn given_a_serve_acknowledgement_then_only_an_absent_or_boolean_wake_decodes() {
        assert_eq!(
            decode_serve_ack(&json!({})),
            Some(DeliveryServeAck { wake: None })
        );
        assert_eq!(
            decode_serve_ack(&json!({ "wake": true })),
            Some(DeliveryServeAck { wake: Some(true) })
        );
        assert_eq!(decode_serve_ack(&json!({ "wake": "yes" })), None);
        assert_eq!(
            decode_serve_ack(&json!({ "wake": false, "extra": 1 })),
            None
        );
        assert_eq!(decode_serve_ack(&json!(null)), None);
    }

    #[test]
    fn given_a_coded_refusal_then_its_code_is_read_back() {
        let refused = PortRefusal::coded("unknown_type");
        assert_eq!(refusal_code(&refused), Some("unknown_type"));
        assert_eq!(refused.to_string(), "local API refused: unknown_type");
    }
}
