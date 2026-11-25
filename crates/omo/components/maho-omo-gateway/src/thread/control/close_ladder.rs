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

use super::port::{CloseAnswer, CloseGraceful, CloseOptions, CloseOutcome, ControlResult, refuse};

/// senpi `unload_session { graceful, waitMs }` (omo #14313).
pub const UNLOAD_GRACEFUL: &str = "unload_graceful";

/// Default graceful drain when the caller names no `wait_ms` and what of the call budget stays reserved for the answer.
const GRACEFUL_WAIT_MS: i64 = 30_000;
const GRACEFUL_ANSWER_RESERVE_MS: i64 = 10_000;

/// Longest drain a remote device leg can wait before its relay request bound cuts off (omo #14324).
pub const REMOTE_GRACEFUL_MAX_WAIT_MS: i64 = 35_000;

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

fn close_effect_id() -> String {
    format!("omo-thread-close:{}", uuid::Uuid::new_v4())
}

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

fn done(
    outcome: CloseOutcome,
    data: Option<&Map<String, Value>>,
    graceful: Option<CloseGraceful>,
) -> CloseAnswer {
    CloseAnswer {
        outcome,
        dropped_deliveries: data
            .and_then(|data| deliveries_of(data.get("dropped")))
            .unwrap_or_default(),
        graceful,
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
    F: Fn(Value, Option<i64>) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    let mut record = Map::new();
    record.insert("type".into(), json!("unload_session"));
    record.insert("force".into(), json!(true));
    record.insert("children".into(), json!("detach"));
    record.insert("sessionId".into(), json!(session_id));
    record.insert("effectId".into(), json!(close_effect_id()));
    record.extend(fields);
    Ok(match send(Value::Object(record), None).await? {
        RecordAnswer::Gone(_) => Unloaded::Done(done(CloseOutcome::NotLive, None, None)),
        RecordAnswer::Ok(data) => {
            if data.get("outcome").and_then(Value::as_str) == Some("unloaded") {
                Unloaded::Done(done(CloseOutcome::Closed, Some(&data), None))
            } else if data.get("reason").and_then(Value::as_str) == Some("session_closing") {
                Unloaded::Done(done(CloseOutcome::NotLive, Some(&data), None))
            } else {
                Unloaded::Refused(data)
            }
        }
    })
}

/// Whether the session's engine advertises `capability`; anything it cannot answer counts as an older engine.
async fn advertises<F, Fut>(session_id: &str, send: &F, capability: &str) -> bool
where
    F: Fn(Value, Option<i64>) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    match send(
        json!({ "type": "get_protocol_info", "sessionId": session_id, "observe": true }),
        None,
    )
    .await
    {
        Ok(RecordAnswer::Ok(data)) => strings(data.get("capabilities"))
            .iter()
            .any(|item| item.as_str() == capability),
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
    options: CloseOptions,
    leg_max_wait_ms: Option<i64>,
) -> ControlResult<CloseAnswer>
where
    F: Fn(Value, Option<i64>) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    close_ladder_at(
        session_id,
        send,
        options,
        leg_max_wait_ms,
        call_budget::now_ms(),
    )
    .await
}

