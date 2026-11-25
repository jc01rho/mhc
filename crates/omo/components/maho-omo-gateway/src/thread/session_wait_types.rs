//! What the carrier and the consumer keep beside the woken row.
//!
//! Upstream counterpart: `thread/gateway/session-wait-types.ts` (omo#13037 A2.6, A3.2.8, A3.5.3, A5.3).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionWaitKind {
    Held,
    Waiting,
}

impl SessionWaitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Waiting => "waiting",
        }
    }
}

/// A session the carrier found held or waiting for a person; a held session is not woken again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWaitState {
    pub durable_id: String,
    pub wait: SessionWaitKind,
    pub hold_kinds: Vec<String>,
    /// The session file's size when the state was written; a different size allows one new probe.
    pub file_size: i64,
    pub written_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWaitWrite {
    pub durable_id: String,
    pub wait: SessionWaitKind,
    pub hold_kinds: Vec<String>,
    pub file_size: i64,
}

/// Holds the engine itself releases when a delivery is admitted: a host restart stamps `unattended` on the sessions it
/// restores, and a delivery to such a session lifts it.
const RELEASED_BY_DELIVERY: [&str; 1] = ["unattended"];

/// Whether a stored state keeps a delivery from the session: a person's wait, or a hold that a delivery does not
/// release. A held state made only of `unattended` holds blocks no wake.
pub fn wait_blocks_delivery(state: &SessionWaitState) -> bool {
    state.wait != SessionWaitKind::Held
        || state
            .hold_kinds
            .iter()
            .any(|kind| !RELEASED_BY_DELIVERY.contains(&kind.as_str()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedWaitingOnKind {
    Hold,
    Approval,
    Wake,
}

/// What a queued row of the session waits on (A2.6, A2.11): a hold, a person's answer, or its wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedWaitingOn {
    pub kind: QueuedWaitingOnKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IntakePair {
    pub sender_device: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeMark {
    pub sender_device: String,
    pub session_id: String,
    pub last_command_id: String,
}

/// A session holding intake rows that a sweep may wake: the devices whose rows are open, and whether same-device rows
/// wait too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenIntakeSession {
    pub durable_id: String,
    pub devices: Vec<String>,
    pub local: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainedMark {
    Drained { last_command_id: String },
    Pending,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWork {
    pub queued: i64,
    pub claimed: i64,
    pub unapplied: i64,
}
