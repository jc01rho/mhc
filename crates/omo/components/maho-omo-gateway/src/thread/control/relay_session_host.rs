//! A thread on another device formatted for tool results (`thread/control/relay-session-host.ts`).

use crate::thread::contracts_results::{ThreadResultSummary, thread_link};
use crate::thread::thread_state::retry_wait;

use crate::thread::remote::ports::{
    RemoteSessionRow, RowSource, escape_for_display, iso_from_ms, keyed, row_created_at,
    row_folder, row_name_of, row_state, row_status,
};

/// A remote listing row shaped into a `ThreadResultSummary`, before display escaping (`listedThread`).
pub fn listed_thread(row: &RemoteSessionRow, machine: Option<&str>) -> ThreadResultSummary {
    let thread = maho_thread_address_core::row_key(&keyed(row));
    let shown = row_name_of(row);
    let as_of = (row.source == Some(RowSource::Cache))
        .then_some(row.as_of)
        .flatten()
        .map(iso_from_ms);
    ThreadResultSummary {
        link: thread_link(&thread, &shown.name),
        thread,
        name: shown.name,
        unnamed: shown.unnamed.then_some(true),
        status: row_status(row),
        state: row_state(row),
        model: row.model.clone(),
        activity: None,
        error_code: None,
        turn_id: None,
        waiting_on: retry_wait(row.state == "running", row.waiting.as_ref()),
        engine_version: row.engine_version.clone(),
        backlog: None,
        as_of,
        machine: machine.map(str::to_owned),
        cwd: row_folder(row).to_owned(),
        created_at: row_created_at(row),
        updated_at: Some(iso_from_ms(row.updated_ms)),
        device: Some(row.device.clone()),
        alias: None,
        surface: None,
        controls: None,
        error_note: None,
        skills: None,
    }
}

/// A remote listing row with display-unsafe characters escaped (`displayedThread`, #13304, #14305).
pub fn displayed_thread(row: &RemoteSessionRow, machine: Option<&str>) -> ThreadResultSummary {
    let mut listed = listed_thread(row, machine);
    let name = escape_for_display(&listed.name);
    listed.cwd = escape_for_display(&listed.cwd);
    listed.machine = listed.machine.map(|label| escape_for_display(&label));
    listed.link = thread_link(&listed.thread, &name);
    listed.name = name;
    listed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::contracts_results::ThreadStatus;
    use crate::thread::contracts_state::ThreadState;

    #[test]
    fn displayed_thread_uses_row_identity_and_escapes_untrusted_text() {
        let row = RemoteSessionRow {
            device: "b".to_owned(),
            session_id: "rk-1".to_owned(),
            durable_id: None,
            title: "p\u{202e}q\nr\x1b[31ms\u{200d}t".to_owned(),
            project: "/work/proj".to_owned(),
            path: "/work/a\nb".to_owned(),
            state: "idle".to_owned(),
            updated_ms: 1_700_000_000_000,
            ..RemoteSessionRow::default()
        };
        let displayed = displayed_thread(&row, Some("Mac\u{202e}Book"));
        assert_eq!(displayed.thread, "b/rk-1");
        assert!(!displayed.name.contains('\n'));
        assert!(displayed.name.contains("\\u202e"));
        assert_eq!(displayed.cwd, "/work/a\\u000ab");
        assert_eq!(displayed.machine.as_deref(), Some("Mac\\u202eBook"));
        assert!(displayed.link.contains("omo-thread://v1/b/rk-1"));
        assert!(!displayed.link.contains('\n'));
        assert_eq!(displayed.status, ThreadStatus::Live);
        assert_eq!(displayed.state, ThreadState::Idle);
    }

    #[test]
    fn cached_row_reports_inactive_status_and_as_of_iso() {
        let row = RemoteSessionRow {
            device: "b".to_owned(),
            session_id: "rk-1".to_owned(),
            durable_id: Some("01932bf3-0280-7abc-8def-0123456789ab".to_owned()),
            title: "cached".to_owned(),
            project: "/work/proj".to_owned(),
            path: "unknown".to_owned(),
            state: "running".to_owned(),
            updated_ms: 1_700_000_000_000,
            source: Some(RowSource::Cache),
            as_of: Some(1_700_000_050_000),
            ..RemoteSessionRow::default()
        };
        let displayed = displayed_thread(&row, None);
        assert_eq!(displayed.thread, "01932bf3-0280-7abc-8def-0123456789ab");
        assert_eq!(displayed.status, ThreadStatus::Resumable);
        assert_eq!(displayed.state, ThreadState::Inactive);
        assert_eq!(displayed.cwd, "/work/proj");
        assert_eq!(displayed.as_of.as_deref(), Some("2023-11-14T22:14:10.000Z"));
    }
}
