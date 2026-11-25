//! A delivery result read back from its row.
//!
//! Upstream counterpart: `thread/gateway/result.ts`.

use super::errors::{ThreadToolFailure, pair_loop_guard_next_action, thread_tool_failure};
use super::types::{
    DeliveryRow, DeliveryState, GatewayDeliveryDelivery, GatewayDeliveryResult, GatewayEndpointKind,
};

/// How a refused row's reason maps onto the tool taxonomy.
pub fn refusal_mapping(reason: Option<&str>) -> (&'static str, &'static str) {
    match reason {
        Some("not_steerable") => (
            "not_steerable",
            "Send with delivery auto or follow_up; a steer needs a running turn.",
        ),
        Some("turn_conflict") => (
            "turn_conflict",
            "Read the target again and retry with its current turn id, or send with delivery auto.",
        ),
        Some("expired") => (
            "not_resumable",
            "The target did not take the message within its lifetime; send it again if it still matters.",
        ),
        Some("sender_not_admitted") => (
            "not_resumable",
            "The sending device is no longer admitted to this account; sign it in again, then send again.",
        ),
        Some("target_closed") => (
            "not_resumable",
            "The thread was closed before it read this message; reopen it and send again if it still matters.",
        ),
        Some("superseded") => (
            "not_resumable",
            "A newer message from the same sender with the same collapse_key replaced this one before the target read it; nothing to do.",
        ),
        Some("binding_closed") => (
            "invalid_arguments",
            "The binding this message was queued under was detached or rebound; re-read the binding and send again.",
        ),
        _ => ("internal_error", "Call thread_list and check the target."),
    }
}

fn error(
    code: &str,
    message: String,
    next_action: &str,
    delivery_id: &str,
) -> GatewayDeliveryResult {
    GatewayDeliveryResult::Error {
        error: ThreadToolFailure {
            code: super::errors::ThreadErrorCode::parse(code)
                .unwrap_or_else(|| panic!("unknown thread error code: {code}")),
            message,
            next_action: next_action.to_owned(),
            details: Some(serde_json::json!({ "delivery_id": delivery_id })),
        },
    }
}

/// `resultFromRow`: the outcome the sender reads, read back from the target's row.
pub fn result_from_row(
    row: &DeliveryRow,
    queue_position: i64,
    endpoint_kind: Option<GatewayEndpointKind>,
    deduplicated: bool,
    offline: bool,
) -> GatewayDeliveryResult {
    let effective_mode = row
        .mode_effective
        .map(|lane| lane.as_str().to_owned())
        .unwrap_or_else(|| row.mode_requested.as_str().to_owned());
    let ok = |delivery: GatewayDeliveryDelivery| GatewayDeliveryResult::Ok {
        thread_id: row.target_durable_id.clone(),
        delivery_id: row.delivery_id.clone(),
        message_seq: row.seq,
        delivery,
        effective_mode: effective_mode.clone(),
        endpoint_kind,
        deduplicated,
        collapsed: false,
    };
    if matches!(row.state, DeliveryState::Admitted | DeliveryState::Applied) {
        return match row.admission_kind.as_deref() {
            Some("started") => ok(GatewayDeliveryDelivery::Started {
                turn_id: (row.turn_epoch.unwrap_or(0)).to_string(),
            }),
            Some("steered") => ok(GatewayDeliveryDelivery::Steered {
                turn_id: (row.turn_epoch.unwrap_or(0)).to_string(),
            }),
            _ => ok(GatewayDeliveryDelivery::Queued {
                queue_position: queue_position.max(1),
            }),
        };
    }
    if row.state == DeliveryState::Refused {
        let (code, next) = refusal_mapping(row.reason.as_deref());
        return error(
            code,
            format!(
                "Delivery {} was refused: {}.",
                row.delivery_id,
                row.reason.as_deref().unwrap_or("unknown")
            ),
            next,
            &row.delivery_id,
        );
    }
    if row.state == DeliveryState::Uncertain {
        return error(
            "idempotency_uncertain",
            format!(
                "Delivery {} may or may not have reached the target.",
                row.delivery_id
            ),
            "Read the target transcript before deciding whether to send again.",
            &row.delivery_id,
        );
    }
    if offline {
        return ok(GatewayDeliveryDelivery::QueuedOffline {
            queue_position: queue_position.max(1),
        });
    }
    ok(GatewayDeliveryDelivery::Queued {
        queue_position: queue_position.max(1),
    })
}

