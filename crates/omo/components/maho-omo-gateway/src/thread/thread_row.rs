//! A thread as a tool result shows it.
//!
//! Upstream counterpart: `thread/tools/thread-row.ts` (`threadRow`) and the row builders of `thread/tools/internals.ts`
//! (`summary`, `degradedSummary`, `rowNamed`). The address book entry, when there is one, supplies what the endpoint may
//! not report: the real name (else the first user message's opening, never the durable id), the header's creation time
//! and the last entry's time, and the surface.

use maho_thread_address_core::{NamedRow, row_name};
use serde_json::Value;

use super::address_book::{AddressEntry, HostSessionStatus};
use super::contracts_results::{ThreadResultSummary, ThreadStatus, thread_link};
use super::host_view::ThreadHostSession;
use super::thread_state::{activity_detail, retry_wait, thread_state};

/// A listed thread named by the one rule every row takes (thread-address-core `rowName`): its name, else its first
/// message, else its folder, marked `unnamed`, because only a real name is an address. The link follows the shown name.
fn row_named(
    mut row: ThreadResultSummary,
    name: Option<&str>,
    first_message: Option<&str>,
) -> ThreadResultSummary {
    let shown = row_name(&NamedRow {
        name: name.map(str::to_owned),
        first_message: first_message.map(str::to_owned),
        folder: row.cwd.clone(),
    });
    row.link = thread_link(&row.thread, &shown.name);
    row.name = shown.name;
    row.unnamed = shown.unnamed.then_some(true);
    row
}

/// A live session's row; `now` stands in for a creation time nothing reports.
pub fn live_row(
    session: &ThreadHostSession,
    entry: Option<&AddressEntry>,
    now: &str,
) -> ThreadResultSummary {
    let thread = session.thread_id().to_owned();
    let held =
        session.status != Some(HostSessionStatus::Closed) && entry.is_none_or(|entry| entry.alive);
    let created_at = entry
        .map(|entry| entry.created_at.clone())
        .or_else(|| session.created_at.clone())
        .unwrap_or_else(|| now.to_owned());
    let updated_at = match entry {
        Some(entry) => entry.updated_at.clone(),
        None => session
            .updated_at
            .clone()
            .or_else(|| Some(created_at.clone())),
    };
    let opening = session.status == Some(HostSessionStatus::Opening);
    let settled = held && !opening && !session.archived;
    let detail = activity_detail(
        settled,
        session.activity.as_deref(),
        session.error_code.as_deref(),
        session.turn_id.as_deref(),
    );
    let row = ThreadResultSummary {
        thread,
        name: String::new(),
        unnamed: None,
        status: if held {
            ThreadStatus::Live
        } else {
            ThreadStatus::Resumable
        },
        state: thread_state(session.archived, held, opening, session.activity.as_deref()),
        model: session.model.clone(),
        activity: detail.activity,
        error_code: detail.error_code,
        turn_id: detail.turn_id,
        waiting_on: retry_wait(
            settled && session.activity.as_deref() == Some("running"),
            session.waiting.as_ref(),
        ),
        engine_version: session.engine_version.clone(),
        backlog: None,
        as_of: None,
        machine: None,
        cwd: session.cwd.clone(),
        created_at,
        updated_at,
        link: String::new(),
        device: None,
        alias: None,
        surface: entry
            .and_then(|entry| entry.surface)
            .map(|surface| surface.as_str().to_owned()),
        controls: Some(if opening {
            Vec::new()
        } else {
            session.controls.clone()
        }),
        error_note: None,
        skills: None,
    };
    let name = entry
        .and_then(|entry| entry.name.clone())
        .or_else(|| session.name.clone());
    row_named(
        row,
        name.as_deref(),
        entry.and_then(|entry| entry.title.as_deref()),
    )
}

/// A thread listed from disk because no engine holds it: inactive (or archived), addressed by its durable id.
pub fn degraded_row(entry: &AddressEntry) -> ThreadResultSummary {
    let row = ThreadResultSummary {
        thread: entry.thread_id.clone(),
        name: String::new(),
        unnamed: None,
        status: ThreadStatus::Resumable,
        state: thread_state(entry.archived, false, false, None),
        model: None,
        activity: None,
        error_code: None,
        turn_id: None,
        waiting_on: None,
        engine_version: None,
        backlog: None,
        as_of: None,
        machine: None,
        cwd: entry.cwd.clone(),
        created_at: entry.created_at.clone(),
        updated_at: entry.updated_at.clone(),
        link: String::new(),
        device: None,
        alias: None,
        surface: entry.surface.map(|surface| surface.as_str().to_owned()),
        controls: None,
        error_note: entry.error_note.clone(),
        skills: None,
    };
    row_named(row, entry.name.as_deref(), entry.title.as_deref())
}

