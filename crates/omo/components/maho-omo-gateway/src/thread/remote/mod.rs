//! Cross-device thread addressing (omo `thread/remote/`, `remote/`; omo#12709, #13038, #14237,
//! #14305, #14318, #14320, #14333, #14428, senpi#7014): a `<device>/<session>` address names a
//! session on another of the account's devices, reached through this device's device service over
//! the local API.

pub mod address_book;
pub mod attach_pool;
pub mod closed_rows;
pub mod cross_device;
pub mod device_service;
pub mod outcome;
pub mod ports;
pub mod remote_ops;

use std::sync::Arc;

use maho_thread_address_core::{HERE, ParsedAddress, parse_address};
use serde_json::{Value, json};

use super::adapter::BoxFuture;
use super::errors::thread_tool_failure;
use super::tools::ThreadTool;
use super::types::GatewayDeliveryMode;

pub use device_service::{DeviceServicePort, device_service_available};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAddress {
    pub device: String,
    pub session: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteSendOptions {
    pub mode: Option<GatewayDeliveryMode>,
    pub expected_turn_id: Option<i64>,
    pub collapse_key: Option<String>,
}

pub fn parse_remote_address(address: &str) -> Option<RemoteAddress> {
    let (device, session) = address.trim().split_once('/')?;
    let plain = |part: &str| {
        !part.is_empty()
            && !part.contains('/')
            && !part.starts_with('.')
            && !part.chars().any(char::is_whitespace)
    };
    (plain(device) && plain(session)).then(|| RemoteAddress {
        device: device.to_owned(),
        session: session.to_owned(),
    })
}

pub trait RemoteThreadPort: Send + Sync {
    fn send(
        &self,
        address: RemoteAddress,
        message: String,
        mode: Option<GatewayDeliveryMode>,
    ) -> BoxFuture<Value>;

    fn send_with_options(
        &self,
        address: RemoteAddress,
        message: String,
        options: RemoteSendOptions,
    ) -> BoxFuture<Value> {
        if options.collapse_key.is_some() {
            let device = address.device;
            return Box::pin(async move {
                remote_failure(
                    "unsupported",
                    format!(
                        "collapse_key reaches sessions on this device only; the delivery to device {device} was not sent."
                    ),
                    "Send without collapse_key, or send from the device that holds the thread.",
                    Some(json!({ "device": device, "field": "collapse_key" })),
                )
            });
        }
        self.send(address, message, options.mode)
    }
}

pub fn remote_failure(
    code: &str,
    message: impl Into<String>,
    next_action: impl Into<String>,
    details: Option<Value>,
) -> Value {
    json!({
        "kind": "error",
        "error": thread_tool_failure(code, message, next_action, details),
    })
}

fn route_address(address: &str) -> Option<RemoteAddress> {
    match parse_address(address) {
        ParsedAddress::Device { device, thread, .. } if device != HERE => Some(RemoteAddress {
            device,
            session: thread,
        }),
        _ => None,
    }
}

/// Routes `<device>/<session>` (excluding `here/...`) on `thread_send` and `thread_handoff` to `port`.
pub fn with_remote_thread_tools(
    tools: Vec<ThreadTool>,
    port: Arc<dyn RemoteThreadPort>,
) -> Vec<ThreadTool> {
    tools
        .into_iter()
        .map(|tool| {
            if tool.name != "thread_send" && tool.name != "thread_handoff" {
                return tool;
            }
            let local = Arc::clone(&tool.execute);
            let port = Arc::clone(&port);
            ThreadTool {
                execute: Arc::new(move |call_id: &str, args: Value| {
                    let address = args
                        .get("thread")
                        .and_then(Value::as_str)
                        .and_then(route_address);
                    let Some(address) = address else {
                        return local(call_id, args);
                    };
                    let message = args
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let mode = args
                        .get("delivery")
                        .and_then(Value::as_str)
                        .and_then(GatewayDeliveryMode::parse);
                    let expected_turn_id = args.get("expected_turn_id").and_then(Value::as_i64);
                    let collapse_key = args
                        .get("collapse_key")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    port.send_with_options(
                        address,
                        message,
                        RemoteSendOptions {
                            mode,
                            expected_turn_id,
                            collapse_key,
                        },
                    )
                }),
                ..tool
            }
        })
        .collect()
}
