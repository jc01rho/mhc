//! The [`RemoteThreadPort`] over this device's device service (local API v1): resolve the device
//! via `maho_thread_address_core`, attach to the session, probe state when deciding a lane, send one
//! engine record with steer-race fallback, and read its terminal state.

use std::path::PathBuf;

use device_service_core::local_api::transport::is_service_not_running;
use device_service_core::local_api::{
    ClientInfo, CommandOutcome, ConnectOptions, LocalApiClient, LocalApiError, connect_local_api,
};
use maho_thread_address_core::ResolveDeviceOptions;
use serde_json::{Value, json};

use super::outcome::{
    command_not_ready, engine_state_of, outcome_failure, plan_delivery, result_of, steer_conflict,
};
use super::{RemoteAddress, RemoteSendOptions, RemoteThreadPort, remote_failure};
use crate::thread::adapter::BoxFuture;
use crate::thread::address::device::{AccountDevice, AccountDevices, resolve_account_device};
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

fn text_field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub fn account_devices(rows: &[Value]) -> AccountDevices {
    let here = rows
        .iter()
        .find(|row| row.get("this_device").and_then(Value::as_bool) == Some(true))
        .map(|row| text_field(row, "device").to_owned())
        .filter(|id| !id.is_empty());
    let devices = rows
        .iter()
        .map(|row| AccountDevice {
            device: text_field(row, "device").to_owned(),
            label: text_field(row, "label").to_owned(),
            online: row.get("online").and_then(Value::as_bool) == Some(true),
            last_seen_ms: row.get("last_seen_ms").and_then(Value::as_i64),
        })
        .collect();
    AccountDevices {
        here,
        devices,
        listing_of: None,
    }
}

pub fn resolve_device(devices: &[Value], query: &str) -> Result<(String, bool), Value> {
    let account = account_devices(devices);
    match resolve_account_device(
        &account,
        query,
        ResolveDeviceOptions {
            online: true,
            exact_id: false,
        },
    ) {
        Ok(device) => Ok((device.id, device.here)),
        Err(refusal) => Err(json!({ "kind": "error", "error": *refusal })),
    }
}

fn listing_names_device(
    device: &str,
    rows: &[Value],
    incomplete: &[Value],
    devices: &Value,
) -> bool {
    rows.iter().any(|row| text_field(row, "device") == device)
        || incomplete
            .iter()
            .any(|named| named.as_str() == Some(device))
        || devices
            .as_object()
            .is_some_and(|states| states.contains_key(device))
}

/// #14358: a command the other device refused `attach_required` ran nothing there: this device's attach is dropped and
/// made again, and the record is sent once more; a second refusal is the answer.
async fn command_reattaching(
    client: &LocalApiClient,
    attach: &mut String,
    device: &str,
    session: &str,
    rpc: Value,
) -> Result<CommandOutcome, LocalApiError> {
    let outcome = client.command(attach.as_str(), rpc.clone(), None).await?;
    if command_not_ready(&outcome) != Some("attach_required") {
        return Ok(outcome);
    }
    let _ = client.detach(attach.as_str()).await;
    *attach = client.attach(device, session).await?;
    client.command(attach.as_str(), rpc, None).await
}

