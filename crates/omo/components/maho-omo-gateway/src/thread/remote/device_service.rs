//! The [`RemoteThreadPort`] over this device's device service (local API v1): resolve the device,
//! attach to the session, send one engine record, read its terminal state.

use std::path::PathBuf;

use device_service_core::local_api::transport::is_service_not_running;
use device_service_core::local_api::{
    ClientInfo, CommandOutcome, ConnectOptions, LocalApiClient, connect_local_api,
};
use serde_json::{Value, json};

use super::{RemoteAddress, RemoteThreadPort, remote_failure};
use crate::thread::adapter::BoxFuture;
use crate::thread::types::GatewayDeliveryMode;

pub struct DeviceServicePort {
    pub state_dir: PathBuf,
}

impl DeviceServicePort {
    pub fn for_process() -> Self {
        Self {
            state_dir: device_service_core::device_service_state_dir(),
        }
    }
}

/// Whether a device service has an endpoint in the default state dir: the thread tools reach other
/// devices only then.
pub fn device_service_available() -> bool {
    let state_dir = device_service_core::device_service_state_dir();
    state_dir
        .join(device_service_core::local_api::access::SOCKET_FILE)
        .exists()
        || state_dir
            .join(device_service_core::local_api::transport::ENDPOINT_FILE)
            .exists()
}

fn unavailable(message: impl Into<String>) -> Value {
    remote_failure(
        "host_unavailable",
        message,
        "Start this device's device service (mhc remote daemon) and retry.",
        None,
    )
}

fn id_of(row: &Value) -> String {
    row.get("device")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn this_of(row: &Value) -> bool {
    row.get("this_device").and_then(Value::as_bool) == Some(true)
}

/// The device by id, by name (case-insensitive) or by unique id prefix.
fn resolve_device(devices: &[Value], query: &str) -> Result<(String, bool), Value> {
    let by_name: Vec<&Value> = devices
        .iter()
        .filter(|row| {
            id_of(row) == query
                || row
                    .get("label")
                    .and_then(Value::as_str)
                    .is_some_and(|label| label.eq_ignore_ascii_case(query))
        })
        .collect();
    let candidates: Vec<&Value> = if by_name.is_empty() {
        devices
            .iter()
            .filter(|row| !query.is_empty() && id_of(row).starts_with(query))
            .collect()
    } else {
        by_name
    };
    match candidates.as_slice() {
        [row] => Ok((id_of(row), this_of(row))),
        [] => Err(remote_failure(
            "not_found",
            format!("no device {query} on this account"),
            "Call thread_list for the account's devices and their sessions.",
            None,
        )),
        many => Err(remote_failure(
            "ambiguous_target",
            format!("{query} names more than one device"),
            "Name the device by its id.",
            Some(json!({ "candidates": many.iter().map(|row| id_of(row)).collect::<Vec<_>>() })),
        )),
    }
}

fn result_of(address: &RemoteAddress, device: &str, outcome: &CommandOutcome) -> Value {
    let reason = outcome.reason.clone().unwrap_or_default();
    match outcome.state.as_str() {
        "engine_applied" => {
            let disposition = outcome
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.get("disposition"))
                .and_then(Value::as_str)
                .unwrap_or("handled")
                .to_owned();
            json!({
                "kind": "ok",
                "thread": address.session,
                "device": device,
                "delivery": { "kind": disposition },
                "command_id": outcome.command_id,
            })
        }
        "engine_refused" if reason == "route_unlisted" => remote_failure(
            "not_resumable",
            format!(
                "{}/{} is no longer listed on its device",
                address.device, address.session
            ),
            "Call thread_list for the sessions that device runs now.",
            Some(json!({ "reason": reason })),
        ),
        "engine_refused" => remote_failure(
            "scope_denied",
            format!(
                "{}/{} refused the message ({reason})",
                address.device, address.session
            ),
            "Ask the person on that device to turn remote control on (mhc remote control on).",
            Some(json!({ "reason": reason })),
        ),
        "not delivered: never_sent" | "not confirmed" => unavailable(format!(
            "{} did not answer; nothing was sent",
            address.device
        )),
        _ => remote_failure(
            "idempotency_uncertain",
            format!(
                "the message to {}/{} may or may not have arrived ({reason})",
                address.device, address.session
            ),
            "Read the session before sending again.",
            Some(json!({ "command_id": outcome.command_id })),
        ),
    }
}

async fn send(
    state_dir: PathBuf,
    address: RemoteAddress,
    message: String,
    mode: Option<GatewayDeliveryMode>,
) -> Value {
    if message.is_empty() {
        return remote_failure(
            "invalid_arguments",
            "message must be a non-empty string",
            "Fix the arguments and call again.",
            None,
        );
    }
    let client: LocalApiClient =
        match connect_local_api(ConnectOptions::new(state_dir, ClientInfo::cli("0.0.0"))).await {
            Ok(client) => client,
            Err(error) if is_service_not_running(&error) => {
                return unavailable("this device has no device service running");
            }
            Err(error) => return unavailable(error.reason),
        };
    let answer = async {
        let devices = client
            .devices()
            .await
            .map_err(|error| unavailable(error.reason))?;
        let (device, this_device) = resolve_device(&devices, &address.device)?;
        if this_device {
            return Err(remote_failure(
                "invalid_arguments",
                "that address names this device",
                "Address a session on this device by its own id or name, without the device part.",
                None,
            ));
        }
        let attach = client
            .attach(&device, &address.session)
            .await
            .map_err(|error| match error.code.as_str() {
                "session_not_found" => remote_failure(
                    "not_found",
                    format!("{} lists no session {}", address.device, address.session),
                    "Call thread_list for that device's sessions.",
                    None,
                ),
                _ => unavailable(error.reason),
            })?;
        let method = match mode {
            Some(GatewayDeliveryMode::Steer) => "steer",
            Some(GatewayDeliveryMode::FollowUp) => "follow_up",
            _ => "prompt",
        };
        let outcome = client
            .command(&attach, json!({ "type": method, "message": message }), None)
            .await
            .map_err(|error| unavailable(error.reason))?;
        let _ = client.detach(&attach).await;
        Ok::<Value, Value>(result_of(&address, &device, &outcome))
    }
    .await;
    client.close().await;
    answer.unwrap_or_else(|failure| failure)
}

impl RemoteThreadPort for DeviceServicePort {
    fn send(
        &self,
        address: RemoteAddress,
        message: String,
        mode: Option<GatewayDeliveryMode>,
    ) -> BoxFuture<Value> {
        Box::pin(send(self.state_dir.clone(), address, message, mode))
    }
}
