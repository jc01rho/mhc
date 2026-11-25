//! The control half of the thread tools: one body per control tool over the `SessionControl` port.
//!
//! Upstream counterpart: `thread/control/` (omo #12879, #14234, #14237). `port.rs` is the boundary another leg plugs
//! into: this device's endpoint (`local_control.rs`) implements it here, and a session on another device of the account
//! implements the same trait over its relay. Nothing past `locate.rs` reads the device again.

pub mod close_ladder;
pub mod create;
pub mod create_folder;
pub mod create_launch;
pub mod interrupt;
pub mod local_control;
pub mod local_session_host;
pub mod locate;
pub mod open_reply;
pub mod operations;
pub mod plan_create;
pub mod port;
pub mod prompt_start;
pub mod relay_control;
pub mod relay_session_host;
pub mod rename;
pub mod session_host;
