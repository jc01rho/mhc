//! Cross-device thread addressing (omo `thread/control/relay-control.ts`, `relay-session-host.ts`;
//! omo#12709, #13038, #14248): a `<device>/<session>` address names a session on another of the
//! account's devices, reached through this device's device service over the local API. Same-device
//! addresses stay with the local engine; this module only wraps a tool's execute behind its own port.
//!
//! omo v5.1.79: a session that parked or ended on its device after it was listed answers
//! `route_unlisted`, which a send reports as `not_resumable` (nothing was sent to it).

pub mod device_service;

use std::sync::Arc;

use serde_json::{Value, json};

use super::adapter::BoxFuture;
use super::errors::thread_tool_failure;
use super::tools::ThreadTool;
use super::types::GatewayDeliveryMode;

pub use device_service::{DeviceServicePort, device_service_available};

/// `<device>/<session>`: both parts non-empty, the device part a name or id rather than a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAddress {
    pub device: String,
    pub session: String,
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

/// The device service, as the thread tools see it.
pub trait RemoteThreadPort: Send + Sync {
    /// Delivers `message` to the addressed session; the tool's result value (`kind` ok or error).
    fn send(
        &self,
        address: RemoteAddress,
        message: String,
        mode: Option<GatewayDeliveryMode>,
    ) -> BoxFuture<Value>;
}

pub fn remote_failure(
    code: &str,
    message: impl Into<String>,
    next_action: &str,
    details: Option<Value>,
) -> Value {
    json!({
        "kind": "error",
        "error": thread_tool_failure(code, message, next_action, details),
    })
}

/// `thread_send` (and `thread_handoff`) route a `<device>/<session>` address to `port`; every other
/// address, and every other tool, is left as it was.
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
                        .and_then(parse_remote_address);
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
                    port.send(address, message, mode)
                }),
                ..tool
            }
        })
        .collect()
}