/// The `thread_create` answer for a session an open answered (or found live already): flat, `thread` first
/// (`openedResult`).
pub fn opened_result(opened: &ThreadResultSummary, deduplicated: bool) -> Value {
    let mut result = serde_json::Map::new();
    result.insert("kind".to_owned(), Value::String("ok".to_owned()));
    if let Ok(Value::Object(row)) = serde_json::to_value(opened) {
        result.extend(row);
    }
    result.insert("deduplicated".to_owned(), Value::Bool(deduplicated));
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::contracts_state::{ThreadRetryWaitReason, ThreadState};
    use crate::thread::host_view::ThreadHostSession;
    use crate::thread::types::GatewayEndpointKind;

    fn session(activity: &str, waiting: Option<serde_json::Value>) -> ThreadHostSession {
        let mut row = serde_json::json!({
            "sessionId": "rpc-1",
            "durableSessionId": "d-1",
            "cwd": "/work/builder",
            "status": "open",
            "activity": activity,
            "engineVersion": "2026.11.1",
        });
        if let Some(waiting) = waiting {
            row["waiting"] = waiting;
        }
        ThreadHostSession::from_listing(&row, "/run/i-1.sock", GatewayEndpointKind::RpcHost, &[])
            .expect("session")
    }

    fn entry(name: Option<&str>, title: Option<&str>) -> AddressEntry {
        AddressEntry {
            durable_id: "d-1".into(),
            routing_id: None,
            name: name.map(str::to_owned),
            cwd: "/work/builder".into(),
            status: ThreadStatus::Live,
            liveness: ThreadStatus::Live,
            source_host: Some("/run/i-1.sock".into()),
            session_path: None,
            created_at: "2026-11-01T00:00:00.000Z".into(),
            updated_at: Some("2026-11-02T00:00:00.000Z".into()),
            thread_id: "d-1".into(),
            error_note: None,
            endpoint: None,
            surface: None,
            alive: true,
            title: title.map(str::to_owned),
            archived: false,
        }
    }

    #[test]
    fn a_named_thread_is_not_unnamed_and_its_link_carries_the_name() {
        let row = live_row(
            &session("idle", None),
            Some(&entry(Some("Builder"), Some("Builder"))),
            "2026-11-01T00:00:00.000Z",
        );
        assert_eq!(row.name, "Builder");
        assert_eq!(row.unnamed, None);
        assert!(row.link.contains("Builder"));
        assert_eq!(row.engine_version.as_deref(), Some("2026.11.1"));
        assert_eq!(row.state, ThreadState::Idle);
    }

    #[test]
    fn a_session_its_engine_is_still_opening_is_opening_with_no_controls_and_no_activity() {
        let mut opening = session("running", None);
        opening.status = Some(HostSessionStatus::Opening);
        opening.controls = vec!["prompt".to_owned()];
        let row = live_row(&opening, None, "2026-11-01T00:00:00.000Z");
        assert_eq!(row.state, ThreadState::Opening);
        assert_eq!(row.status, ThreadStatus::Live);
        assert_eq!(row.controls, Some(Vec::new()));
        assert_eq!(row.activity, None);
        assert_eq!(row.waiting_on, None);
    }

    #[test]
    fn a_thread_with_no_name_is_named_by_its_folder_and_marked_unnamed() {
        let row = live_row(&session("idle", None), None, "2026-11-01T00:00:00.000Z");
        assert_eq!(row.name, "builder");
        assert_eq!(row.unnamed, Some(true));
        assert!(row.link.contains("builder"));
    }

    #[test]
    fn a_running_thread_asleep_between_retries_reports_its_wait() {
        let waiting = serde_json::json!({
            "kind": "rate_limit",
            "until": "2026-11-02T00:01:00.000Z",
            "attempt": 2,
            "maxAttempts": 5
        });
        let row = live_row(
            &session("running", Some(waiting.clone())),
            None,
            "2026-11-01T00:00:00.000Z",
        );
        let wait = row.waiting_on.expect("wait");
        assert_eq!(wait.reason, ThreadRetryWaitReason::RateLimit);
        assert_eq!(wait.attempt, 2);
        assert_eq!(wait.max_attempts, 5);
        let idle = live_row(
            &session("idle", Some(waiting)),
            None,
            "2026-11-01T00:00:00.000Z",
        );
        assert_eq!(idle.waiting_on, None);
    }

    #[test]
    fn a_thread_listed_from_disk_has_no_wait_and_no_engine_version() {
        let row = degraded_row(&entry(Some("Gone"), Some("Gone")));
        assert_eq!(row.name, "Gone");
        assert_eq!(row.unnamed, None);
        assert_eq!(row.waiting_on, None);
        assert_eq!(row.engine_version, None);
        assert_eq!(row.state, ThreadState::Inactive);
        let unnamed = degraded_row(&entry(None, None));
        assert_eq!(unnamed.name, "builder");
        assert_eq!(unnamed.unnamed, Some(true));

        let mut archived_entry = entry(Some("Archived"), Some("Archived"));
        archived_entry.archived = true;
        let archived = degraded_row(&archived_entry);
        assert_eq!(archived.state, ThreadState::Archived);
        assert_eq!(archived.status, ThreadStatus::Resumable);
    }
}
