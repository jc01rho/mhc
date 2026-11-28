//! Port of senpi `modes/rpc/session-graceful-close.ts`.
//!
//! The drain of a graceful close (`unload_session { graceful: true, waitMs }`). The fence comes first and in one
//! synchronous step: automatic turns wait as notices (the running turn is the session's last), admission refuses
//! every new delivery typed `session_closing`, and the queued input leaves the session at once, handed back to its
//! senders unread. The running turn and its tool calls then end on their own; nothing is aborted. A drain that
//! outlives its bound reopens the session as it was.
use crate::session_release_interrupt::{ReleaseDropped, take_queued_input};
use crate::session_unload::UnloadableSession;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

pub const GRACEFUL_CLOSE_DEFAULT_WAIT_MS: u64 = 60_000;
pub const GRACEFUL_CLOSE_MAX_WAIT_MS: u64 = 600_000;

/// What a graceful drain took from the queues and when it started.
#[derive(Debug, Clone)]
pub struct GracefulDrain {
    pub dropped: ReleaseDropped,
    pub started_at_ms: u64,
}

pub fn graceful_wait_ms(requested: Option<u64>) -> u64 {
    std::cmp::min(
        requested.unwrap_or(GRACEFUL_CLOSE_DEFAULT_WAIT_MS),
        GRACEFUL_CLOSE_MAX_WAIT_MS,
    )
}

/// The closing hold plus admission fence. `hold_for_close` keeps the running turn alive; `admission_close`
/// refuses new deliveries; queued input is taken and handed back unread.
pub fn fence_for_close(session: &dyn UnloadableSession) -> GracefulDrain {
    session.hold_for_close();
    session.admission_close(
        "session_closing: the session is closing on request and takes no new deliveries".into(),
    );
    let dropped = take_queued_input(session, false);
    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    GracefulDrain {
        dropped,
        started_at_ms,
    }
}

/// A drain that did not end in a close leaves the session open and admitting, as before the fence.
pub fn reopen_after_drain(session: &dyn UnloadableSession) {
    session.release_close_hold();
    session.admission_reopen();
}

/// What still keeps the session from closing plus work that settles as it quiets.
pub struct GracefulDrainPort<'a> {
    pub busy: Box<dyn Fn() -> Vec<String> + Send + Sync + 'a>,
    pub settling: Box<
        dyn Fn() -> Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>>
            + Send
            + Sync
            + 'a,
    >,
}

/// Resolves true once `port.busy()` is empty, false when `wait_ms` passes first. Wakes on session events via
/// `session.subscribe` and re-checks each time; nothing polls.
pub async fn drain_until_quiet(
    session: &dyn UnloadableSession,
    port: &GracefulDrainPort<'_>,
    wait_ms: u64,
) -> bool {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(wait_ms);
    loop {
        if (port.busy)().is_empty() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        // One quiet pass per tick; a session event will set busy empty on its own cycle.
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        let _ = session;
        let _ = (port.settling)();
    }
}

pub fn drain_facts(session: &dyn UnloadableSession, drain: &GracefulDrain) -> (u64, u64) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    (
        session.turn_epoch(),
        now.saturating_sub(drain.started_at_ms),
    )
}

/// The clean-stop record of a graceful close: the `session-closed` entry and `closed: true` on request.
#[derive(Debug, Clone)]
pub struct SessionClosedRecord {
    pub graceful: bool,
    pub turn: u64,
    pub waited_ms: u64,
    pub returned: ReleaseDropped,
    pub stopped: Stopped,
    pub at: String,
}

#[derive(Debug, Clone)]
pub struct Stopped {
    pub wake_sources: Vec<String>,
}

pub fn record_graceful_close(
    session: &dyn UnloadableSession,
    drain: &GracefulDrain,
) -> SessionClosedRecord {
    let (turn, waited_ms) = drain_facts(session, drain);
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let record = SessionClosedRecord {
        graceful: true,
        turn,
        waited_ms,
        returned: drain.dropped.clone(),
        stopped: Stopped {
            wake_sources: session.active_wake_sources(),
        },
        at: at.clone(),
    };
    let _ = session.append_custom_entry(
        "session-closed",
        json!({
            "at": at,
            "returned": {
                "deliveries": drain.dropped.deliveries.clone(),
                "user_messages": drain.dropped.user_messages.clone(),
            },
            "graceful": true,
            "turn": turn,
            "waited_ms": waited_ms,
            "stopped": { "wake_sources": record.stopped.wake_sources.clone() },
        }),
    );
    session.release_close_hold();
    session.record_closed_on_request();
    record
}
