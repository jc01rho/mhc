//! The one close ladder.
//!
//! Upstream counterpart: `thread/control/close-ladder.ts` (omo #12879 audit 2, #14234, #14233/#14237). `unload_session`
//! with force ends the session for every client that holds it. A close never drops a message the session has not read
//! unless the caller asked for it:
//!
//! - `force`: one interrupting unload; whatever the engine hands back unwritten is listed in `dropped_deliveries`.
//! - default: a non-interrupting unload first, which drops nothing on any engine and closes an idle session in one
//!   round trip. A refusal that names unwritten deliveries (`pending.deliveries`) comes from an engine with
//!   `unload_refuse_deliveries`; any other busy refusal asks the engine for that capability (`get_protocol_info`). A
//!   capable engine gets `interrupt` with `deliveries: "refuse"`: it stops a run with nothing unread, hands back an
//!   idle session's stranded deliveries, and refuses a busy one with unread deliveries, naming them. An engine without
//!   the capability ignores `deliveries`, so it is never sent an interrupt: the close is refused, saying the ids are
//!   unavailable there and that `force: true` closes it.
//!
//! A session with no file yet cannot be unloaded (`unsupported`), so it gets `close_session`, which releases only this
//! hold, and one `get_state` probe: still answered, another client holds it; gone, it ended. A draining host is
//! `host_unavailable`; any other unload refusal is typed `unsupported` naming the engine's reason. A session the host no
//! longer lists (`route_unlisted`, it parked after it was listed) reads as gone: `not_live`.

use std::future::Future;

use serde_json::{Map, Value, json};

use super::port::{CloseAnswer, CloseOutcome, ControlResult, refuse};

/// How a leg answers one engine record for the ladder: the engine's reply data, or `Gone` when the engine no longer
/// holds the session (one of [`SESSION_GONE`]). Every other failure the leg answers itself, typed the way that leg
/// types it.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordAnswer {
    Ok(Map<String, Value>),
    Gone(String),
}

/// The refusals that mean the session was not on its host when the record arrived: the engine's, and the device
/// service's `route_unlisted` (the host lists no such session right now, so nothing was dispatched, omo #14233).
pub const SESSION_GONE: [&str; 5] = [
    "unknown_session",
    "route_unlisted",
    "session_identity_mismatch",
    "session_not_attached",
    "not_attached",
];

/// senpi#7006 (senpi#7004): the engine takes `unload_session { deliveries: "refuse" }` and names the unwritten
/// deliveries it refuses on.
pub const UNLOAD_REFUSE_DELIVERIES: &str = "unload_refuse_deliveries";

/// Whether an error text a leg reads names a session its host no longer holds: `<code>` or `<code>:<detail>`.
pub fn session_gone_reason(error: &str) -> Option<&'static str> {
    let code = error.split(':').next().unwrap_or_default();
    SESSION_GONE.into_iter().find(|gone| *gone == code)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn deliveries_of(value: Option<&Value>) -> Option<Vec<String>> {
    value
        .filter(|value| value.is_object())
        .map(|value| strings(value.get("deliveries")))
}

fn done(outcome: CloseOutcome, data: Option<&Map<String, Value>>) -> CloseAnswer {
    CloseAnswer {
        outcome,
        dropped_deliveries: data
            .and_then(|data| deliveries_of(data.get("dropped")))
            .unwrap_or_default(),
    }
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "unknown".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn describe(data: &Map<String, Value>) -> String {
    let reason = text_of(data.get("reason"));
    match data.get("detail").and_then(Value::as_str) {
        Some(detail) => format!("{reason}: {detail}"),
        None => reason,
    }
}

enum Unloaded {
    Done(CloseAnswer),
    Refused(Map<String, Value>),
}

async fn unload<F, Fut>(
    session_id: &str,
    send: &F,
    fields: Map<String, Value>,
) -> ControlResult<Unloaded>
where
    F: Fn(Value) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    let mut record = Map::new();
    record.insert("type".into(), json!("unload_session"));
    record.insert("force".into(), json!(true));
    record.insert("children".into(), json!("detach"));
    record.insert("sessionId".into(), json!(session_id));
    record.extend(fields);
    Ok(match send(Value::Object(record)).await? {
        RecordAnswer::Gone(_) => Unloaded::Done(done(CloseOutcome::NotLive, None)),
        RecordAnswer::Ok(data) => {
            if data.get("outcome").and_then(Value::as_str) == Some("unloaded") {
                Unloaded::Done(done(CloseOutcome::Closed, Some(&data)))
            } else if data.get("reason").and_then(Value::as_str) == Some("session_closing") {
                Unloaded::Done(done(CloseOutcome::NotLive, Some(&data)))
            } else {
                Unloaded::Refused(data)
            }
        }
    })
}

