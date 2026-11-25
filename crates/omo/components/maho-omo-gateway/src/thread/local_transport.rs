//! Local delivery transport wrapping `GatewayEngine` and `SameDeviceWake` (`thread/gateway/local-transport.ts`).

use std::sync::Arc;

use super::adapter::BoxFuture;
use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::errors::thread_tool_failure;
use super::same_device_wake::SameDeviceWake;
use super::types::{GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundTarget {
    Local { thread_id: String },
    Remote { device: String, thread_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundSender {
    Gateway(GatewaySender),
    Person { durable_id: String },
}

#[derive(Debug, Clone)]
pub struct OutboundDelivery {
    pub sender: OutboundSender,
    pub target: OutboundTarget,
    pub text: String,
    pub mode: Option<GatewayDeliveryMode>,
    pub expected_turn_id: Option<String>,
    pub all_scope: bool,
    pub idempotency_key: Option<String>,
    pub collapse_key: Option<String>,
    pub root_id: Option<String>,
}

impl OutboundDelivery {
    pub fn local(
        sender: GatewaySender,
        thread_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            sender: OutboundSender::Gateway(sender),
            target: OutboundTarget::Local {
                thread_id: thread_id.into(),
            },
            text: text.into(),
            mode: None,
            expected_turn_id: None,
            all_scope: false,
            idempotency_key: None,
            collapse_key: None,
            root_id: None,
        }
    }
}

#[derive(Clone)]
pub struct LocalTransport {
    engine: GatewayEngine,
    same_device_wake: Option<Arc<SameDeviceWake>>,
}

impl LocalTransport {
    pub fn new(engine: GatewayEngine, same_device_wake: Option<Arc<SameDeviceWake>>) -> Self {
        Self {
            engine,
            same_device_wake,
        }
    }

    pub fn deliver(&self, delivery: OutboundDelivery) -> BoxFuture<GatewayDeliveryResult> {
        let engine = self.engine.clone();
        let same_device_wake = self.same_device_wake.clone();
        Box::pin(async move {
            let thread_id = match delivery.target {
                OutboundTarget::Local { thread_id } => thread_id,
                OutboundTarget::Remote { .. } => {
                    return GatewayDeliveryResult::Error {
                        error: thread_tool_failure(
                            "unsupported",
                            "This transport reaches sessions on this device only.",
                            "Route this delivery through the remote device transport.",
                            None,
                        ),
                    };
                }
            };
            let sender = match delivery.sender {
                OutboundSender::Gateway(sender) => sender,
                OutboundSender::Person { .. } => {
                    return GatewayDeliveryResult::Error {
                        error: thread_tool_failure(
                            "unsupported",
                            "A person's send goes through a direct session attachment, not the gateway.",
                            "Attach to the session instead of calling the gateway transport.",
                            None,
                        ),
                    };
                }
            };
            let expected_turn_id = match delivery.expected_turn_id {
                Some(raw) => {
                    let parsed = (!raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()))
                        .then(|| raw.parse::<i64>().ok())
                        .flatten();
                    match parsed {
                        Some(epoch) => Some(epoch),
                        None => {
                            return GatewayDeliveryResult::Error {
                                error: thread_tool_failure(
                                    "invalid_arguments",
                                    "expected_turn_id must be a non-negative integer turn epoch.",
                                    "Pass the target's numeric turn_epoch.",
                                    None,
                                ),
                            };
                        }
                    }
                }
                None => None,
            };
            let mut request = GatewayDeliverRequest::new(sender, thread_id, delivery.text);
            request.mode = delivery.mode;
            request.expected_turn_id = expected_turn_id;
            request.all_scope = delivery.all_scope;
            request.idempotency_key = delivery.idempotency_key;
            request.collapse_key = delivery.collapse_key;
            request.root_id = delivery.root_id;
            let result = engine.deliver(request).await;
            match same_device_wake {
                Some(wake) => wake.after_send(result),
                None => result,
            }
        })
    }
}
