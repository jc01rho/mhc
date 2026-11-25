//! The store's record of woken sessions.
//!
//! Upstream counterpart: `thread/gateway/woken-types.ts` (omo#13037 A3.1). Every write is one transaction on the
//! store's own clock; a write after `requested` is a compare-and-set on the generation a takeover raises, so a carrier
//! that lost its lease writes nothing.

use serde::{Deserialize, Serialize};

use super::types::ProcessIdentity;

/// The life of a woken session's row (A3.1.1); a row past `requested` holds one of the store's places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WokenSessionState {
    Requested,
    Opening,
    Open,
    Closing,
}

impl WokenSessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Opening => "opening",
            Self::Open => "open",
            Self::Closing => "closing",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "requested" => Some(Self::Requested),
            "opening" => Some(Self::Opening),
            "open" => Some(Self::Open),
            "closing" => Some(Self::Closing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WokenSessionRow {
    pub durable_id: String,
    pub session_file: String,
    pub host_instance: Option<String>,
    pub state: WokenSessionState,
    pub requested_at: i64,
    pub opened_at: Option<i64>,
    pub first_waiting_at: Option<i64>,
    /// The carrier: the process identity the store records for a claimant; null while the row is `requested`.
    pub lease_owner: Option<ProcessIdentity>,
    pub lease_expires_at: Option<i64>,
    pub generation: i64,
    pub attempts: i64,
    pub next_attempt_at: Option<i64>,
}

/// A write that names the generation it was judged on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WokenSessionFence {
    pub durable_id: String,
    pub generation: i64,
}

impl WokenSessionFence {
    pub fn new(durable_id: impl Into<String>, generation: i64) -> Self {
        Self {
            durable_id: durable_id.into(),
            generation,
        }
    }
}

/// Another carrier raised the generation: the write was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LostLease;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeRequestOutcome {
    pub row: Box<WokenSessionRow>,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeStartOutcome {
    Started {
        row: Box<WokenSessionRow>,
    },
    NotYet {
        next_attempt_at: i64,
    },
    /// Another carrier already moved the row past `requested`.
    InProgress {
        row: Box<WokenSessionRow>,
    },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeTakeOverOutcome {
    TakenOver {
        row: Box<WokenSessionRow>,
        previous_owner_alive: bool,
    },
    LeaseLive {
        row: Box<WokenSessionRow>,
    },
    /// A `requested` row has no lease to take over; `start` takes it.
    NotLeased {
        row: Box<WokenSessionRow>,
    },
    NotFound,
    LostLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeRetryOutcome {
    Deferred { row: Box<WokenSessionRow> },
    Failed { attempts: i64 },
    LostLease,
}
