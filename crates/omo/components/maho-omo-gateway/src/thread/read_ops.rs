//! `thread_list` and `thread_read` over one host view.
//!
//! Upstream counterpart: `thread/tools/read-ops.ts`. The default scope is the caller's workspace, the same test a send
//! applies; the address book itself spans every workspace the host knows. A session no engine holds is still a thread,
//! listed `inactive` from its session file: one whose endpoint stopped answering (with the endpoint's failure in
//! `error_note`), and one whose session file is in the scanned session directories after its engine closed it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::address_book::{
    DiskSession, find_disk_sessions, read_disk_session, session_directory_prefix,
};
use super::addressing::{to_thread_address_entries, workspace_directories, workspace_entries};
use super::backlog::TargetBacklog;
use super::contracts_results::{
    ThreadBacklog, ThreadListOk, ThreadReadOk, ThreadResultSummary, ThreadStatus,
    ThreadTranscriptItem, thread_link,
};
use super::contracts_state::ThreadState;
use super::control::locate::{LocatedControl, ThreadCall, book_of, locate_control};
use super::control::operations::{error_json, ok_json};
use super::control::port::{ControlResult, control_identity};
use super::errors::thread_tool_failure;
use super::host_view::ThreadHostView;
use super::read_item::read_item;
use super::reader::{ReadSource, read_transcript};
use super::store_api::GatewayStoreApi;
use super::store_ops::rfc3339;
use super::thread_row::{degraded_row, live_row};
use super::thread_state::listed_states;

/// The most session files one list reads, newest first; older inactive threads are left out and counted instead.
pub const LIST_DISK_LIMIT: usize = 200;
/// The most session files one list stats to rank by modification time (`disk-list.ts`).
pub const LIST_DISK_MAX_STATS: usize = 1000;

/// The session files one listing read, and the durable ids of the files in scope it did not read (a session file is
/// named `<creation time>_<id>.jsonl`, so the id is known without reading it).
pub struct ScannedSessions {
    pub sessions: Vec<DiskSession>,
    pub unread: Vec<String>,
}

fn file_durable_id(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    stem.rsplit('_').next().unwrap_or_default().to_owned()
}

/// The session files in the caller's workspace directories (every directory with `all_scope`), newest by modification
/// time first (ranking the newest by name up to `LIST_DISK_MAX_STATS`), and the durable ids of those past `LIST_DISK_LIMIT`.
pub fn scan_listed_sessions(
    sessions_directory: &Path,
    all_scope: bool,
    caller_workspace_root: &str,
) -> ScannedSessions {
    let Ok(dir_entries) = std::fs::read_dir(sessions_directory) else {
        return ScannedSessions {
            sessions: Vec::new(),
            unread: Vec::new(),
        };
    };
    let prefixes: Vec<String> = workspace_directories(caller_workspace_root)
        .iter()
        .map(|root| session_directory_prefix(root.as_str()))
        .collect();
    let mut paths: Vec<PathBuf> = dir_entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
        })
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.starts_with("--") && name.ends_with("--"))
        .filter(|name| {
            all_scope
                || prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix.as_str()))
        })
        .flat_map(|dir| {
            std::fs::read_dir(sessions_directory.join(dir))
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        })
        .collect();
    paths.sort_by(|left, right| {
        let left_name = left
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let right_name = right
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        right_name.cmp(left_name).then_with(|| left.cmp(right))
    });
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = paths
        .iter()
        .take(LIST_DISK_MAX_STATS)
        .filter_map(|path| {
            let meta = std::fs::metadata(path).ok()?;
            if !meta.is_file() {
                return None;
            }
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            Some((path.clone(), modified))
        })
        .collect();
    files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let read_count = files.len().min(LIST_DISK_LIMIT);
    let read_files = &files[..read_count];
    let read_paths: BTreeSet<&Path> = read_files.iter().map(|(path, _)| path.as_path()).collect();
    let sessions: Vec<DiskSession> = read_files
        .iter()
        .filter_map(|(path, _)| read_disk_session(path, None))
        .collect();
    let unread: Vec<String> = paths
        .iter()
        .map(PathBuf::as_path)
        .filter(|path| !read_paths.contains(path))
        .map(file_durable_id)
        .filter(|id| !id.is_empty())
        .collect();
    ScannedSessions { sessions, unread }
}

