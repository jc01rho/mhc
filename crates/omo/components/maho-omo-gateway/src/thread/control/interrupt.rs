//! The one interrupt body of every leg.
//!
//! Upstream counterpart: `thread/control/interrupt.ts`. This device's endpoint and another device's engine get the same
//! record and their answers are read by the same rule. An engine that predates the background stop answers no
//! `background`, which reads as `not_supported`: its turn stopped, and whatever that turn left running in the
//! background was not touched.

use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::contracts_results::{
    ThreadInterruptBackground, ThreadInterruptBackgroundJob, ThreadInterruptFailedJob,
};

/// What an interrupt answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptOutcome {
    pub interrupted: bool,
    pub turn_id: Option<String>,
    pub background: ThreadInterruptBackground,
}

/// The record's fields: the turn to stop when one is named, and the background stop.
pub fn interrupt_request(turn_id: Option<&str>) -> Map<String, Value> {
    let mut request = Map::new();
    if let Some(turn_id) = turn_id {
        request.insert("turnId".into(), json!(turn_id));
    }
    request.insert("background".into(), json!("stop"));
    request
}

fn job_of(entry: &Value) -> Option<ThreadInterruptBackgroundJob> {
    Some(ThreadInterruptBackgroundJob {
        kind: entry.get("kind")?.as_str()?.to_owned(),
        id: entry.get("id")?.as_str()?.to_owned(),
        description: entry
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn entries_of(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn background_of(value: Option<&Value>) -> ThreadInterruptBackground {
    let Some(background) = value.filter(|value| value.is_object()) else {
        return ThreadInterruptBackground::NotSupported;
    };
    ThreadInterruptBackground::Stopped {
        stopped: entries_of(background.get("stopped"))
            .iter()
            .filter_map(job_of)
            .collect(),
        failed: entries_of(background.get("failed"))
            .iter()
            .filter_map(|entry| {
                Some(ThreadInterruptFailedJob {
                    job: job_of(entry)?,
                    reason: entry
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                })
            })
            .collect(),
    }
}

/// Reads an engine's `interrupt` reply data.
pub fn interrupt_outcome(reply: &Value) -> InterruptOutcome {
    InterruptOutcome {
        interrupted: reply.get("interrupted").and_then(Value::as_bool) == Some(true),
        turn_id: reply
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        background: background_of(reply.get("background")),
    }
}

/// Sends one interrupt record through `send` and reads its answer.
pub async fn interrupt_over<E>(
    send: impl FnOnce(Map<String, Value>) -> BoxFuture<Result<Value, E>>,
    turn_id: Option<&str>,
) -> Result<InterruptOutcome, E> {
    let reply = send(interrupt_request(turn_id)).await?;
    Ok(interrupt_outcome(&reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_engine_without_the_background_stop_reads_as_not_supported() {
        let outcome = interrupt_outcome(&json!({ "interrupted": true, "turnId": "t-1" }));
        assert!(outcome.interrupted);
        assert_eq!(outcome.turn_id.as_deref(), Some("t-1"));
        assert_eq!(outcome.background, ThreadInterruptBackground::NotSupported);
    }

    #[test]
    fn stopped_and_failed_jobs_are_read_and_malformed_ones_skipped() {
        let outcome = interrupt_outcome(&json!({
            "interrupted": false,
            "background": {
                "stopped": [{ "kind": "bash", "id": "b-1", "description": "build" }, { "id": "no-kind" }],
                "failed": [{ "kind": "bash", "id": "b-2" }],
            },
        }));
        assert!(!outcome.interrupted);
        assert_eq!(
            outcome.background,
            ThreadInterruptBackground::Stopped {
                stopped: vec![ThreadInterruptBackgroundJob {
                    kind: "bash".into(),
                    id: "b-1".into(),
                    description: Some("build".into()),
                }],
                failed: vec![ThreadInterruptFailedJob {
                    job: ThreadInterruptBackgroundJob {
                        kind: "bash".into(),
                        id: "b-2".into(),
                        description: None,
                    },
                    reason: "unknown".into(),
                }],
            }
        );
    }

    #[test]
    fn the_record_names_the_turn_only_when_one_is_given() {
        assert_eq!(
            Value::Object(interrupt_request(Some("t-9"))),
            json!({ "turnId": "t-9", "background": "stop" })
        );
        assert_eq!(
            Value::Object(interrupt_request(None)),
            json!({ "background": "stop" })
        );
    }
}
