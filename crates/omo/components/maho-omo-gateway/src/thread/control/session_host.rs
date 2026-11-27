//! The identity a create gives a new thread, and how an opened session reads as a thread.
//!
//! Upstream counterpart: `thread/control/session-host.ts` (`createdThreadId`, `openedThread`). The `SessionHost` port
//! and `SessionOpenRequest` stay in `port.rs`.

use sha2::{Digest, Sha256};

use crate::thread::address_book::thread_title;
use crate::thread::contracts_results::{ThreadResultSummary, thread_link};
use crate::thread::host_view::ThreadHostSession;
use crate::thread::thread_row::live_row;

/// The id thread_create creates a thread on this device under (#14060): derived from the caller and the call's
/// idempotency key, UUID-shaped (version 8), so a create whose reply was lost, and a retry under the same key, find the
/// thread it made instead of making another.
pub fn created_thread_id(caller_id: &str, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("thread_create\0{caller_id}\0{idempotency_key}").as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    let variant = hex[16..17]
        .chars()
        .next()
        .and_then(|digit| digit.to_digit(16))
        .map_or(8, |digit| (digit & 0x3) | 0x8);
    format!(
        "{}-{}-8{}-{:x}{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        variant,
        &hex[17..20],
        &hex[20..32]
    )
}

/// The engine capability that says `open_session` forks a session on `forkFrom` (#13031).
pub const OPEN_SESSION_FORK: &str = "open_session_fork";

/// One session as the tools report a thread when no address book entry names it (`summary()` without an entry): the
/// name is the session's own, else empty, never a fallback.
pub fn session_summary(session: &ThreadHostSession, now: &str) -> ThreadResultSummary {
    let mut row = live_row(session, None, now);
    row.name = thread_title(session.name.as_deref(), None).unwrap_or_default();
    row.unnamed = None;
    row.link = thread_link(&row.thread, &row.name);
    row
}

/// The one thread thread_create answers for a session it opened: its identity, folder, name, state and times, never
/// the rest of the engine's reply state (its lifecycle, holds, questions, endpoint facts).
pub fn opened_thread(session: &ThreadHostSession, now: &str) -> ThreadResultSummary {
    let mut bare = session.clone();
    bare.archived = false;
    bare.activity = None;
    bare.error_code = None;
    bare.turn_id = None;
    bare.model = None;
    bare.waiting = None;
    bare.engine_version = None;
    bare.controls = Vec::new();
    let mut row = session_summary(&bare, now);
    row.controls = None;
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::address_book::HostSessionStatus;
    use crate::thread::contracts_state::{ThreadModel, ThreadState};
    use crate::thread::types::GatewayEndpointKind;

    fn session(name: Option<&str>) -> ThreadHostSession {
        ThreadHostSession {
            session_id: "rpc-9".into(),
            durable_session_id: Some("dur-9".into()),
            session_path: Some("/sessions/dur-9.jsonl".into()),
            cwd: "/work/api".into(),
            name: name.map(str::to_owned),
            status: Some(HostSessionStatus::Open),
            kind: Some("worker".into()),
            archived: true,
            activity: Some("running".into()),
            error_code: Some("auth".into()),
            turn_id: Some("t-1".into()),
            model: Some(ThreadModel {
                provider: "anthropic".into(),
                id: "opus".into(),
            }),
            waiting: None,
            engine_version: Some("2026.11.1".into()),
            created_at: Some("2026-11-01T00:00:00.000Z".into()),
            updated_at: Some("2026-11-02T00:00:00.000Z".into()),
            endpoint_kind: GatewayEndpointKind::RpcHost,
            controls: vec!["send".into()],
            socket: "/run/host.sock".into(),
        }
    }

    #[test]
    fn a_created_id_is_uuid_shaped_version_8_and_stable_per_caller_and_key() {
        let id = created_thread_id("dur-caller", "create-replay");
        assert_eq!(id, "c40f2318-aa30-83e7-bdcd-c658d794ce48");
        let bytes = id.as_bytes();
        assert_eq!(id.len(), 36);
        for index in [8, 13, 18, 23] {
            assert_eq!(bytes[index], b'-', "{id}");
        }
        assert_eq!(bytes[14], b'8', "{id}");
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'), "{id}");
        assert_eq!(id, created_thread_id("dur-caller", "create-replay"));
        assert_ne!(
            created_thread_id("dur-caller", "a"),
            created_thread_id("dur-caller", "b")
        );
        assert_ne!(
            created_thread_id("dur-caller", "a"),
            created_thread_id("dur-other", "a")
        );
    }

    #[test]
    fn an_opened_session_answers_identity_folder_name_and_times_only() {
        let row = opened_thread(&session(Some("fix the build")), "2026-11-26T00:00:00.000Z");
        assert_eq!(row.thread, "dur-9");
        assert_eq!(row.name, "fix the build");
        assert_eq!(row.cwd, "/work/api");
        assert_eq!(row.created_at, "2026-11-01T00:00:00.000Z");
        assert_eq!(row.updated_at.as_deref(), Some("2026-11-02T00:00:00.000Z"));
        assert_eq!(row.state, ThreadState::Idle);
        assert_eq!(row.unnamed, None);
        assert_eq!(row.model, None);
        assert_eq!(row.activity, None);
        assert_eq!(row.turn_id, None);
        assert_eq!(row.error_code, None);
        assert_eq!(row.engine_version, None);
        assert_eq!(row.controls, None);
        assert!(row.link.starts_with("[fix the build]("), "{}", row.link);
    }

    #[test]
    fn a_session_with_no_name_is_answered_with_an_empty_name_not_a_fallback() {
        let row = opened_thread(&session(None), "2026-11-26T00:00:00.000Z");
        assert_eq!(row.name, "");
        assert_eq!(row.unnamed, None);
        assert!(row.link.starts_with("[thread]("), "{}", row.link);
    }

    #[test]
    fn a_live_thread_keeps_its_state_model_and_controls() {
        let row = session_summary(&session(Some("peer")), "2026-11-26T00:00:00.000Z");
        assert_eq!(row.state, ThreadState::Archived);
        assert_eq!(row.model.map(|model| model.id), Some("opus".to_owned()));
        assert_eq!(row.controls, Some(vec!["send".to_owned()]));
        assert_eq!(row.name, "peer");
    }
}
