//! What a thread is doing.
//!
//! Upstream counterpart: `thread/contracts/state.ts` (#13403, #13407). A thread has no online state; `online` belongs
//! to devices.

use serde::{Deserialize, Serialize};

/// `opening` (the local engine lists it but cannot attach yet), `running` (a turn in flight), `idle` (an engine holds it,
/// no turn), `inactive` (its session file exists but no engine holds it) or `archived` (its session file carries the
/// archived flag, whatever else holds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Opening,
    Running,
    Idle,
    Inactive,
    Archived,
}

impl ThreadState {
    pub const ALL: [Self; 5] = [
        Self::Opening,
        Self::Running,
        Self::Idle,
        Self::Inactive,
        Self::Archived,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opening => "opening",
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Inactive => "inactive",
            Self::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }
}

/// The engine's finer activity of a held thread (senpi#6688); `state` folds it to running or idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadActivity {
    Idle,
    Running,
    AwaitingApproval,
    Queued,
    Errored,
}

impl ThreadActivity {
    pub const ALL: [Self; 5] = [
        Self::Idle,
        Self::Running,
        Self::AwaitingApproval,
        Self::Queued,
        Self::Errored,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Queued => "queued",
            Self::Errored => "errored",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|activity| activity.as_str() == value)
    }
}

/// The model a thread runs, as its engine names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadModel {
    pub provider: String,
    pub id: String,
}

/// The kind of provider retry wait a running thread reports (`rate_limit` or `retry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRetryWaitReason {
    RateLimit,
    Retry,
}

impl ThreadRetryWaitReason {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "rate_limit" => Some(Self::RateLimit),
            "retry" => Some(Self::Retry),
            _ => None,
        }
    }
}

/// A running thread asleep between provider retries (senpi#7025): `kind` is `"retry"`,
/// `reason` is `"rate_limit"` or `"retry"`, `until` is when the next attempt starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRetryWait {
    pub kind: String,
    pub reason: ThreadRetryWaitReason,
    pub until: String,
    pub attempt: i64,
    pub max_attempts: i64,
}