/// The next action for a store refusal code, as the engine reports it.
pub fn engine_next_action(code: &str) -> &'static str {
    match code {
        "loop_detected" => {
            "Stop relaying here: a direct reply to the session that messaged you is a cycle. That session reads your answer with thread_read; a bound external thread gets it through thread_report, and its questions come back through thread_answer."
        }
        "overloaded" => "Wait before sending to this session again.",
        "queue_full" => {
            "Read details.reason: the target is not taking its queue. A steer has its own reserve; send repeats with one collapse_key so each replaces the last."
        }
        "idempotency_conflict" => "Retry with a new idempotency_key.",
        "idempotency_in_progress" => "Wait for the earlier call to settle, then retry.",
        "idempotency_uncertain" => {
            "Read the target transcript before deciding whether to send again; the gateway never resends."
        }
        "invalid_arguments" => "Fix the arguments and send again.",
        _ => "Call thread_list and retry after checking the target.",
    }
}

/// What to do about a full backlog whose target nobody runs: nothing drains it until the target resumes (omo#14466).
const QUEUE_FULL_NOT_RUNNING: &str = "The target is not running, so nothing takes its queue until it resumes. Resume it, or send again after details.frees_at, when its oldest queued message expires and frees a slot.";

fn detail<'a>(details: Option<&'a serde_json::Value>, key: &str) -> Option<&'a serde_json::Value> {
    details.and_then(|details| details.get(key))
}

