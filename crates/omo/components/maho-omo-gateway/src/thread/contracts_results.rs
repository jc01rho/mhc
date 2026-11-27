//! Discriminated thread result unions.
//!
//! Upstream counterpart: `thread/contracts/results.ts` (omo v5.1.79). The error branch is shared data, never an
//! exception. One field names a session (#14044 step 4a): every result that names one carries `thread`, the exact
//! string the verbs take back; routing ids, sockets, session paths and endpoint internals stay in the address book.

use maho_thread_address_core::DeviceListing;
use serde::{Deserialize, Serialize};

use super::contracts_state::{ThreadActivity, ThreadModel, ThreadRetryWait, ThreadState};
use super::control::port::CloseGraceful;
use super::errors::ThreadToolFailure;
use super::types::DeliveryWait;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadDataError {
    Error { error: ThreadToolFailure },
}

impl ThreadDataError {
    pub fn of(error: ThreadToolFailure) -> Self {
        Self::Error { error }
    }
}

/// Deprecated beside `state`, kept for one release: `resumable` when no engine holds the thread, else `live`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Live,
    Resumable,
}

impl ThreadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Resumable => "resumable",
        }
    }
}

/// The endpoint serving a thread: a host (with the session's routing id) or a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadEndpointSummary {
    pub kind: String,
    pub socket: String,
    pub routing_id: Option<String>,
    pub durable_session_id: String,
}

/// A thread as the address book and the controls hold it, never as a tool answers it: `thread_id` is its durable id
/// and `endpoint` names the socket and routing id that reach it. Tool results carry [`ThreadResultSummary`] instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub thread_id: String,
    pub name: String,
    pub status: ThreadStatus,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ThreadEndpointSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
}

/// A thread as a tool result names it (#14044 step 4a): `thread` is the address every thread tool takes back, and
/// only what a model acts on is kept. `link` is `[title](omo-thread://v1/<thread>)`, pasted when telling the user
/// about the thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadResultSummary {
    pub thread: String,
    pub name: String,
    /// Set when the name is only a fallback (first message or folder), never a real name (thread-address-core `rowName`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unnamed: Option<bool>,
    pub status: ThreadStatus,
    pub state: ThreadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ThreadModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<ThreadActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// A running thread asleep between provider retries (senpi#7025, #14278).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<ThreadRetryWait>,
    /// The version of the engine that endpoint runs, as its `get_protocol_info` names it (#14278).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// This device's undelivered messages for the thread (#14428): how many, where, since when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlog: Option<ThreadBacklog>,
    /// Set when the row comes from another device's cached listing: when that listing was taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    /// The display name of the device that holds the thread, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub link: String,
    /// The account device that holds the thread, when it is not this device's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls: Option<Vec<String>>,
    /// Present only on a thread listed from disk because its endpoint stopped answering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadDelivery {
    Steered {
        turn_id: String,
    },
    Started {
        turn_id: String,
    },
    Queued {
        queue_position: i64,
        /// What the receiver waits on while the delivery is queued (#13014).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waiting_on: Option<DeliveryWait>,
    },
    QueuedOffline {
        queue_position: i64,
    },
}

/// What a send through the session gateway adds to a delivery result: the durable `delivery_id` and the lane the
/// target used. The kind of endpoint that served the target is an endpoint internal (#14044) and stays out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadGatewayDeliveryFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_mode: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadAddressResolution {
    Id,
    ExactName,
    Fuzzy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadReadSource {
    LiveHost,
    SessionJsonl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadTranscriptItem {
    pub seq: i64,
    pub role: String,
    pub content: String,
}

/// `thread_create` answers the thread it made or opened flat: its `thread` is the address to pass back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadCreateOk {
    #[serde(flatten)]
    pub thread: ThreadResultSummary,
    pub deduplicated: bool,
}

/// One thread's undelivered backlog as a listing row shows it (#14428): the oldest wait is ISO 8601.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadBacklog {
    pub held: i64,
    pub held_by_session: i64,
    pub waiting_in_gateway: i64,
    pub oldest_at: String,
}