/// `thread_list`: the live sessions in scope with their listing facts, then the inactive threads the book holds, in
/// the address book's order (newest first), filtered by `states` (every state but `archived` by default).
pub fn list_threads(
    view: &ThreadHostView,
    now: &str,
    caller_workspace_root: &str,
    scanned: &ScannedSessions,
    all_scope: bool,
    states: Option<&[ThreadState]>,
) -> ThreadListOk {
    let book = book_of(view, &scanned.sessions, now);
    let scoped: BTreeSet<String> =
        workspace_entries(&to_thread_address_entries(&book), caller_workspace_root)
            .into_iter()
            .map(|entry| entry.thread_id)
            .collect();
    let in_scope = |thread_id: &str| all_scope || scoped.contains(thread_id);
    let order: BTreeMap<&str, usize> = book
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.thread_id.as_str(), index))
        .collect();
    let scanned_ids: BTreeSet<&str> = scanned
        .sessions
        .iter()
        .map(|session| session.durable_id.as_str())
        .collect();
    let mut rows: Vec<(usize, ThreadResultSummary)> = Vec::new();
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    for session in &view.sessions {
        let thread_id = session.thread_id();
        if !in_scope(thread_id) {
            continue;
        }
        listed.insert(thread_id);
        let entry = book.iter().find(|entry| {
            entry.thread_id == thread_id
                && entry
                    .source_host
                    .as_deref()
                    .is_none_or(|socket| socket == session.socket)
        });
        rows.push((
            order.get(thread_id).copied().unwrap_or(usize::MAX),
            live_row(session, entry, now),
        ));
    }
    for entry in &book {
        let id = entry.thread_id.as_str();
        if entry.status != ThreadStatus::Live
            && (entry.error_note.is_some() || scanned_ids.contains(id))
            && !listed.contains(id)
            && in_scope(id)
        {
            rows.push((
                order.get(id).copied().unwrap_or(usize::MAX),
                degraded_row(entry),
            ));
        }
    }
    let shown = listed_states(states);
    rows.retain(|(_, row)| shown.contains(&row.state));
    rows.sort_by_key(|(index, _)| *index);
    let threads: Vec<ThreadResultSummary> = rows.into_iter().map(|(_, row)| row).collect();
    let row_ids: BTreeSet<&str> = threads.iter().map(|row| row.thread.as_str()).collect();
    // An unread file's thread is inactive or archived (only its file knows which), so a filter showing either is
    // partial: how many of those threads this listing dropped.
    let dropped =
        if shown.contains(&ThreadState::Inactive) || shown.contains(&ThreadState::Archived) {
            scanned
                .unread
                .iter()
                .filter(|id| !listed.contains(id.as_str()) && !row_ids.contains(id.as_str()))
                .count()
        } else {
            0
        };
    // An endpoint that did not answer (timed out, an auth or protocol failure) may hold threads this listing reads from
    // their files as inactive; one known dead (its socket gone or refusing) holds none, so only the former makes the
    // listing partial.
    let unanswered = view
        .hosts
        .iter()
        .filter(|host| {
            let failed = host
                .list_sessions
                .as_ref()
                .is_none_or(|listing| listing.error.is_some());
            (failed || host.alive == Some(false)) && host.reason.as_deref() != Some("dead")
        })
        .count();
    let dropped = i64::try_from(dropped).unwrap_or(i64::MAX);
    let unanswered = i64::try_from(unanswered).unwrap_or(i64::MAX);
    ThreadListOk {
        threads,
        scope: if all_scope { "all" } else { "workspace" }.to_owned(),
        inactive_truncated: (dropped > 0).then_some(true),
        inactive_dropped: (dropped > 0).then_some(dropped),
        listing: (unanswered > 0).then(|| "incomplete".to_owned()),
        unanswered_endpoints: (unanswered > 0).then_some(unanswered),
        backlog_error: None,
        devices: None,
        remote_unavailable: None,
        aliases_unavailable: None,
    }
}