/// Whether the session's engine advertises `unload_refuse_deliveries`; anything it cannot answer counts as an older
/// engine.
async fn refuses_deliveries<F, Fut>(session_id: &str, send: &F) -> bool
where
    F: Fn(Value) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    match send(json!({ "type": "get_protocol_info", "sessionId": session_id, "observe": true }))
        .await
    {
        Ok(RecordAnswer::Ok(data)) => strings(data.get("capabilities"))
            .iter()
            .any(|capability| capability == UNLOAD_REFUSE_DELIVERIES),
        _ => false,
    }
}

fn fields(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// The one close ladder, on this device's host and on another device alike; `send` writes one engine record on the
/// thread's engine.
pub async fn close_ladder<F, Fut>(
    session_id: &str,
    send: F,
    force: bool,
) -> ControlResult<CloseAnswer>
where
    F: Fn(Value) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    let first_fields = if force {
        fields([("interrupt", json!(true))])
    } else {
        fields([("deliveries", json!("refuse"))])
    };
    let mut refused = match unload(session_id, &send, first_fields).await? {
        Unloaded::Done(answer) => return Ok(answer),
        Unloaded::Refused(data) => data,
    };
    let busy = matches!(
        refused.get("reason").and_then(Value::as_str),
        Some("turn_active" | "work_pending")
    );
    if !force && busy {
        if deliveries_of(refused.get("pending")).is_none()
            && !refuses_deliveries(session_id, &send).await
        {
            let turn_active = refused.get("reason").and_then(Value::as_str) == Some("turn_active");
            let message = if turn_active {
                format!(
                    "Thread {session_id} is still working ({}), and its host's engine cannot name the messages a forced close would drop.",
                    describe(&refused)
                )
            } else {
                format!(
                    "Thread {session_id} holds messages it has not read ({}), and its host's engine cannot name them.",
                    describe(&refused)
                )
            };
            let mut details = fields([
                ("command", json!("unload_session")),
                (
                    "reason",
                    refused.get("reason").cloned().unwrap_or(Value::Null),
                ),
            ]);
            if let Some(detail) = refused.get("detail") {
                details.insert("detail".into(), detail.clone());
            }
            details.insert("pending_deliveries".into(), json!("unavailable"));
            return Err(refuse(
                "turn_active",
                message,
                "Wait for it to read them, or close it with force: true (anything it has not read is dropped); an engine that names them comes with a newer senpi on that device.",
                Some(Value::Object(details)),
            ));
        }
        let second_fields = fields([("interrupt", json!(true)), ("deliveries", json!("refuse"))]);
        refused = match unload(session_id, &send, second_fields).await? {
            Unloaded::Done(answer) => return Ok(answer),
            Unloaded::Refused(data) => data,
        };
        // Any refusal that names unread deliveries is answered by them, whichever blocker the engine named first.
        if let Some(unread) =
            deliveries_of(refused.get("pending")).filter(|unread| !unread.is_empty())
        {
            return Err(refuse(
                "turn_active",
                format!(
                    "Thread {session_id} has {} message(s) it has not read yet ({}); closing it now would drop them.",
                    unread.len(),
                    unread.join(", ")
                ),
                "Let it read them and retry, or close it with force: true to drop them (the result lists dropped_deliveries).",
                Some(json!({
                    "command": "unload_session",
                    "reason": refused.get("reason").cloned().unwrap_or(Value::Null),
                    "pending_deliveries": unread,
                    "retry_with": { "force": true },
                })),
            ));
        }
    }
    refused_unload(session_id, &refused, &send).await
}