async fn send(
    state_dir: PathBuf,
    address: RemoteAddress,
    message: String,
    options: RemoteSendOptions,
) -> Value {
    if message.is_empty() {
        return remote_failure(
            "invalid_arguments",
            "message must be a non-empty string",
            "Fix the arguments and call again.",
            None,
        );
    }
    if options.collapse_key.is_some() {
        return remote_failure(
            "unsupported",
            format!(
                "collapse_key reaches sessions on this device only; the delivery to device {} was not sent.",
                address.device
            ),
            "Send without collapse_key, or send from the device that holds the thread.",
            Some(json!({ "device": address.device, "field": "collapse_key" })),
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
        let (device, this_device) = match resolve_device(&devices, &address.device) {
            Ok(resolved) => resolved,
            Err(failure) if failure["error"]["code"] == "not_found" => {
                let listed = client
                    .sessions_list(Some(json!({ "device": &address.device })), Some(3))
                    .await
                    .ok();
                if let Some(listing) = listed
                    && listing_names_device(
                        &address.device,
                        &listing.rows,
                        &listing.incomplete,
                        &listing.devices,
                    )
                {
                    (address.device.clone(), false)
                } else {
                    return Err(failure);
                }
            }
            Err(failure) => return Err(failure),
        };
        if this_device {
            return Err(remote_failure(
                "invalid_arguments",
                "that address names this device",
                "Address a session on this device by its own id or name, without the device part.",
                None,
            ));
        }
        let mut attach =
            client
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

        let requested_mode = options.mode.unwrap_or(GatewayDeliveryMode::Auto);
        let (mut method, mut planned_lane, expected_turn) = if requested_mode
            != GatewayDeliveryMode::FollowUp
        {
            let probe = command_reattaching(
                &client,
                &mut attach,
                &device,
                &address.session,
                json!({ "type": "get_state" }),
            )
            .await;
            match probe {
                Ok(outcome) if outcome.applied() => {
                    let state = engine_state_of(outcome.receipt.as_ref().unwrap_or(&Value::Null));
                    match plan_delivery(
                        &state,
                        requested_mode,
                        options.expected_turn_id,
                        &device,
                        &address.session,
                    ) {
                        Ok(plan) => (plan.method, plan.planned_lane, plan.expected_turn_id),
                        Err(failure) => {
                            let _ = client.detach(&attach).await;
                            return Err(failure);
                        }
                    }
                }
                Ok(outcome) => {
                    let _ = client.detach(&attach).await;
                    return Err(outcome_failure(
                        &address,
                        &device,
                        &outcome,
                        options.expected_turn_id,
                    ));
                }
                Err(error) => {
                    let _ = client.detach(&attach).await;
                    return Err(unavailable(error.reason));
                }
            }
        } else {
            ("follow_up", "follow_up", None)
        };

        let build_record = |kind: &str, turn_id: Option<i64>| -> Value {
            let mut record = json!({ "type": kind, "message": message });
            if kind == "steer"
                && let Some(turn_id) = turn_id
                && let Some(map) = record.as_object_mut()
            {
                map.insert("expectedTurnId".into(), Value::String(turn_id.to_string()));
            }
            record
        };

        let mut outcome = match command_reattaching(
            &client,
            &mut attach,
            &device,
            &address.session,
            build_record(method, expected_turn),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = client.detach(&attach).await;
                return Err(unavailable(error.reason));
            }
        };
        if requested_mode == GatewayDeliveryMode::Auto
            && method == "steer"
            && steer_conflict(&outcome).is_some()
        {
            method = "follow_up";
            planned_lane = "follow_up";
            outcome = match command_reattaching(
                &client,
                &mut attach,
                &device,
                &address.session,
                build_record(method, None),
            )
            .await
            {
                Ok(retried) => retried,
                Err(error) => {
                    let _ = client.detach(&attach).await;
                    return Err(unavailable(error.reason));
                }
            };
        }
        let _ = client.detach(&attach).await;
        Ok::<Value, Value>(result_of(
            &address,
            &device,
            &outcome,
            method,
            planned_lane,
            options.expected_turn_id,
        ))
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
        self.send_with_options(
            address,
            message,
            RemoteSendOptions {
                mode,
                ..RemoteSendOptions::default()
            },
        )
    }

    fn send_with_options(
        &self,
        address: RemoteAddress,
        message: String,
        options: RemoteSendOptions,
    ) -> BoxFuture<Value> {
        Box::pin(send(self.state_dir.clone(), address, message, options))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Vec<Value> {
        vec![
            json!({ "device": "a11ce0de5e1f0123", "label": "this mac", "online": true, "this_device": true }),
            json!({ "device": "b7c41e09d2a64f1e", "label": "Studio Laptop", "online": true, "this_device": false }),
            json!({ "device": "c0ffee5a1d2b3c4d", "label": "attic", "online": false, "this_device": false }),
        ]
    }

    #[test]
    fn resolves_by_id_nfkc_name_prefix_and_refuses_offline_devices() {
        let rows = account();
        assert_eq!(
            resolve_device(&rows, "Ｓｔｕｄｉｏ  Ｌａｐｔｏｐ").unwrap(),
            ("b7c41e09d2a64f1e".to_owned(), false)
        );
        assert_eq!(
            resolve_device(&rows, "b7c41e").unwrap(),
            ("b7c41e09d2a64f1e".to_owned(), false)
        );
        assert_eq!(
            resolve_device(&rows, "here").unwrap(),
            ("a11ce0de5e1f0123".to_owned(), true)
        );
        let offline = resolve_device(&rows, "attic").unwrap_err();
        assert_eq!(offline["error"]["code"], "device_offline");
    }
}