/// Names each listed thread's undelivered messages from this device's gateway store: how many, where, since when. The
/// backlog is an optional detail, so a store that cannot be read (a newer schema, a worker that failed to start) never
/// costs the listing, which is how a stuck gateway is diagnosed: the listing names why the backlog is missing.
pub fn with_backlog(store: &dyn GatewayStoreApi, mut listed: ThreadListOk) -> ThreadListOk {
    if listed.threads.is_empty() {
        return listed;
    }
    let targets: Vec<String> = listed
        .threads
        .iter()
        .map(|thread| thread.thread.clone())
        .collect();
    let backlogs = match store.backlogs(&targets) {
        Ok(backlogs) => backlogs,
        Err(error) => {
            listed.backlog_error = Some(error.message());
            return listed;
        }
    };
    for thread in &mut listed.threads {
        let Some(found) = backlogs.get(&thread.thread) else {
            continue;
        };
        thread.backlog = backlog_of(found);
    }
    listed
}

/// One thread's backlog as a listing row shows it; a thread holding nothing (or nothing with a time) shows none.
fn backlog_of(found: &TargetBacklog) -> Option<ThreadBacklog> {
    Some(ThreadBacklog {
        held: found.held,
        held_by_session: found.held_by_session,
        waiting_in_gateway: found.waiting_in_gateway,
        oldest_at: rfc3339(found.oldest_at?),
    })
}

/// One role vocabulary for both read paths: engine `toolResult` is `tool`; every other non-chat kind is `system`.
pub fn transcript_role(role: Option<&Value>) -> &'static str {
    match role.and_then(Value::as_str) {
        Some("user") => "user",
        Some("assistant") => "assistant",
        Some("toolResult") => "tool",
        _ => "system",
    }
}

fn items_of(messages: &[Value]) -> Vec<ThreadTranscriptItem> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| read_item(message, index))
        .collect()
}

/// A thread with no live owner, read from its session file (a closed thread, or one whose endpoint is dead, which adds
/// the endpoint's failure as `error_note`). Only a thread with no file here is `not_resumable`.
fn read_degraded(
    call: &ThreadCall,
    thread_id: &str,
    max_bytes: Option<u64>,
    cursor: Option<&str>,
) -> Value {
    let entry = call.book.iter().find(|entry| entry.thread_id == thread_id);
    let error_note = entry.and_then(|entry| entry.error_note.clone());
    let title = entry
        .and_then(|entry| entry.title.clone())
        .unwrap_or_default();
    let path = entry
        .and_then(|entry| entry.session_path.clone())
        .or_else(|| {
            let directory = call.sessions_directory.as_ref()?;
            find_disk_sessions(directory, thread_id, true, &[])
                .into_iter()
                .next()
                .map(|session| session.session_path)
        });
    let Some(path) = path else {
        return error_json(thread_tool_failure(
            "not_resumable",
            "The thread has no live owner.",
            "Retry when the target is live.",
            None,
        ));
    };
    let slice = match read_transcript(
        ReadSource::Jsonl {
            path: Path::new(&path),
            live_host_present: false,
        },
        max_bytes,
        cursor,
    ) {
        Ok(slice) => slice,
        Err(failure) => return error_json(*failure),
    };
    // A session file interleaves messages with bookkeeping (the header, model and thinking changes, names): only
    // message entries are transcript, rendered with the live path's roles.
    let messages: Vec<Value> = slice
        .items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("message"))
        .filter(|message| {
            matches!(
                message.get("role").and_then(Value::as_str),
                Some("user" | "assistant" | "toolResult")
            )
        })
        .cloned()
        .collect();
    ok_json(&ThreadReadOk {
        thread: thread_id.to_owned(),
        link: thread_link(thread_id, &title),
        items: items_of(&messages),
        truncated: slice.truncated,
        next_cursor: slice.next_cursor,
        source: slice.source,
        source_incomplete: Some(slice.source_incomplete),
        error_note,
        device: None,
    })
}