/// The failure an engine reports for a code. A refusal whose details say how to recover gets the next action that
/// reads them: the pair loop guard's wait, and the full backlog of a target nobody runs.
pub fn engine_failure(
    code: &str,
    message: impl Into<String>,
    details: Option<serde_json::Value>,
) -> ThreadToolFailure {
    let next_action = match code {
        "pair_loop_guard" => pair_loop_guard_next_action(
            detail(details.as_ref(), "retry_after_ms")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
        ),
        "queue_full"
            if detail(details.as_ref(), "target_running").and_then(serde_json::Value::as_bool)
                == Some(false) =>
        {
            QUEUE_FULL_NOT_RUNNING.to_owned()
        }
        _ => engine_next_action(code).to_owned(),
    };
    thread_tool_failure(code, message, next_action, details)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::types::{EnvelopeOrigin, GatewayDeliveryMode, StoredDeliveryEnvelope};

    fn row(
        state: DeliveryState,
        admission_kind: Option<&str>,
        turn_epoch: Option<i64>,
    ) -> DeliveryRow {
        DeliveryRow {
            delivery_id: "d-1".into(),
            target_durable_id: "receiver".into(),
            seq: 3,
            sender: "session:sender".into(),
            sender_turn: None,
            envelope: StoredDeliveryEnvelope {
                provenance: None,
                origin: EnvelopeOrigin::Session {
                    session: "sender".into(),
                },
                actor: "planner".into(),
                via: vec![],
                root_id: "root".into(),
                hop: 1,
                delivery_id: "d-1".into(),
            },
            body: "hi".into(),
            bytes: 2,
            mode_requested: GatewayDeliveryMode::Auto,
            mode_effective: None,
            expected_turn_id: None,
            state,
            reason: None,
            admitted_by: None,
            claimed_at: None,
            attempt: 0,
            admission_kind: admission_kind.map(str::to_owned),
            wait_kind: None,
            turn_epoch,
            root_id: "root".into(),
            hop: 1,
            created_at: 1,
            updated_at: 1,
            expires_at: 9,
            binding_id: None,
            binding_revision: None,
            actor_user_id: None,
            ledger_id: None,
            verbatim_text: None,
            verbatim_display_text: None,
            collapse_key: None,
        }
    }

    #[test]
    fn a_turn_begun_on_an_idle_session_reports_the_start_lane() {
        let mut started = row(DeliveryState::Admitted, Some("started"), Some(2));
        started.mode_effective = Some(crate::thread::types::EffectiveLane::Start);
        match result_from_row(&started, 1, None, false, false) {
            GatewayDeliveryResult::Ok {
                delivery,
                effective_mode,
                collapsed,
                ..
            } => {
                assert_eq!(
                    delivery,
                    GatewayDeliveryDelivery::Started {
                        turn_id: "2".into()
                    }
                );
                assert_eq!(effective_mode, "start");
                assert!(!collapsed);
            }
            other => panic!("expected ok, got {other:?}"),
        }
    }

    #[test]
    fn a_row_a_newer_collapse_key_repeat_replaced_reads_not_resumable() {
        let mut superseded = row(DeliveryState::Refused, None, None);
        superseded.reason = Some("superseded".into());
        match result_from_row(&superseded, 0, None, true, false) {
            GatewayDeliveryResult::Error { error } => {
                assert_eq!(error.code.as_str(), "not_resumable");
                assert!(error.message.contains("was refused: superseded"));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn an_admitted_row_reports_its_lane() {
        match result_from_row(
            &row(DeliveryState::Admitted, Some("steered"), Some(7)),
            1,
            Some(GatewayEndpointKind::RpcHost),
            false,
            false,
        ) {
            GatewayDeliveryResult::Ok {
                delivery,
                message_seq,
                effective_mode,
                ..
            } => {
                assert_eq!(
                    delivery,
                    GatewayDeliveryDelivery::Steered {
                        turn_id: "7".into()
                    }
                );
                assert_eq!(message_seq, 3);
                assert_eq!(effective_mode, "auto");
            }
            other => panic!("expected ok, got {other:?}"),
        }
    }

    #[test]
    fn a_queued_row_reports_a_position_of_at_least_one() {
        match result_from_row(
            &row(DeliveryState::Queued, None, None),
            0,
            None,
            false,
            true,
        ) {
            GatewayDeliveryResult::Ok { delivery, .. } => assert_eq!(
                delivery,
                GatewayDeliveryDelivery::QueuedOffline { queue_position: 1 }
            ),
            other => panic!("expected ok, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_row_maps_its_reason() {
        let mut refused = row(DeliveryState::Refused, None, None);
        refused.reason = Some("expired".into());
        match result_from_row(&refused, 0, None, false, false) {
            GatewayDeliveryResult::Error { error } => {
                assert_eq!(error.code.as_str(), "not_resumable");
                assert!(error.message.contains("was refused: expired"));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn a_row_its_closed_session_handed_back_reads_not_delivered() {
        let mut returned = row(DeliveryState::Refused, Some("queued"), Some(1));
        returned.reason = Some("target_closed".into());
        match result_from_row(&returned, 0, None, false, false) {
            GatewayDeliveryResult::Error { error } => {
                assert_eq!(error.code.as_str(), "not_resumable");
                assert!(error.message.contains("was refused: target_closed"));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn a_refusal_that_names_its_recovery_gets_the_next_action_reading_it() {
        let guard = engine_failure(
            "pair_loop_guard",
            "loop",
            Some(serde_json::json!({ "retry_after_ms": 5_000 })),
        );
        assert_eq!(guard.next_action, pair_loop_guard_next_action(5_000));
        let stopped = engine_failure(
            "queue_full",
            "full",
            Some(serde_json::json!({ "target_running": false })),
        );
        assert_eq!(stopped.next_action, QUEUE_FULL_NOT_RUNNING);
        let running = engine_failure(
            "queue_full",
            "full",
            Some(serde_json::json!({ "target_running": true })),
        );
        assert_eq!(running.next_action, engine_next_action("queue_full"));
    }

    #[test]
    fn an_uncertain_row_is_never_resent() {
        match result_from_row(
            &row(DeliveryState::Uncertain, None, None),
            0,
            None,
            false,
            false,
        ) {
            GatewayDeliveryResult::Error { error } => {
                assert_eq!(error.code.as_str(), "idempotency_uncertain")
            }
            other => panic!("expected error, got {other:?}"),
        }
    }
}
