//! A thread as a tool result shows it.
//!
//! Upstream counterpart: `thread/tools/thread-row.ts` (`threadRow`) and the row builders of `thread/tools/internals.ts`
//! (`summary`, `degradedSummary`, `rowNamed`). The address book entry, when there is one, supplies what the endpoint may
//! not report: the real name (else the first user message's opening, never the durable id), the header's creation time
//! and the last entry's time, and the surface.

use maho_thread_address_core::{NamedRow, row_name};
use serde_json::{Map, Value};

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

/// A stored tool receipt in today's shape (upstream `currentResult`): receipts live 30 days, so a retry under the same
/// idempotency key can replay a result a build before #14044 wrote, with a nested `thread` object or `thread_id`, rows
/// carrying routing ids and endpoint internals, an endpoint kind on a delivery, and candidates named `id`. Each is mapped
/// to the shape a current call answers; a current result comes back unchanged.
pub fn current_result(result: Value) -> Value {
    let Value::Object(mut value) = result else {
        return result;
    };
    if value.get("kind").and_then(Value::as_str) == Some("error") {
        let Some(Value::Object(error)) = value.get("error").cloned() else {
            return Value::Object(value);
        };
        let mut error = error;
        if let Some(candidates) = value.get("candidates") {
            let mapped = stored_candidates(candidates);
            value.insert("candidates".to_owned(), mapped);
        }
        if let Some(Value::Object(details)) = error.get("details").cloned()
            && details.contains_key("candidates")
        {
            let mut details = details;
            let mapped = stored_candidates(details.get("candidates").unwrap_or(&Value::Null));
            details.insert("candidates".to_owned(), mapped);
            error.insert("details".to_owned(), Value::Object(details));
        }
        value.insert("error".to_owned(), Value::Object(error));
        return Value::Object(value);
    }
    if let Some(Value::Object(nested)) = value.get("thread").cloned() {
        let kind = value.get("kind").cloned();
        let mut others = value.clone();
        others.remove("thread");
        others.remove("kind");
        let mut rebuilt = Map::new();
        if let Some(kind) = kind {
            rebuilt.insert("kind".to_owned(), kind);
        }
        for (key, child) in stored_row(&nested) {
            rebuilt.insert(key, child);
        }
        for key in ["requested_skills", "worktree", "warnings"] {
            if let Some(value) = nested.get(key) {
                rebuilt.insert(key.to_owned(), value.clone());
            }
        }
        for (key, child) in others {
            rebuilt.insert(key, child);
        }
        value = rebuilt;
    } else if value.get("thread_id").is_some_and(Value::is_string) && value.get("thread").is_none()
    {
        value = renamed(&value, "thread_id", "thread");
    }
    if value.get("thread").is_some_and(Value::is_string)
        && value.get("status").is_some_and(Value::is_string)
    {
        value = stored_row(&value);
    }
    if let Some(Value::Array(threads)) = value.get("threads").cloned() {
        let mapped = threads
            .into_iter()
            .map(|row| match row {
                Value::Object(row) => Value::Object(stored_row(&row)),
                other => other,
            })
            .collect();
        value.insert("threads".to_owned(), Value::Array(mapped));
    }
    value.remove("endpoint");
    if let Some(Value::Object(prompt)) = value.get("prompt").cloned()
        && prompt.contains_key("endpoint")
    {
        let mut prompt = prompt;
        prompt.remove("endpoint");
        value.insert("prompt".to_owned(), Value::Object(prompt));
    }
    Value::Object(value)
}