/// One device of this account as `thread_list` reports it across devices (omo #14305, #14304, #14308, #14295).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDevice {
    pub device: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub online: bool,
    pub last_seen: Option<String>,
    pub this_device: bool,
    pub listing: DeviceListing,
    pub threads: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadListOk {
    pub threads: Vec<ThreadResultSummary>,
    pub scope: String,
    /// More session files were in scope than one list reads: older inactive threads are left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactive_truncated: Option<bool>,
    /// How many unread session files' threads the filter dropped (#14304): only when it shows inactive or archived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactive_dropped: Option<i64>,
    /// `incomplete` when an endpoint that did not answer may hold threads this listing reads from their files (#14304).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unanswered_endpoints: Option<i64>,
    /// Why the backlog is missing, when this device's store could not be read: never a failed listing (#14431).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlog_error: Option<String>,
    /// The account's devices with their listing state and thread counts (#14305, #14304, #14308, #14295).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<ThreadDevice>>,
    /// Set when the account relay or device roster could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_unavailable: Option<String>,
    /// Set when the account alias registry could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases_unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadReadOk {
    pub thread: String,
    pub link: String,
    pub items: Vec<ThreadTranscriptItem>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub source: ThreadReadSource,
    /// Set on the JSONL fallback for a thread whose endpoint is dead: the file may lag the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_incomplete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSendOk {
    pub thread: String,
    pub link: String,
    pub delivery: ThreadDelivery,
    /// The gateway row's sequence; absent for a session on another device, which has no row here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_seq: Option<i64>,
    pub deduplicated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(flatten)]
    pub gateway: ThreadGatewayDeliveryFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadInterruptBackgroundJob {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadInterruptFailedJob {
    #[serde(flatten)]
    pub job: ThreadInterruptBackgroundJob,
    pub reason: String,
}

/// The background jobs the interrupted turn started: `stopped` ended them, so none of them wakes the thread later;
/// `not_supported` is an engine that predates the stop, where they were left running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ThreadInterruptBackground {
    Stopped {
        stopped: Vec<ThreadInterruptBackgroundJob>,
        failed: Vec<ThreadInterruptFailedJob>,
    },
    NotSupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadInterruptOk {
    pub thread: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub interrupted: bool,
    pub background: ThreadInterruptBackground,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadCloseReason {
    Closed,
    NotLive,
}

/// `closed` is whether this call shut the session down; `reason` is always set: `closed` when it did, `not_live` when
/// nothing live held it. `dropped_deliveries` is always set: the deliveries the close handed back unread (their
/// senders redeliver them), empty when none (omo #14234).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCloseOk {
    pub thread: String,
    pub closed: bool,
    pub reason: ThreadCloseReason,
    pub dropped_deliveries: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graceful: Option<CloseGraceful>,
}

/// `thread_handoff` answers the thread it reached flat, like `thread_create`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadHandoffOk {
    #[serde(flatten)]
    pub thread: ThreadResultSummary,
    pub resolved_by: ThreadAddressResolution,
    pub delivery: ThreadDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_seq: Option<i64>,
    pub deduplicated: bool,
    #[serde(flatten)]
    pub gateway: ThreadGatewayDeliveryFacts,
}

/// `alias_scope: "local"` (#13104): no account could hold the alias (`alias_unavailable` says why), so it is only the
/// thread's name on its device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRenameOk {
    pub thread: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSetModelOk {
    pub thread: String,
    pub model: ThreadModel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSetReasoningOk {
    pub thread: String,
    pub level: String,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// The thread as `thread_list` would list it now, with the state its archived flag gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadArchiveOk {
    #[serde(flatten)]
    pub thread: ThreadResultSummary,
    pub archived: bool,
}

/// `[title](omo-thread://v1/<thread_id>)`, pasted when telling the user about the thread. The label is a session name
/// users and peer agents set, so link syntax, autolink, entity and code-span characters are escaped (upstream
/// `thread-link.ts`); an empty label reads `thread`.
pub fn thread_link(thread_id: &str, title: &str) -> String {
    let collapsed = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = if collapsed.is_empty() {
        "thread"
    } else {
        collapsed.as_str()
    };
    let mut escaped = String::with_capacity(label.len());
    for character in label.chars() {
        if matches!(character, '\\' | '[' | ']' | '<' | '>' | '&' | '`') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    format!("[{escaped}](omo-thread://v1/{thread_id})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_link_names_the_durable_thread() {
        assert_eq!(thread_link("t-1", "Build"), "[Build](omo-thread://v1/t-1)");
        assert_eq!(
            thread_link("t-1", " a]  [b "),
            "[a\\] \\[b](omo-thread://v1/t-1)"
        );
        assert_eq!(thread_link("t-1", "  "), "[thread](omo-thread://v1/t-1)");
    }

    #[test]
    fn an_error_result_serializes_its_failure() {
        let error = ThreadDataError::of(super::super::errors::thread_tool_failure(
            "not_found",
            "gone",
            "call thread_list",
            None,
        ));
        let json = serde_json::to_value(&error).unwrap_or_default();
        assert_eq!(json["kind"], "error");
        assert_eq!(json["error"]["code"], "not_found");
    }

    #[test]
    fn a_close_always_names_its_reason_and_its_dropped_deliveries() {
        let closed = ThreadCloseOk {
            thread: "t-1".into(),
            closed: false,
            reason: ThreadCloseReason::NotLive,
            dropped_deliveries: Vec::new(),
            device: None,
            graceful: None,
        };
        assert_eq!(
            serde_json::to_value(&closed).unwrap_or_default(),
            serde_json::json!({ "thread": "t-1", "closed": false, "reason": "not_live", "dropped_deliveries": [] })
        );
    }

    #[test]
    fn an_interrupt_background_is_tagged_by_its_status() {
        let stopped = ThreadInterruptBackground::Stopped {
            stopped: vec![ThreadInterruptBackgroundJob {
                kind: "bash".into(),
                id: "b-1".into(),
                description: None,
            }],
            failed: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(&stopped).unwrap_or_default(),
            serde_json::json!({ "status": "stopped", "stopped": [{ "kind": "bash", "id": "b-1" }], "failed": [] })
        );
        assert_eq!(
            serde_json::to_value(ThreadInterruptBackground::NotSupported).unwrap_or_default(),
            serde_json::json!({ "status": "not_supported" })
        );
    }
}