/// `thread_read`: the live transcript through the thread's control; else its session file when no engine holds it.
pub async fn read_thread(
    call: &ThreadCall,
    thread: &str,
    all_scope: bool,
    max_bytes: Option<u64>,
    cursor: Option<&str>,
) -> ControlResult<Value> {
    let control = match locate_control(call, thread, all_scope).await {
        LocatedControl::Error(failure) => return Ok(error_json(failure)),
        LocatedControl::NotLive { thread_id, .. } => {
            return Ok(read_degraded(call, &thread_id, max_bytes, cursor));
        }
        LocatedControl::Control(control) => control,
    };
    let messages = control.get_messages().await?;
    let slice = match read_transcript(ReadSource::Live(&messages), max_bytes, cursor) {
        Ok(slice) => slice,
        Err(failure) => return Ok(error_json(*failure)),
    };
    let (thread_id, device) = control_identity(control.as_ref());
    Ok(ok_json(&ThreadReadOk {
        link: thread_link(&thread_id, control.title()),
        thread: thread_id,
        items: items_of(&slice.items),
        truncated: slice.truncated,
        next_cursor: slice.next_cursor,
        source: slice.source,
        source_incomplete: None,
        error_note: None,
        device,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::address_book::{AddressBookHost, HostListSessions};
    use crate::thread::host_view::{ThreadHostSession, ThreadHostView};
    use crate::thread::types::GatewayEndpointKind;

    fn session(id: &str) -> ThreadHostSession {
        ThreadHostSession::from_listing(
            &serde_json::json!({
                "sessionId": id,
                "durableSessionId": id,
                "cwd": "/work/builder",
                "status": "open",
                "activity": "idle",
            }),
            "/run/i-1.sock",
            GatewayEndpointKind::RpcHost,
            &[],
        )
        .expect("session")
    }

    fn host(
        listing: Option<HostListSessions>,
        alive: Option<bool>,
        reason: &str,
    ) -> AddressBookHost {
        AddressBookHost {
            socket: "/run/i-1.sock".into(),
            list_sessions: listing,
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive,
            reason: Some(reason.to_owned()),
            legacy: false,
        }
    }

    fn none() -> ScannedSessions {
        ScannedSessions {
            sessions: Vec::new(),
            unread: Vec::new(),
        }
    }

    const NOW: &str = "2026-11-02T00:00:00.000Z";
    const ROOT: &str = "/work";

    #[test]
    fn an_unanswered_endpoint_makes_the_listing_incomplete_but_a_dead_one_does_not() {
        let view = ThreadHostView {
            sessions: vec![session("d-1")],
            hosts: vec![host(
                Some(HostListSessions::failed("timeout")),
                None,
                "timeout",
            )],
            disk: Vec::new(),
        };
        let ok = list_threads(&view, NOW, ROOT, &none(), false, None);
        assert_eq!(ok.listing.as_deref(), Some("incomplete"));
        assert_eq!(ok.unanswered_endpoints, Some(1));

        let dead = ThreadHostView {
            sessions: vec![session("d-1")],
            hosts: vec![host(None, Some(false), "dead")],
            disk: Vec::new(),
        };
        let ok = list_threads(&dead, NOW, ROOT, &none(), false, None);
        assert_eq!(ok.listing, None);
        assert_eq!(ok.unanswered_endpoints, None);
    }

    #[test]
    fn unread_session_files_are_counted_only_when_the_filter_shows_inactive_or_archived() {
        let view = ThreadHostView {
            sessions: vec![session("d-1")],
            hosts: vec![host(Some(HostListSessions::ok(Vec::new())), Some(true), "")],
            disk: Vec::new(),
        };
        let scanned = ScannedSessions {
            sessions: Vec::new(),
            unread: vec!["d-2".into(), "d-3".into()],
        };
        let dropped = list_threads(
            &view,
            NOW,
            ROOT,
            &scanned,
            false,
            Some(&[ThreadState::Inactive]),
        );
        assert_eq!(dropped.inactive_dropped, Some(2));
        assert_eq!(dropped.inactive_truncated, Some(true));
        let listable = list_threads(
            &view,
            NOW,
            ROOT,
            &scanned,
            false,
            Some(&[ThreadState::Idle]),
        );
        assert_eq!(listable.inactive_dropped, None);
        assert_eq!(listable.inactive_truncated, None);
    }

    #[test]
    fn a_backlog_with_no_oldest_time_is_not_shown() {
        assert_eq!(backlog_of(&TargetBacklog::default()), None);
        let shown = backlog_of(&TargetBacklog {
            held: 3,
            held_bytes: 30,
            held_by_session: 1,
            waiting_in_gateway: 2,
            oldest_at: Some(0),
            frees_at: None,
        })
        .expect("backlog");
        assert_eq!(shown.held, 3);
        assert_eq!(shown.waiting_in_gateway, 2);
        assert_eq!(shown.oldest_at, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn with_backlog_attaches_backlogs_and_records_store_error_without_failing_listing() {
        use std::sync::Arc;

        use crate::gateway_database_path;
        use crate::thread::schema::GATEWAY_MIGRATIONS;
        use crate::thread::store::{GatewayStoreOptions, create_gateway_store};
        use crate::thread::types::{
            EnqueueOutcome, EnqueueRequest, EnvelopeOrigin, GatewayDeliveryMode, Provenance,
            ReceiptRef,
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let mut store_opts = GatewayStoreOptions::new(dir.path().to_string_lossy().into_owned());
        store_opts.now = Some(Arc::new(|| 1_000));
        let store = create_gateway_store(store_opts).expect("store");
        let sent = store
            .enqueue(&EnqueueRequest {
                provenance: Provenance {
                    workspace: "w".into(),
                    project: "p".into(),
                    lane: None,
                    session: None,
                },
                now: 1_000,
                delivery_id: "d-msg-1".into(),
                target_durable_id: "d-1".into(),
                sender_principal: "session:sender".into(),
                rate_principal: None,
                sender_node: "sender".into(),
                sender_turn: None,
                cause_delivery_id: None,
                claimed_root_id: None,
                origin: EnvelopeOrigin::Session {
                    session: "sender".into(),
                },
                actor: "sender".into(),
                body: "hello".into(),
                mode: GatewayDeliveryMode::FollowUp,
                expected_turn_id: None,
                binding: None,
                receipt: ReceiptRef {
                    idempotency_key: "d-msg-1".into(),
                    args_hash: "d-msg-1".into(),
                },
                endpoint_kind: None,
                collapse_key: None,
            })
            .expect("enqueue");
        assert!(matches!(sent, EnqueueOutcome::Inserted { .. }));

        let sessions = vec![session("d-1"), session("d-quiet")];
        let view = ThreadHostView {
            hosts: vec![host(
                Some(HostListSessions::ok(
                    sessions
                        .iter()
                        .map(ThreadHostSession::host_session)
                        .collect(),
                )),
                Some(true),
                "",
            )],
            sessions,
            disk: Vec::new(),
        };
        let listed = list_threads(&view, NOW, "/work/builder", &none(), false, None);
        let with = with_backlog(store.as_ref(), listed);
        assert_eq!(with.backlog_error, None);
        let row1 = with
            .threads
            .iter()
            .find(|t| t.thread == "d-1")
            .expect("d-1 row");
        assert_eq!(
            row1.backlog,
            Some(ThreadBacklog {
                held: 1,
                held_by_session: 0,
                waiting_in_gateway: 1,
                oldest_at: "1970-01-01T00:00:01.000Z".into(),
            })
        );
        let quiet = with
            .threads
            .iter()
            .find(|t| t.thread == "d-quiet")
            .expect("d-quiet row");
        assert_eq!(quiet.backlog, None);

        // A store another binary migrated past this one sets backlog_error and preserves the listing.
        let other = rusqlite::Connection::open(gateway_database_path(dir.path()))
            .expect("newer binary connection");
        other
            .execute_batch(&format!(
                "DELETE FROM gateway_meta WHERE key = 'reader_floor'; PRAGMA user_version = {}",
                GATEWAY_MIGRATIONS.len() + 1
            ))
            .expect("bump schema");
        let empty = with_backlog(
            store.as_ref(),
            ThreadListOk {
                threads: Vec::new(),
                scope: "workspace".into(),
                inactive_truncated: None,
                inactive_dropped: None,
                listing: None,
                unanswered_endpoints: None,
                backlog_error: None,
                devices: None,
                remote_unavailable: None,
                aliases_unavailable: None,
            },
        );
        assert_eq!(empty.backlog_error, None);

        let broken = with_backlog(
            store.as_ref(),
            list_threads(&view, NOW, "/work/builder", &none(), false, None),
        );
        assert_eq!(broken.threads.len(), 2);
        assert!(
            broken
                .backlog_error
                .as_deref()
                .is_some_and(|message| message.contains("newer"))
        );
    }

    #[test]
    fn scan_listed_sessions_ranks_by_mtime_and_counts_unread_for_inactive_and_archived() {
        use std::io::Write;

        let dir = tempfile::tempdir().expect("tempdir");
        let sessions_dir = dir.path().join("sessions");
        let project = sessions_dir.join("--work--");
        std::fs::create_dir_all(&project).expect("create dir");
        let mut ids = Vec::new();
        for index in 0..203usize {
            let id = format!("0195f0c2-1a2b-7c3d-8e4f-{index:012x}");
            let created = format!("2026-01-01T00-{:02}-{:02}-000Z", index / 60, index % 60);
            let file_path = project.join(format!("{created}_{id}.jsonl"));
            let mut file = std::fs::File::create(&file_path).expect("create");
            let header = serde_json::json!({
                "type": "session",
                "version": 3,
                "id": id,
                "cwd": "/work",
                "timestamp": "2026-01-01T00:00:00.000Z",
            });
            writeln!(file, "{header}").expect("write");
            if index == 202 {
                let archived = serde_json::json!({
                    "type": "custom",
                    "customType": "session_archived",
                    "data": { "archived": true },
                    "timestamp": "2026-01-01T00:05:00.000Z",
                });
                writeln!(file, "{archived}").expect("write archived");
            }
            let modified = std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(1_800_000_000 + index as u64);
            file.set_modified(modified).expect("set_modified");
            ids.push(id);
        }

        let scanned = scan_listed_sessions(&sessions_dir, true, ROOT);
        assert_eq!(scanned.sessions.len(), 200);
        assert_eq!(scanned.unread.len(), 3);
        assert!(scanned.unread.contains(&ids[0]));
        assert!(scanned.unread.contains(&ids[1]));
        assert!(scanned.unread.contains(&ids[2]));

        // The oldest unread file (ids[0]) is running live, so only 2 unread files are inactive/archived.
        let view = ThreadHostView {
            sessions: vec![session(&ids[0])],
            hosts: Vec::new(),
            disk: Vec::new(),
        };
        let default_listed = list_threads(&view, NOW, ROOT, &scanned, true, None);
        assert_eq!(default_listed.inactive_truncated, Some(true));
        assert_eq!(default_listed.inactive_dropped, Some(2));
        assert!(
            default_listed
                .threads
                .iter()
                .any(|row| row.thread == ids[0])
        );
        assert!(
            !default_listed
                .threads
                .iter()
                .any(|row| row.thread == ids[202])
        );

        let archived_listed = list_threads(
            &view,
            NOW,
            ROOT,
            &scanned,
            true,
            Some(&[ThreadState::Archived]),
        );
        assert_eq!(archived_listed.inactive_truncated, Some(true));
        assert_eq!(archived_listed.inactive_dropped, Some(2));
        assert_eq!(archived_listed.threads.len(), 1);
        assert_eq!(archived_listed.threads[0].thread, ids[202]);
        assert_eq!(archived_listed.threads[0].state, ThreadState::Archived);

        let live_only = list_threads(
            &view,
            NOW,
            ROOT,
            &scanned,
            true,
            Some(&[ThreadState::Running, ThreadState::Idle]),
        );
        assert_eq!(live_only.inactive_dropped, None);
        assert_eq!(live_only.inactive_truncated, None);
    }

    #[test]
    fn list_threads_keeps_live_unresponsive_note_when_workspace_scan_also_sees_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions_dir = dir.path().join("sessions");
        let project = sessions_dir.join("--work--");
        std::fs::create_dir_all(&project).expect("mkdir");
        let file = project.join("2026-11-01T00-00-00-000Z_d-unresp.jsonl");
        let header = serde_json::json!({
            "type": "session",
            "version": 3,
            "id": "d-unresp",
            "cwd": "/work",
            "timestamp": "2026-11-01T00:00:00.000Z",
        });
        std::fs::write(&file, format!("{header}\n")).expect("write");

        let socket = "/run/t-slow.sock";
        let claimed = read_disk_session(&file, Some(socket)).expect("claimed");
        let scanned = scan_listed_sessions(&sessions_dir, true, ROOT);
        let view = ThreadHostView {
            sessions: Vec::new(),
            hosts: vec![AddressBookHost {
                socket: socket.to_owned(),
                list_sessions: None,
                endpoint_kind: Some(GatewayEndpointKind::Tui),
                alive: Some(false),
                reason: Some("live_unresponsive".into()),
                legacy: false,
            }],
            disk: vec![claimed],
        };
        let listed = list_threads(&view, NOW, ROOT, &scanned, true, None);
        assert_eq!(listed.listing.as_deref(), Some("incomplete"));
        assert_eq!(listed.unanswered_endpoints, Some(1));
        assert_eq!(listed.threads.len(), 1);
        assert_eq!(listed.threads[0].thread, "d-unresp");
        assert_eq!(
            listed.threads[0].error_note.as_deref(),
            Some("live_unresponsive")
        );
    }
}