async fn refused_unload<F, Fut>(
    session_id: &str,
    refused: &Map<String, Value>,
    send: &F,
) -> ControlResult<CloseAnswer>
where
    F: Fn(Value) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    let reason = refused.get("reason").and_then(Value::as_str);
    if reason == Some("host_draining") {
        return Err(refuse(
            "host_unavailable",
            format!(
                "The host holding thread {session_id} is shutting down and refused to close it."
            ),
            "Retry once that host has restarted.",
            Some(json!({ "command": "unload_session", "reason": "host_draining" })),
        ));
    }
    if reason != Some("unsupported") {
        let detail = refused
            .get("detail")
            .and_then(Value::as_str)
            .map(|detail| format!(": {detail}"))
            .unwrap_or_default();
        let mut details = fields([
            ("command", json!("unload_session")),
            (
                "reason",
                refused.get("reason").cloned().unwrap_or(Value::Null),
            ),
        ]);
        if let Some(detail) = refused.get("detail") {
            details.insert("detail".into(), detail.clone());
        }
        // Deliveries an interrupt handed back before a later blocker refused the unload are gone from the session
        // either way.
        if let Some(dropped) =
            deliveries_of(refused.get("dropped")).filter(|dropped| !dropped.is_empty())
        {
            details.insert("dropped_deliveries".into(), json!(dropped));
        }
        return Err(refuse(
            "unsupported",
            format!(
                "The host holding thread {session_id} refused to close it ({}{detail}).",
                text_of(refused.get("reason"))
            ),
            "Read the thread and retry once its work settles.",
            Some(Value::Object(details)),
        ));
    }
    if let RecordAnswer::Gone(_) =
        send(json!({ "type": "close_session", "sessionId": session_id })).await?
    {
        return Ok(CloseAnswer {
            outcome: CloseOutcome::NotLive,
            dropped_deliveries: Vec::new(),
        });
    }
    let probe = send(json!({ "type": "get_state", "sessionId": session_id })).await?;
    Ok(CloseAnswer {
        outcome: if matches!(probe, RecordAnswer::Gone(_)) {
            CloseOutcome::Closed
        } else {
            CloseOutcome::HeldElsewhere
        },
        dropped_deliveries: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use futures::executor::block_on;

    use super::super::port::ControlError;
    use super::*;

    /// An engine that answers each record with the next scripted answer and keeps every record it was sent.
    struct Engine {
        answers: Mutex<VecDeque<ControlResult<RecordAnswer>>>,
        records: Mutex<Vec<Value>>,
    }

    impl Engine {
        fn new(answers: Vec<ControlResult<RecordAnswer>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                records: Mutex::new(Vec::new()),
            })
        }

        fn close(self: &Arc<Self>, force: bool) -> ControlResult<CloseAnswer> {
            let engine = self.clone();
            let send = move |record: Value| {
                engine.records.lock().unwrap().push(record);
                let answer = engine
                    .answers
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Err(ControlError::Transport("unexpected record".into())));
                std::future::ready(answer)
            };
            block_on(close_ladder("rpc-7", send, force))
        }

        fn sent(&self) -> Vec<Value> {
            self.records.lock().unwrap().clone()
        }
    }

    fn data(value: Value) -> ControlResult<RecordAnswer> {
        Ok(RecordAnswer::Ok(
            value.as_object().cloned().unwrap_or_default(),
        ))
    }

    fn refused_code(answer: ControlResult<CloseAnswer>) -> (String, Value) {
        match answer {
            Err(ControlError::Refused(failure)) => (
                failure.code.as_str().to_owned(),
                failure.details.unwrap_or(Value::Null),
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_idle_session_closes_in_one_unload_that_interrupts_nothing() {
        let engine = Engine::new(vec![data(json!({ "outcome": "unloaded" }))]);
        assert_eq!(
            engine.close(false),
            Ok(CloseAnswer {
                outcome: CloseOutcome::Closed,
                dropped_deliveries: Vec::new()
            })
        );
        assert_eq!(
            engine.sent(),
            vec![json!({
                "type": "unload_session",
                "force": true,
                "children": "detach",
                "sessionId": "rpc-7",
                "deliveries": "refuse",
            })]
        );
    }

    #[test]
    fn a_forced_close_interrupts_once_and_lists_what_it_dropped() {
        let engine = Engine::new(vec![data(json!({
            "outcome": "unloaded",
            "dropped": { "deliveries": ["d-1", "d-2"] },
        }))]);
        assert_eq!(
            engine.close(true),
            Ok(CloseAnswer {
                outcome: CloseOutcome::Closed,
                dropped_deliveries: vec!["d-1".into(), "d-2".into()]
            })
        );
        assert_eq!(engine.sent()[0]["interrupt"], json!(true));
        assert_eq!(engine.sent()[0].get("deliveries"), None);
    }

    #[test]
    fn a_busy_session_with_unread_deliveries_is_refused_naming_them() {
        let busy = || {
            data(json!({
                "outcome": "refused",
                "reason": "turn_active",
                "pending": { "deliveries": ["d-9"] },
            }))
        };
        let engine = Engine::new(vec![busy(), busy()]);
        let (code, details) = refused_code(engine.close(false));
        assert_eq!(code, "turn_active");
        assert_eq!(details["pending_deliveries"], json!(["d-9"]));
        assert_eq!(details["retry_with"], json!({ "force": true }));
        let sent = engine.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["interrupt"], json!(true));
        assert_eq!(sent[1]["deliveries"], json!("refuse"));
    }

    #[test]
    fn an_engine_that_cannot_name_unread_deliveries_is_never_interrupted() {
        let engine = Engine::new(vec![
            data(json!({ "outcome": "refused", "reason": "turn_active", "detail": "agent run" })),
            data(json!({ "capabilities": ["unload_session"] })),
        ]);
        let (code, details) = refused_code(engine.close(false));
        assert_eq!(code, "turn_active");
        assert_eq!(details["pending_deliveries"], json!("unavailable"));
        assert_eq!(details["detail"], json!("agent run"));
        let sent = engine.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["type"], json!("get_protocol_info"));
        assert!(sent.iter().all(|record| record.get("interrupt").is_none()));
    }

    #[test]
    fn a_capable_engine_stops_a_run_with_nothing_unread() {
        let engine = Engine::new(vec![
            data(json!({ "outcome": "refused", "reason": "turn_active" })),
            data(json!({ "capabilities": [UNLOAD_REFUSE_DELIVERIES] })),
            data(json!({ "outcome": "unloaded" })),
        ]);
        assert_eq!(
            engine.close(false).map(|answer| answer.outcome),
            Ok(CloseOutcome::Closed)
        );
        assert_eq!(engine.sent()[2]["interrupt"], json!(true));
    }

    #[test]
    fn a_session_the_host_no_longer_lists_is_not_live() {
        let engine = Engine::new(vec![Ok(RecordAnswer::Gone("route_unlisted".into()))]);
        assert_eq!(
            engine.close(false).map(|answer| answer.outcome),
            Ok(CloseOutcome::NotLive)
        );
        assert_eq!(
            session_gone_reason("route_unlisted:d-1"),
            Some("route_unlisted")
        );
        assert_eq!(session_gone_reason("connection refused"), None);
    }

    #[test]
    fn a_session_with_no_file_is_released_and_probed() {
        let unsupported = || data(json!({ "outcome": "refused", "reason": "unsupported" }));
        let held = Engine::new(vec![unsupported(), data(json!({})), data(json!({}))]);
        assert_eq!(
            held.close(false).map(|answer| answer.outcome),
            Ok(CloseOutcome::HeldElsewhere)
        );
        let ended = Engine::new(vec![
            unsupported(),
            data(json!({})),
            Ok(RecordAnswer::Gone("unknown_session".into())),
        ]);
        assert_eq!(
            ended.close(false).map(|answer| answer.outcome),
            Ok(CloseOutcome::Closed)
        );
    }

    #[test]
    fn a_draining_host_and_any_other_refusal_are_typed() {
        let draining = Engine::new(vec![data(
            json!({ "outcome": "refused", "reason": "host_draining" }),
        )]);
        assert_eq!(refused_code(draining.close(false)).0, "host_unavailable");
        let children = Engine::new(vec![data(json!({
            "outcome": "refused",
            "reason": "children_live",
            "dropped": { "deliveries": ["d-3"] },
        }))]);
        let (code, details) = refused_code(children.close(true));
        assert_eq!(code, "unsupported");
        assert_eq!(details["dropped_deliveries"], json!(["d-3"]));
    }
}
