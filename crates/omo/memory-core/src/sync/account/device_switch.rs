//! This device's own memory-sync switch (omo #14093). The service keeps the switch that gates the
//! sync routes for a device; a mark under the pin directory is its local half, so a device that was
//! turned off stays idle without asking the service, in every process on this machine. Both halves
//! change together, the service's first: a call that cannot reach the service changes nothing here.
//! Neither direction touches local memory or the trust pin, so turning sync on again resumes where
//! the device left off.

use std::path::{Path, PathBuf};

use super::pairing_http::{DeviceSyncOff, PairingHttp};
use crate::fs::resilient;
use crate::sync::crypto::signatures::sha256_b64;
use crate::sync::engine::error::EngineResult;

/// One mark per account, beside the trust pins and named like them, outside every memory repository.
fn off_mark(pin_dir: &Path, account: &str) -> PathBuf {
    pin_dir
        .join("sync-off")
        .join(sha256_b64(account.as_bytes()))
}

/// Whether memory sync is turned off on this device for the account.
pub fn is_device_sync_off(pin_dir: &Path, account: &str) -> std::io::Result<bool> {
    match resilient::metadata(&off_mark(pin_dir, account)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Turns this device's switch on: the service's switch, then the mark an earlier disable left here.
pub async fn turn_device_sync_on(
    pairing: &PairingHttp,
    service_device_id: &str,
    pin_dir: &Path,
    account: &str,
) -> EngineResult<()> {
    pairing.set_device_sync(service_device_id, true).await?;
    match resilient::remove_file(&off_mark(pin_dir, account)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisableOutcome {
    Off,
    /// The service would not turn this device's sync off; `code` is its closed error code.
    Refused {
        code: String,
    },
}

/// Turns memory sync off on this device only. The service's switch is cleared first (with its
/// typed confirmation), then this device is marked off, so every later sync session here stays
/// idle. Other devices keep syncing and nothing in local memory changes. Safe to repeat. A service
/// that cannot be reached is an error, and this device stays on.
pub async fn disable_sync(
    pairing: &PairingHttp,
    service_device_id: &str,
    pin_dir: &Path,
    account: &str,
) -> EngineResult<DisableOutcome> {
    if let DeviceSyncOff::Refused { code } = pairing.disable_device_sync(service_device_id).await? {
        return Ok(DisableOutcome::Refused { code });
    }
    let mark = off_mark(pin_dir, account);
    if let Some(parent) = mark.parent() {
        resilient::create_dir_all(parent)?;
    }
    resilient::write(&mark, b"")?;
    Ok(DisableOutcome::Off)
}
