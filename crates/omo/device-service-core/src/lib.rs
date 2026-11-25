//! Native port of omo `packages/device-service-core` (omo v5.1.79).
//!
//! The device service is the one process on a device that owns its connection to the account's
//! other devices; every other process reaches it through the local API v1 (`local_api`), on a
//! private Unix socket (or a loopback port on Windows) in its state dir (`state_dir`). `cold_wake`
//! installs the always-on session host unit beside it.
//!
//! mhc talks to whichever device service runs: omo's own, or mhc's open `mhc-relayd`. Both serve the
//! same wire contract (`docs/local-api.md` upstream), so nothing here names a transport between
//! devices.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod cold_wake;
pub mod engine_endpoint;
pub mod hand_device_keys;
pub mod local_api;
pub mod release;
pub mod state_dir;

pub use engine_endpoint::{
    ENDPOINT_PRIMARY_MIN_SERVICE_VERSION, EngineEndpoints, engine_endpoints_of,
};
pub use hand_device_keys::{
    CancelController, CancelSignal, DeviceServiceStatus, StatusClient,
    device_service_endpoint_path, read_device_service_status, read_device_service_status_with,
};
pub use state_dir::{DEVICE_SERVICE_STATE_DIR_ENV, device_service_state_dir};
