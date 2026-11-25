//! What the carrier reads from `get_state` and the open reply.
//!
//! Upstream counterpart: `thread/wake/engine-state.ts` (omo#13037 A3.2.1-A3.2.8, close probe K1-K10). Finished means
//! nothing runs and nothing waits but a stalled turn; a person's wait is a pending dialog or a human wait that is not
//! stalled; held is a hold or a paused session. Background work and children are work: the carrier never releases then.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineView {
    pub held: bool,
    pub hold_kinds: Vec<String>,
    pub waiting_for_person: bool,
    pub working: bool,
    pub finished: bool,
    pub human_input_revision: Option<i64>,
}

fn record(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn list(value: Option<&Value>) -> Vec<Value> {
    value.and_then(Value::as_array).cloned().unwrap_or_default()
}

fn kind_of(entry: &Value) -> String {
    match entry.get("kind") {
        None | Some(Value::Null) => "unknown".to_owned(),
        Some(Value::String(kind)) => kind.clone(),
        Some(other) => other.to_string(),
    }
}

fn number_of(value: Option<&Value>) -> f64 {
    match value {
        None | Some(Value::Null) => 0.0,
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(f64::NAN),
        Some(_) => f64::NAN,
    }
}

pub fn read_engine_state(state: &Value) -> EngineView {
    let data = state.as_object().cloned().unwrap_or_default();
    let lifecycle = record(data.get("lifecycle"));
    let waiting_on = record(lifecycle.get("waitingOn"));
    let hold_kinds: Vec<String> = list(data.get("holds")).iter().map(kind_of).collect();
    let person_wait = list(waiting_on.get("human"))
        .iter()
        .map(kind_of)
        .any(|kind| kind != "stalled")
        || !list(data.get("pendingDialogs")).is_empty();
    let held = !hold_kinds.is_empty() || data.get("paused") == Some(&Value::Bool(true));
    let lifecycle_state = lifecycle.get("state").and_then(Value::as_str);
    let working = data.get("isStreaming") == Some(&Value::Bool(true))
        || matches!(
            lifecycle_state,
            Some("running" | "waiting_background" | "waiting_children")
        )
        || !list(waiting_on.get("background")).is_empty()
        || number_of(waiting_on.get("children")) > 0.0;
    let human_input_revision = lifecycle.get("humanInputRevision").and_then(|revision| {
        revision
            .as_i64()
            .or_else(|| revision.as_f64().map(|value| value as i64))
    });
    EngineView {
        held,
        hold_kinds,
        waiting_for_person: person_wait,
        working,
        finished: !(held || person_wait || working),
        human_input_revision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn given_a_session_that_ended_cleanly_then_it_reads_finished_with_its_input_revision() {
        let view = read_engine_state(&json!({
            "lifecycle": { "state": "done", "waitingOn": { "children": 0, "background": [], "human": [] }, "humanInputRevision": 7 },
            "holds": [], "paused": false, "isStreaming": false, "pendingDialogs": []
        }));
        assert!(view.finished);
        assert!(!(view.held || view.working || view.waiting_for_person));
        assert_eq!(view.human_input_revision, Some(7));
    }

    #[test]
    fn given_a_stop_or_a_host_stamp_then_it_reads_held_with_the_hold_kinds() {
        let stopped = read_engine_state(&json!({
            "lifecycle": { "state": "paused", "waitingOn": { "children": 0, "background": [], "human": [] } },
            "holds": [{ "kind": "session_hold", "key": "session_hold:1" }], "paused": true
        }));
        assert!(stopped.held);
        assert!(!stopped.finished);
        assert_eq!(stopped.hold_kinds, vec!["session_hold".to_owned()]);
        let paused_only = read_engine_state(&json!({ "paused": true }));
        assert!(paused_only.held);
        assert!(paused_only.hold_kinds.is_empty());
    }

    #[test]
    fn given_a_pending_dialog_or_a_question_then_a_person_is_waited_for_but_a_stalled_turn_is_finished()
     {
        let parked = read_engine_state(&json!({
            "lifecycle": { "state": "waiting_human", "waitingOn": { "children": 0, "background": [{ "source": "delivery" }], "human": [{ "kind": "question" }] } },
            "isStreaming": true,
            "pendingDialogs": [{ "method": "select" }]
        }));
        assert!(parked.waiting_for_person);
        assert!(!parked.finished);
        let stalled = read_engine_state(&json!({
            "lifecycle": { "state": "waiting_human", "waitingOn": { "children": 0, "background": [], "human": [{ "kind": "stalled" }] } }
        }));
        assert!(!stalled.waiting_for_person);
        assert!(stalled.finished);
    }

    #[test]
    fn given_background_work_or_children_then_the_session_is_working() {
        for state in [
            json!({ "lifecycle": { "state": "waiting_background", "waitingOn": { "background": [{ "source": "x" }] } } }),
            json!({ "lifecycle": { "state": "done", "waitingOn": { "children": 2 } } }),
            json!({ "isStreaming": true }),
            json!({ "lifecycle": { "state": "running" } }),
        ] {
            let view = read_engine_state(&state);
            assert!(view.working, "{state}");
            assert!(!view.finished, "{state}");
        }
    }
}