/// [`close_ladder`] evaluated against an explicit `now_ms` epoch timestamp for deterministic call-budget tests.
pub async fn close_ladder_at<F, Fut>(
    session_id: &str,
    send: F,
    options: CloseOptions,
    leg_max_wait_ms: Option<i64>,
    now_ms: i64,
) -> ControlResult<CloseAnswer>
where
    F: Fn(Value, Option<i64>) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    if options.graceful {
        return graceful_close(session_id, &send, options.wait_ms, leg_max_wait_ms, now_ms).await;
    }
    let first_fields = if options.force {
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
    if !options.force && busy {
        if deliveries_of(refused.get("pending")).is_none()
            && !advertises(session_id, &send, UNLOAD_REFUSE_DELIVERIES).await
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

fn graceful_of(data: &Map<String, Value>) -> CloseGraceful {
    let facts = data.get("graceful").and_then(Value::as_object);
    let dropped = data.get("dropped").and_then(Value::as_object);
    let stopped = facts
        .and_then(|facts| facts.get("stopped"))
        .and_then(Value::as_object);
    CloseGraceful {
        turn: facts
            .and_then(|facts| facts.get("turn"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        waited_ms: facts
            .and_then(|facts| facts.get("waited_ms"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        stopped_wake_sources: stopped
            .map(|stopped| strings(stopped.get("wake_sources")))
            .unwrap_or_default(),
        returned_user_messages: dropped
            .map(|dropped| strings(dropped.get("user_messages")))
            .unwrap_or_default(),
    }
}

async fn graceful_close<F, Fut>(
    session_id: &str,
    send: &F,
    wait_ms: Option<i64>,
    leg_max_wait_ms: Option<i64>,
    now_ms: i64,
) -> ControlResult<CloseAnswer>
where
    F: Fn(Value, Option<i64>) -> Fut,
    Fut: Future<Output = ControlResult<RecordAnswer>>,
{
    if !advertises(session_id, send, UNLOAD_GRACEFUL).await {
        return Err(refuse(
            "unsupported",
            format!(
                "The engine holding thread {session_id} cannot close it gracefully: it runs a senpi from before graceful close."
            ),
            "Update omo on that device and restart its host, or close the thread with force: true (its running turn is stopped).",
            Some(json!({ "command": "unload_session", "capability": UNLOAD_GRACEFUL })),
        ));
    }
    let requested = wait_ms.unwrap_or(GRACEFUL_WAIT_MS);
    let leg_cut = leg_max_wait_ms.filter(|max| requested > *max);
    let wait = call_budget::wait_within_call_budget(
        leg_cut.unwrap_or(requested),
        GRACEFUL_ANSWER_RESERVE_MS,
        now_ms,
    );
    // Only a drain bounded by the leg's cap names wait_capped; a shorter call budget wins instead (#14352).
    let capped = leg_cut.filter(|cap| wait == *cap);
    if wait <= 0 {
        return Err(refuse(
            "unsupported",
            format!("This call has no time left to wait for thread {session_id}'s turn."),
            "Close it again in a new call.",
            Some(json!({ "command": "unload_session" })),
        ));
    }
    let record = json!({
        "type": "unload_session",
        "force": true,
        "children": "detach",
        "sessionId": session_id,
        "effectId": close_effect_id(),
        "graceful": true,
        "waitMs": wait,
    });
    let answered = send(record, Some(wait)).await?;
    let data = match answered {
        RecordAnswer::Gone(_) => {
            return Ok(CloseAnswer {
                outcome: CloseOutcome::NotLive,
                dropped_deliveries: Vec::new(),
                graceful: None,
            });
        }
        RecordAnswer::Ok(data) => data,
    };
    let dropped = deliveries_of(data.get("dropped")).unwrap_or_default();
    if data.get("outcome").and_then(Value::as_str) == Some("unloaded") {
        return Ok(CloseAnswer {
            outcome: CloseOutcome::Closed,
            dropped_deliveries: dropped,
            graceful: Some(graceful_of(&data)),
        });
    }
    if data.get("reason").and_then(Value::as_str) == Some("session_closing") {
        return Ok(CloseAnswer {
            outcome: CloseOutcome::NotLive,
            dropped_deliveries: dropped,
            graceful: None,
        });
    }
    if data.get("timed_out").and_then(Value::as_bool) == Some(true) {
        let cap_note = capped.map_or_else(String::new, |_| {
            format!(
                " (the most a thread on another device can wait, not the {requested} ms asked for)"
            )
        });
        let next_action = if capped.is_none() {
            "Close it again with a longer wait_ms once its turn is nearly done, or with force: true to stop the turn."
        } else {
            "Close it again once its turn is nearly done, or with force: true to stop the turn."
        };
        let mut details = Map::new();
        if let Some(max_ms) = capped {
            details.insert(
                "wait_capped".into(),
                json!({ "requested_ms": requested, "max_ms": max_ms }),
            );
        }
        details.insert("command".into(), json!("unload_session"));
        details.insert("reason".into(), json!("turn_active"));
        details.insert("still_running".into(), json!(true));
        details.insert("open".into(), json!(true));
        details.insert(
            "turn".into(),
            data.get("turn").cloned().unwrap_or(Value::Null),
        );
        details.insert(
            "waited_ms".into(),
            data.get("waited_ms").cloned().unwrap_or(Value::Null),
        );
        details.insert("returned_deliveries".into(), json!(dropped));
        details.insert("retry_with".into(), json!({ "force": true }));
        return Err(refuse(
            "turn_active",
            format!(
                "Thread {session_id} is still working on turn {} after {} ms{cap_note}. It stays open and takes messages again; the {} unread message(s) taken out of its queue are queued for it again.",
                text_of(data.get("turn")),
                text_of(data.get("waited_ms")),
                dropped.len()
            ),
            next_action,
            Some(Value::Object(details)),
        ));
    }
    if data.get("reason").and_then(Value::as_str) == Some("composer_draft") {
        return Err(refuse(
            "unsupported",
            format!(
                "Thread {session_id}'s terminal has unsent text in its composer, and a close would discard it."
            ),
            "Ask the person at that terminal to send or clear it, then close the thread again.",
            Some(json!({ "command": "unload_session", "reason": "composer_draft" })),
        ));
    }
    refused_unload(session_id, &data, send).await
}

async fn refused_unload<F, Fut>(
    session_id: &str,
    refused: &Map<String, Value>,
    send: &F,
) -> ControlResult<CloseAnswer>
where
    F: Fn(Value, Option<i64>) -> Fut,
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
    if let RecordAnswer::Gone(_) = send(
        json!({ "type": "close_session", "sessionId": session_id }),
        None,
    )
    .await?
    {
        return Ok(CloseAnswer {
            outcome: CloseOutcome::NotLive,
            dropped_deliveries: Vec::new(),
            graceful: None,
        });
    }
    let probe = send(
        json!({ "type": "get_state", "sessionId": session_id }),
        None,
    )
    .await?;
    Ok(CloseAnswer {
        outcome: if matches!(probe, RecordAnswer::Gone(_)) {
            CloseOutcome::Closed
        } else {
            CloseOutcome::HeldElsewhere
        },
        dropped_deliveries: Vec::new(),
        graceful: None,
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
        waits: Mutex<Vec<i64>>,
    }

    impl Engine {
        fn new(answers: Vec<ControlResult<RecordAnswer>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                records: Mutex::new(Vec::new()),
                waits: Mutex::new(Vec::new()),
            })
        }

        fn close_with(
            self: &Arc<Self>,
            options: CloseOptions,
            leg_max_wait_ms: Option<i64>,
        ) -> ControlResult<CloseAnswer> {
            let engine = self.clone();
            let send = move |record: Value, wait_ms: Option<i64>| {
                engine.records.lock().expect("records").push(record);
                if let Some(wait) = wait_ms {
                    engine.waits.lock().expect("waits").push(wait);
                }
                let answer = engine
                    .answers
                    .lock()
                    .expect("answers")
                    .pop_front()
                    .unwrap_or_else(|| Err(ControlError::Transport("unexpected record".into())));
                std::future::ready(answer)
            };
            block_on(close_ladder("rpc-7", send, options, leg_max_wait_ms))
        }

        async fn close_at(
            self: &Arc<Self>,
            options: CloseOptions,
            leg_max_wait_ms: Option<i64>,
            now_ms: i64,
        ) -> ControlResult<CloseAnswer> {
            let engine = self.clone();
            let send = move |record: Value, wait_ms: Option<i64>| {
                engine.records.lock().expect("records").push(record);
                if let Some(wait) = wait_ms {
                    engine.waits.lock().expect("waits").push(wait);
                }
                let answer = engine
                    .answers
                    .lock()
                    .expect("answers")
                    .pop_front()
                    .unwrap_or_else(|| Err(ControlError::Transport("unexpected record".into())));
                std::future::ready(answer)
            };
            close_ladder_at("rpc-7", send, options, leg_max_wait_ms, now_ms).await
        }

        fn close(self: &Arc<Self>, force: bool) -> ControlResult<CloseAnswer> {
            self.close_with(
                CloseOptions {
                    force,
                    ..Default::default()
                },
                None,
            )
        }

        fn sent(&self) -> Vec<Value> {
            self.records.lock().expect("records").clone()
        }

        fn waits(&self) -> Vec<i64> {
            self.waits.lock().expect("waits").clone()
        }
    }

    fn data(value: Value) -> ControlResult<RecordAnswer> {
        Ok(RecordAnswer::Ok(
            value.as_object().cloned().unwrap_or_default(),
        ))
    }

    fn refused_code(answer: ControlResult<CloseAnswer>) -> (String, Value) {
        match answer {
            Err(ControlError::Refused(failure)) => {
                let failure = *failure;
                (
                    failure.code.as_str().to_owned(),
                    failure.details.unwrap_or(Value::Null),
                )
            }
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
                dropped_deliveries: Vec::new(),
                graceful: None,
            })
        );
        let sent = engine.sent();
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0]["effectId"]
                .as_str()
                .is_some_and(|id| id.starts_with("omo-thread-close:"))
        );
        assert_eq!(sent[0]["type"], json!("unload_session"));
        assert_eq!(sent[0]["force"], json!(true));
        assert_eq!(sent[0]["children"], json!("detach"));
        assert_eq!(sent[0]["sessionId"], json!("rpc-7"));
        assert_eq!(sent[0]["deliveries"], json!("refuse"));
        assert_eq!(sent[0].get("interrupt"), None);
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
                dropped_deliveries: vec!["d-1".into(), "d-2".into()],
                graceful: None,
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

    #[test]
    fn a_capable_engine_closes_gracefully_and_reports_what_it_waited_for_and_returned() {
        let engine = Engine::new(vec![
            data(
                json!({ "capabilities": ["unload_session", UNLOAD_REFUSE_DELIVERIES, UNLOAD_GRACEFUL] }),
            ),
            data(json!({
                "outcome": "unloaded",
                "sessionPath": "/s.jsonl",
                "dropped": { "deliveries": ["d-queued"], "user_messages": ["typed while busy"] },
                "graceful": { "turn": 4, "waited_ms": 1200, "stopped": { "wake_sources": ["terminal"] } },
            })),
        ]);
        assert_eq!(
            engine.close_with(
                CloseOptions {
                    graceful: true,
                    wait_ms: Some(20_000),
                    ..Default::default()
                },
                None,
            ),
            Ok(CloseAnswer {
                outcome: CloseOutcome::Closed,
                dropped_deliveries: vec!["d-queued".into()],
                graceful: Some(CloseGraceful {
                    turn: 4,
                    waited_ms: 1200,
                    stopped_wake_sources: vec!["terminal".into()],
                    returned_user_messages: vec!["typed while busy".into()],
                }),
            })
        );
        let sent = engine.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["type"], json!("unload_session"));
        assert_eq!(sent[1]["graceful"], json!(true));
        assert_eq!(sent[1]["waitMs"], json!(20_000));
        assert_eq!(sent[1].get("interrupt"), None);
        assert!(
            sent[1]["effectId"]
                .as_str()
                .is_some_and(|id| id.starts_with("omo-thread-close:"))
        );
        assert_eq!(engine.waits(), vec![20_000]);
    }

    #[test]
    fn a_remote_leg_cap_bounds_graceful_wait_and_names_wait_capped() {
        let engine = Engine::new(vec![
            data(json!({ "capabilities": [UNLOAD_GRACEFUL] })),
            data(json!({
                "outcome": "refused",
                "reason": "turn_active",
                "timed_out": true,
                "open": true,
                "turn": 2,
                "waited_ms": REMOTE_GRACEFUL_MAX_WAIT_MS,
                "dropped": { "deliveries": [], "user_messages": [] },
            })),
        ]);
        let (code, details) = refused_code(engine.close_with(
            CloseOptions {
                graceful: true,
                wait_ms: Some(120_000),
                ..Default::default()
            },
            Some(REMOTE_GRACEFUL_MAX_WAIT_MS),
        ));
        assert_eq!(code, "turn_active");
        assert_eq!(
            engine.sent()[1]["waitMs"],
            json!(REMOTE_GRACEFUL_MAX_WAIT_MS)
        );
        assert_eq!(engine.waits(), vec![REMOTE_GRACEFUL_MAX_WAIT_MS]);
        assert_eq!(details["waited_ms"], json!(REMOTE_GRACEFUL_MAX_WAIT_MS));
        assert_eq!(
            details["wait_capped"],
            json!({ "requested_ms": 120_000, "max_ms": REMOTE_GRACEFUL_MAX_WAIT_MS })
        );
    }

    #[test]
    fn a_timed_out_graceful_drain_stays_open_and_names_turn_and_returned_deliveries() {
        let engine = Engine::new(vec![
            data(json!({ "capabilities": [UNLOAD_GRACEFUL] })),
            data(json!({
                "outcome": "refused",
                "reason": "turn_active",
                "timed_out": true,
                "open": true,
                "admission": "reopened",
                "turn": 7,
                "waited_ms": 5000,
                "dropped": { "deliveries": ["d-1"], "user_messages": [] },
            })),
        ]);
        let (code, details) = refused_code(engine.close_with(
            CloseOptions {
                graceful: true,
                wait_ms: Some(5000),
                ..Default::default()
            },
            None,
        ));
        assert_eq!(code, "turn_active");
        assert_eq!(details["still_running"], json!(true));
        assert_eq!(details["open"], json!(true));
        assert_eq!(details["turn"], json!(7));
        assert_eq!(details["waited_ms"], json!(5000));
        assert_eq!(details["returned_deliveries"], json!(["d-1"]));
    }

    #[test]
    fn an_engine_without_unload_graceful_is_refused_unsupported_without_unloading() {
        let engine = Engine::new(vec![
            data(json!({ "capabilities": ["unload_session"] })),
            data(json!({ "outcome": "unloaded" })),
        ]);
        let (code, details) = refused_code(engine.close_with(
            CloseOptions {
                graceful: true,
                ..Default::default()
            },
            None,
        ));
        assert_eq!(code, "unsupported");
        assert_eq!(details["capability"], json!(UNLOAD_GRACEFUL));
        assert_eq!(engine.sent().len(), 1);
        assert_eq!(engine.sent()[0]["type"], json!("get_protocol_info"));
    }

    #[test]
    fn a_terminal_composer_draft_refuses_graceful_close_naming_the_draft() {
        let engine = Engine::new(vec![
            data(json!({ "capabilities": [UNLOAD_GRACEFUL] })),
            data(json!({
                "outcome": "refused",
                "reason": "composer_draft",
                "detail": "unsent draft",
            })),
        ]);
        let (code, details) = refused_code(engine.close_with(
            CloseOptions {
                graceful: true,
                ..Default::default()
            },
            None,
        ));
        assert_eq!(code, "unsupported");
        assert_eq!(details["reason"], json!("composer_draft"));
        let unloads: Vec<_> = engine
            .sent()
            .into_iter()
            .filter(|rpc| rpc["type"] == "unload_session")
            .collect();
        assert_eq!(unloads.len(), 1);
        assert_eq!(unloads[0]["graceful"], json!(true));
    }

    #[tokio::test]
    async fn graceful_close_cuts_wait_to_the_remaining_call_budget() {
        const NOW: i64 = 1_000_000;
        let engine = Engine::new(vec![
            data(json!({ "capabilities": [UNLOAD_GRACEFUL] })),
            data(json!({ "outcome": "unloaded" })),
        ]);
        let result = call_budget::with_call_deadline(
            NOW + 25_000,
            engine.close_at(
                CloseOptions {
                    graceful: true,
                    wait_ms: Some(600_000),
                    ..Default::default()
                },
                None,
                NOW,
            ),
        )
        .await;
        assert_eq!(
            result,
            Ok(CloseAnswer {
                outcome: CloseOutcome::Closed,
                dropped_deliveries: Vec::new(),
                graceful: Some(CloseGraceful::default()),
            })
        );
        let wait_ms = engine.sent()[1]["waitMs"].as_i64().expect("waitMs");
        assert!(wait_ms > 0 && wait_ms <= 15_000, "wait_ms={wait_ms}");
    }

    #[tokio::test]
    async fn graceful_close_under_a_shorter_call_budget_does_not_name_wait_capped() {
        const NOW: i64 = 1_000_000;
        let engine = Engine::new(vec![
            data(json!({ "capabilities": [UNLOAD_GRACEFUL] })),
            data(json!({
                "outcome": "refused",
                "reason": "turn_active",
                "timed_out": true,
                "open": true,
                "turn": 3,
                "waited_ms": 15_000,
                "dropped": { "deliveries": [], "user_messages": [] },
            })),
        ]);
        let (code, details) = refused_code(
            call_budget::with_call_deadline(
                NOW + 25_000,
                engine.close_at(
                    CloseOptions {
                        graceful: true,
                        wait_ms: Some(120_000),
                        ..Default::default()
                    },
                    Some(REMOTE_GRACEFUL_MAX_WAIT_MS),
                    NOW,
                ),
            )
            .await,
        );
        assert_eq!(code, "turn_active");
        assert_eq!(engine.waits(), vec![15_000]);
        assert!(
            details["waited_ms"].as_i64().expect("waited_ms") < REMOTE_GRACEFUL_MAX_WAIT_MS,
            "{details}"
        );
        assert_eq!(details.get("wait_capped"), None);
    }
}