/// A row a build before #14044 stored (`thread_id`, routing ids, endpoint internals) as today's row; a current row as is.
fn stored_row(row: &Map<String, Value>) -> Map<String, Value> {
    let is_text = |key: &str| row.get(key).is_some_and(Value::is_string);
    if is_text("thread") && is_text("state") {
        return row.clone();
    }
    if is_text("thread") {
        let mut rest = row.clone();
        rest.remove("alive");
        rest.insert(
            "state".to_owned(),
            Value::String(stored_state(row).to_owned()),
        );
        return rest;
    }
    let Some(Value::String(id)) = row.get("thread_id") else {
        return row.clone();
    };
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let link = row
        .get("link")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| thread_link(id, &name));
    let mut source = row.clone();
    source.insert("name".to_owned(), Value::String(name));
    source.insert("link".to_owned(), Value::String(link));
    source.insert(
        "state".to_owned(),
        Value::String(stored_state(row).to_owned()),
    );
    current_row(&source)
}

/// A stored row's state: a build before #13469 stored `alive` and `status` only, never `state`.
fn stored_state(row: &Map<String, Value>) -> &'static str {
    let live = row.get("status").and_then(Value::as_str) == Some("live");
    if live && row.get("alive").and_then(Value::as_bool) != Some(false) {
        "idle"
    } else {
        "inactive"
    }
}

