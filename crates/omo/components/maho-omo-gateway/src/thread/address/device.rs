//! The core's device resolver over an account device list, with its refusal worded for the thread tools.
//!
//! Upstream counterpart: `device.ts` (#14305, #14072).

use maho_thread_address_core::device::ListingOf;
use maho_thread_address_core::{
    DeviceRef, DeviceResolution, DeviceRoster, DeviceRow, ResolveDeviceOptions, resolve_device,
};
use serde_json::json;

use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::remote::ports::{RemoteClient, RemoteThreadSurface};

use super::account::account_devices;
use super::refusals::{ThreadRefusal, as_failure, tool_refusal};

/// One device of the account as the remote layer lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDevice {
    pub device: String,
    pub label: String,
    pub online: bool,
    pub last_seen_ms: Option<i64>,
}

/// This device's account id (`None` with no device service id) and the account's devices.
#[derive(Clone, Default)]
pub struct AccountDevices {
    pub here: Option<String>,
    pub devices: Vec<AccountDevice>,
    pub listing_of: Option<ListingOf>,
}

pub fn device_rows(devices: &[AccountDevice]) -> Vec<DeviceRow> {
    devices
        .iter()
        .map(|row| DeviceRow {
            id: row.device.clone(),
            name: row.label.clone(),
            online: row.online,
            last_seen_ms: row.last_seen_ms,
        })
        .collect()
}

/// The core's device resolver over the account's roster, its refusal worded for the thread tools.
pub fn resolve_account_device(
    account: &AccountDevices,
    text: &str,
    options: ResolveDeviceOptions,
) -> Result<DeviceRef, ThreadRefusal> {
    let roster = DeviceRoster {
        here: account.here.clone(),
        devices: device_rows(&account.devices),
        listing_of: account.listing_of.clone(),
    };
    match resolve_device(&roster, text, options) {
        DeviceResolution::Ok(device) => Ok(device),
        DeviceResolution::Refusal(refusal) => Err(tool_refusal(&refusal, None, None)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedDeviceResolved {
    pub device: String,
    pub devices: Vec<AccountDevice>,
}

/// Resolves a typed device name/id/prefix (or exact id when `exact_id` is set), requiring the device online.
pub async fn resolve_typed_device(
    client: &dyn RemoteClient,
    typed: &str,
    exact_id: bool,
) -> Result<TypedDeviceResolved, ThreadToolFailure> {
    let listed = match account_devices(client).await {
        Ok(listed) => listed,
        Err(reason) => {
            return Err(thread_tool_failure(
                "host_unavailable",
                "This device's device service could not list the account's devices.",
                "Retry when the device service is reachable.",
                Some(json!({ "device": typed, "reason": reason })),
            ));
        }
    };
    let resolved = resolve_account_device(
        &listed,
        typed,
        ResolveDeviceOptions {
            online: true,
            exact_id,
        },
    )
    .map_err(|refusal| as_failure(&refusal))?;
    Ok(TypedDeviceResolved {
        device: resolved.id,
        devices: listed.devices,
    })
}

/// The device a `thread_create` names (#14072): `Ok(None)` when it is this device, `Ok(Some(id))` when it is another
/// device, or `Err(failure)` on refusal.
pub async fn target_device(
    remote: &RemoteThreadSurface,
    text: &str,
) -> Result<Option<String>, ThreadToolFailure> {
    let listed = account_devices(remote.client.as_ref())
        .await
        .map_err(|reason| {
            thread_tool_failure(
                "host_unavailable",
                "This device's device service could not list the account's devices.",
                "Retry when the device service is reachable.",
                Some(json!({ "device": text, "reason": reason })),
            )
        })?;
    let resolved = resolve_account_device(
        &listed,
        text,
        ResolveDeviceOptions {
            online: true,
            exact_id: false,
        },
    )
    .map_err(|refusal| as_failure(&refusal))?;
    if listed.here.as_deref() == Some(resolved.id.as_str()) {
        Ok(None)
    } else {
        Ok(Some(resolved.id))
    }
}
