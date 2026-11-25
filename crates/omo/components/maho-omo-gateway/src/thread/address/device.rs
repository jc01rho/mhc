//! The core's device resolver over an account device list, with its refusal worded for the thread tools.
//!
//! Upstream counterpart: `device.ts` (#14305).

use maho_thread_address_core::device::ListingOf;
use maho_thread_address_core::{
    DeviceRef, DeviceResolution, DeviceRoster, DeviceRow, ResolveDeviceOptions, resolve_device,
};

use super::refusals::{ThreadRefusal, tool_refusal};

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