/// The fields of a row a tool result carries (upstream `threadRow`): only what a model acts on, so a routing id or an
/// endpoint internal a stored receipt kept never comes back.
fn current_row(row: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(thread) = row.get("thread_id") {
        out.insert("thread".to_owned(), thread.clone());
    }
    for key in [
        "name",
        "unnamed",
        "status",
        "state",
        "activity",
        "error_code",
        "turn_id",
        "waiting_on",
        "engine_version",
        "backlog",
        "as_of",
        "machine",
        "cwd",
        "created_at",
        "updated_at",
        "link",
        "device",
        "alias",
        "surface",
        "controls",
        "error_note",
        "skills",
    ] {
        if let Some(value) = row.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(model) = row.get("model") {
        let projected = match model {
            Value::Object(model) => {
                let mut projected = Map::new();
                for key in ["provider", "id"] {
                    if let Some(value) = model.get(key) {
                        projected.insert(key.to_owned(), value.clone());
                    }
                }
                Value::Object(projected)
            }
            other => other.clone(),
        };
        out.insert("model".to_owned(), projected);
    }
    out
}

fn stored_candidates(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|candidate| match candidate {
                    Value::Object(candidate) if candidate.contains_key("id") => {
                        Value::Object(renamed(candidate, "id", "thread"))
                    }
                    other => other.clone(),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn renamed(value: &Map<String, Value>, from: &str, to: &str) -> Map<String, Value> {
    value
        .iter()
        .map(|(key, child)| {
            let key = if key == from {
                to.to_owned()
            } else {
                key.clone()
            };
            (key, child.clone())
        })
        .collect()
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

    #[test]
    fn a_receipt_a_build_before_14044_stored_replays_as_todays_row() {
        // tools.test.ts: "#given thread_create, thread_list, thread_rename and an ambiguous thread_read receipts stored
        // in the old shape #when the same keys are retried #then each replays the current shape".
        let old_row = serde_json::json!({
            "sessionId": "route-peer",
            "durableSessionId": "dur-peer",
            "thread_id": "dur-peer",
            "name": "old create",
            "status": "live",
            "cwd": "/w",
            "created_at": "2026-11-01T00:00:00.000Z",
            "updated_at": "2026-11-01T00:00:00.000Z",
            "socket": "/tmp/thread-tools-test.sock",
            "endpoint_kind": "rpc_host",
            "endpoint": { "kind": "rpc_host", "socket": "/tmp/thread-tools-test.sock", "routing_id": "route-peer" },
            "surface": "daemon",
            "alive": true,
        });
        let row = serde_json::json!({
            "thread": "dur-peer",
            "name": "old create",
            "status": "live",
            "cwd": "/w",
            "created_at": "2026-11-01T00:00:00.000Z",
            "updated_at": "2026-11-01T00:00:00.000Z",
            "link": "[old create](omo-thread://v1/dur-peer)",
            "surface": "daemon",
            "state": "idle",
        });
        let mut nested = old_row.clone();
        nested["requested_skills"] = serde_json::json!(["lead"]);
        let created = current_result(serde_json::json!({
            "kind": "ok",
            "thread": nested,
            "deduplicated": false,
            "prompt": {
                "delivery": { "kind": "started", "turn_id": "1" },
                "deduplicated": false,
                "endpoint": { "kind": "rpc_host" },
            },
        }));
        let mut expected_create = row.clone();
        expected_create["kind"] = serde_json::json!("ok");
        expected_create["requested_skills"] = serde_json::json!(["lead"]);
        expected_create["deduplicated"] = serde_json::json!(false);
        expected_create["prompt"] = serde_json::json!({
            "delivery": { "kind": "started", "turn_id": "1" },
            "deduplicated": false,
        });
        assert_eq!(created, expected_create);

        let listed = current_result(serde_json::json!({
            "kind": "ok",
            "threads": [old_row],
            "scope": "workspace",
        }));
        assert_eq!(
            listed,
            serde_json::json!({ "kind": "ok", "threads": [row.clone()], "scope": "workspace" })
        );
        let renamed = current_result(serde_json::json!({
            "kind": "ok",
            "thread_id": "dur-peer",
            "name": "Renamed",
        }));
        assert_eq!(
            renamed,
            serde_json::json!({ "kind": "ok", "thread": "dur-peer", "name": "Renamed" })
        );

        let old_candidates = serde_json::json!([
            { "id": "dur-twin-1", "name": "twin", "cwd": "/w", "preview": "", "updatedAt": null, "state": "live" },
            { "id": "dur-twin-2", "name": "twin", "cwd": "/w", "preview": "", "updatedAt": null, "state": "live" },
        ]);
        let read = current_result(serde_json::json!({
            "kind": "error",
            "error": {
                "code": "ambiguous_target",
                "message": "Two threads are named twin.",
                "next_action": "Pass one thread.",
                "details": { "candidates": old_candidates },
            },
        }));
        assert_eq!(
            read,
            serde_json::json!({
                "kind": "error",
                "error": {
                    "code": "ambiguous_target",
                    "message": "Two threads are named twin.",
                    "next_action": "Pass one thread.",
                    "details": { "candidates": [
                        { "thread": "dur-twin-1", "name": "twin", "cwd": "/w", "preview": "", "updatedAt": null, "state": "live" },
                        { "thread": "dur-twin-2", "name": "twin", "cwd": "/w", "preview": "", "updatedAt": null, "state": "live" },
                    ] },
                },
            })
        );
    }

    #[test]
    fn a_current_receipt_replays_unchanged() {
        // tools.test.ts: "#given current receipts #when replayed #then only the successful create's deduplication flag
        // changes"; the flag is the wrapper's, so this checks the row mapping leaves a current result alone.
        let row = serde_json::json!({
            "thread": "dur-peer",
            "name": "peer",
            "status": "live",
            "cwd": "/w",
            "created_at": "2026-11-17T00:00:00.000Z",
            "updated_at": null,
            "link": "[peer](omo-thread://v1/dur-peer)",
            "surface": "daemon",
            "state": "idle",
            "controls": ["send", "read"],
        });
        for result in [
            serde_json::json!({ "kind": "ok", "threads": [row.clone()], "scope": "workspace", "devices": [] }),
            serde_json::json!({ "kind": "ok", "thread": "dur-peer", "name": "Renamed" }),
            serde_json::json!({
                "kind": "error",
                "error": {
                    "code": "ambiguous_target",
                    "message": "Two threads are named twin.",
                    "next_action": "Pass one thread.",
                    "details": { "candidates": [
                        { "thread": "dur-twin-1", "name": "twin", "cwd": "/w", "preview": "", "updatedAt": null, "state": "live" },
                    ] },
                },
            }),
        ] {
            assert_eq!(current_result(result.clone()), result);
        }
    }
}
