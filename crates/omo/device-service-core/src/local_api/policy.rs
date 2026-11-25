//! Port of `local-api/policy.ts`: `Status.policy`, the relay-only policy as the device service
//! reports it.

use serde::{Deserialize, Serialize};

pub const POLICY_STATES: [&str; 3] = ["off", "pending", "active"];

pub const THIS_DEVICE_STATES: [&str; 6] = [
    "unclassified",
    "desktop_unregistered",
    "indeterminate",
    "desktop",
    "cli_only",
    "acked",
];

pub const POLICY_WAITING_STATES: [&str; 2] = ["reported", "not_reported"];

pub const POLICY_WAIT_REASONS: [&str; 5] = [
    "not_acked",
    "discovery_missing",
    "discovery_incomplete",
    "inventory_mismatch",
    "other",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUnacked {
    pub device: String,
    pub label: String,
}

/// The one device a pending policy is blocked on, why, and the step that clears it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyBlocker {
    pub device: String,
    pub label: String,
    pub reason: String,
    pub action: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub state: String,
    pub generation: u64,
    pub this_device: String,
    pub homes_pending: u64,
    pub unacked: Vec<PolicyUnacked>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<PolicyBlocker>,
}
