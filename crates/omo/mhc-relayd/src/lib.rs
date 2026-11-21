//! mhc-relayd: mhc's open device service (no counterpart upstream; it stands in for the closed
//! omo-relay-daemon). Protocol: `docs/specs/mhc-relay-v1.md`.
//!
//! On this device it serves the local API v1 (`local_server`): the same socket, per-boot token and
//! mutual HMAC proof omo's device service serves, so `mhc remote`, the thread tools and an omo CLI
//! reach it the same way. Toward the account's other devices it keeps a peer link (`peer`): plain
//! TCP that only goes to a loopback or tailnet address (the overlay encrypts it), opened by an
//! Ed25519 mutual proof over fresh nonces from both sides, between devices that list each other's
//! public key. Sessions are read and commanded through a [`session_host::SessionHost`], the engine
//! host of the registered agent dir.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod daemon;
pub mod identity;
pub mod ledger;
#[cfg(unix)]
pub mod local_server;
pub mod peer;
pub mod session_host;
pub mod settings;
pub mod state;
pub mod wire;

pub use config::{PeerConfig, RelaydConfig};
pub use daemon::{Relayd, RelaydOptions};
pub use identity::DeviceIdentity;
pub use session_host::{HostCommandOutcome, HostSession, SessionHost};
