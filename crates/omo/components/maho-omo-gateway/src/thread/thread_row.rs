//! A thread as a tool result shows it.
//!
//! Upstream counterpart: `thread/tools/thread-row.ts` (`threadRow`) and the row builders of `thread/tools/internals.ts`
//! (`summary`, `degradedSummary`). The address book entry, when there is one, supplies what the endpoint may not report:
//! the real name (else the first user message's opening, never the durable id), the header's creation time and the last
//! entry's time, and the surface.

use super::address_book::{AddressEntry, HostSessionStatus, thread_title};
use super::contracts_results::{ThreadResultSummary, ThreadStatus, thread_link};
use super::host_view::ThreadHostSession;
use super::thread_state::{activity_detail, thread_state};

/// A live session's row; `now` stands in for a creation time nothing reports.
pub fn live_row(
    session: &ThreadHostSession,
    entry: Option<&AddressEntry>,
    now: &str,
) -> ThreadResultSummary {
    let thread = session.thread_id().to_owned();
    let held =
        session.status != Some(HostSessionStatus::Closed) && entry.is_none_or(|entry| entry.alive);
    let title = entry
        .and_then(|entry| entry.title.clone())
        .or_else(|| thread_title(session.name.as_deref(), None));
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
    let detail = activity_detail(
        held && !session.archived,
        session.activity.as_deref(),
        session.error_code.as_deref(),
        session.turn_id.as_deref(),
    );
    ThreadResultSummary {
        link: thread_link(&thread, title.as_deref().unwrap_or_default()),
        thread,
        name: title.unwrap_or_default(),
        status: if held {
            ThreadStatus::Live
        } else {
            ThreadStatus::Resumable
        },
        state: thread_state(session.archived, held, session.activity.as_deref()),
        model: session.model.clone(),
        activity: detail.activity,
        error_code: detail.error_code,
        turn_id: detail.turn_id,
        as_of: None,
        machine: None,
        cwd: session.cwd.clone(),
        created_at,
        updated_at,
        device: None,
        alias: None,
        surface: entry
            .and_then(|entry| entry.surface)
            .map(|surface| surface.as_str().to_owned()),
        controls: Some(session.controls.clone()),
        error_note: None,
        skills: None,
    }
}

/// A thread listed from disk because no engine holds it: inactive, addressed by its durable id.
pub fn degraded_row(entry: &AddressEntry) -> ThreadResultSummary {
    let title = entry.title.clone().unwrap_or_default();
    ThreadResultSummary {
        thread: entry.thread_id.clone(),
        link: thread_link(&entry.thread_id, &title),
        name: title,
        status: ThreadStatus::Resumable,
        state: thread_state(false, false, None),
        model: None,
        activity: None,
        error_code: None,
        turn_id: None,
        as_of: None,
        machine: None,
        cwd: entry.cwd.clone(),
        created_at: entry.created_at.clone(),
        updated_at: entry.updated_at.clone(),
        device: None,
        alias: None,
        surface: entry.surface.map(|surface| surface.as_str().to_owned()),
        controls: None,
        error_note: entry.error_note.clone(),
        skills: None,
    }
}
