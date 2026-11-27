//! Native port of omo's `omo remote` (packages/omo-native/remote-*.ts) and `omo login|whoami|logout`
//! (account-cli.ts) for mhc, over the device service's local API v1.
//!
//! mhc speaks to whichever device service runs on this device: omo's own, or mhc's open `mhc-relayd`
//! (`mhc remote daemon`, installed by `mhc remote bootstrap`). There is no hosted sign-in: an account
//! is the set of devices that list each other in `relayd.json`, and `mhc login` makes this device's
//! key (local mode).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod account;
pub mod account_cli;
pub mod account_store;
pub mod args;
pub mod command;
pub mod context;
pub mod daemon;
pub mod device;
pub mod device_service_handoff;
pub mod exit;
pub mod render;
pub mod rpc_host;
pub mod sync_status;
pub mod usage;
pub mod verbs;

pub use account::run_account_command;
pub use account_cli::{run_account_cli, run_account_verb};
pub use command::RemoteCommandComponent;
pub use context::RemoteContext;
pub use exit::RemoteExit;
pub use maho_service_endpoints::mark_local_only_process;
pub use sync_status::run_sync;
pub use verbs::run_remote;
