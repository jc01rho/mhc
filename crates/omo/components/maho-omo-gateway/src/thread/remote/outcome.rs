//! Cross-device command state, probe decision and refusal mapping (`thread/remote/outcome.ts`,
//! `relay-transport.ts`; omo #14320, #14333, #14428, senpi#7014).

use device_service_core::local_api::CommandOutcome;
use serde_json::{Map, Value, json};

use super::ports::{RemoteCommandOutcome, escape_for_display};
use super::remote_ops::not_signed_in_failure;
use super::{RemoteAddress, remote_failure};
use crate::thread::adapter::RuntimePhase;
use crate::thread::decision::{DeliveryDecision, Expect, TargetState, decide_delivery};
use crate::thread::errors::{
    ThreadToolFailure, pair_loop_guard_message, pair_loop_guard_next_action, thread_tool_failure,
};
use crate::thread::types::GatewayDeliveryMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEngineState {
    pub phase: RuntimePhase,
    pub turn_epoch: Option<i64>,
    pub pending: Option<i64>,
    pub parked_on_approval: bool,
    pub held: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedRemoteSend {
    pub method: &'static str,
    pub planned_lane: &'static str,
    pub expected_turn_id: Option<i64>,
}

fn non_empty_array(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
}

fn parse_turn_epoch(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) => number.as_i64(),
        Some(Value::String(text))
            if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) =>
        {
            text.parse::<i64>().ok()
        }
        _ => None,
    }
}

pub fn engine_state_of(reply: &Value) -> RemoteEngineState {
    let body = reply
        .get("state")
        .filter(|state| state.is_object())
        .unwrap_or(reply);
    let parked_on_approval = non_empty_array(body.get("pendingQuestions"))
        || non_empty_array(body.get("pendingDialogs"));
    let compacting = body.get("isCompacting").and_then(Value::as_bool) == Some(true);
    let streaming = body.get("isStreaming").and_then(Value::as_bool) == Some(true);
    let phase = if parked_on_approval {
        RuntimePhase::WaitingQuestion
    } else if compacting {
        RuntimePhase::Compacting
    } else if streaming {
        RuntimePhase::MidTurn
    } else {
        RuntimePhase::Idle
    };
    let turn_epoch = parse_turn_epoch(body.get("currentTurnId"));
    let pending = body
        .get("pendingMessageCount")
        .and_then(Value::as_i64)
        .filter(|count| *count >= 1);
    let held = body.get("held").and_then(Value::as_bool) == Some(true);
    RemoteEngineState {
        phase,
        turn_epoch,
        pending,
        parked_on_approval,
        held,
    }
}

pub fn plan_delivery(
    state: &RemoteEngineState,
    mode: GatewayDeliveryMode,
    expected_turn_id: Option<i64>,
    device: &str,
    session: &str,
) -> Result<PlannedRemoteSend, Value> {
    match decide_delivery(
        TargetState::Phase(state.phase),
        mode,
        expected_turn_id,
        state.turn_epoch,
    ) {
        DeliveryDecision::Admit {
            deliver_as: GatewayDeliveryMode::Steer,
            expected_turn_id,
            ..
        } => Ok(PlannedRemoteSend {
            method: "steer",
            planned_lane: "steer",
            expected_turn_id,
        }),
        DeliveryDecision::Admit {
            expect: Expect::Started,
            ..
        } if !state.held => Ok(PlannedRemoteSend {
            method: "prompt",
            planned_lane: "start",
            expected_turn_id: None,
        }),
        DeliveryDecision::Admit { .. } => Ok(PlannedRemoteSend {
            method: "follow_up",
            planned_lane: "follow_up",
            expected_turn_id: None,
        }),
        DeliveryDecision::Refuse { reason } => {
            let mut details = Map::new();
            details.insert("device".into(), Value::String(device.to_owned()));
            if let Some(epoch) = state.turn_epoch {
                details.insert("current_turn_id".into(), Value::String(epoch.to_string()));
            }
            let (code, message, next) = match reason.as_str() {
                "not_steerable" => (
                    "not_steerable",
                    format!("{session} on device {device} has no running turn to steer."),
                    "Send with delivery auto or follow_up; a steer needs a running turn.",
                ),
                _ => (
                    "turn_conflict",
                    format!(
                        "Turn {} is no longer the running turn of {session} on device {device}.",
                        expected_turn_id.map_or_else(|| "?".to_owned(), |id| id.to_string())
                    ),
                    "Read the target again and retry with its current turn id, or send with delivery auto.",
                ),
            };
            Err(remote_failure(
                code,
                message,
                next,
                Some(Value::Object(details)),
            ))
        }
        DeliveryDecision::QueuedOffline => Ok(PlannedRemoteSend {
            method: "follow_up",
            planned_lane: "follow_up",
            expected_turn_id: None,
        }),
    }
}

pub fn steer_conflict(outcome: &CommandOutcome) -> Option<(&'static str, Option<String>)> {
    if outcome.state == "engine_applied" {
        let receipt = outcome.receipt.as_ref()?;
        let kind = receipt
            .get("kind")
            .or_else(|| receipt.get("disposition"))
            .and_then(Value::as_str);
        if kind == Some("turn_conflict") {
            let turn_id = receipt
                .get("turnId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned);
            return Some(("turn_conflict", turn_id));
        }
        return None;
    }
    if outcome.state == "engine_refused" {
        return match outcome.reason.as_deref() {
            Some("turn_conflict") => Some(("turn_conflict", None)),
            Some("not_steerable") => Some(("not_steerable", None)),
            _ => None,
        };
    }
    None
}

const UNANSWERED_REMOTE_REASONS: &[&str] = &[
    "closed",
    "protocol_violation",
    "internal",
    "read_failed",
    "request_timeout",
    "handshake_timeout",
    "attach_failed",
    "reply_lost",
    "delivered_unconfirmed",
    "host_not_answering",
    "peer_not_found",
    "engine_stale",
    "request_gone",
    "no_receipt",
    "engine_restarted",
    "recipient_revoked",
    "transport_expired",
    "unconfirmed",
    "lease_expired",
    "cancelled",
    "hand_restarted",
    "result_expired",
    "result_undecodable",
];

fn chain_count_limit_failure(device: &str) -> Box<ThreadToolFailure> {
    Box::new(thread_tool_failure(
        "chain_count_limit",
        format!(
            "Device {device} refused this thread call: the chain has made more hops than its user's thread chain limit (threads.chainCountLimit on device {device})."
        ),
        format!(
            "Do not retry or extend this chain; finish with what you have. Only the user of device {device} can allow more hops, by raising or unsetting threads.chainCountLimit there."
        ),
        Some(json!({
            "device": device,
            "reason": "chain_count_limit",
            "setting": "threads.chainCountLimit",
        })),
    ))
}

/// Maps a [`RemoteCommandOutcome`] failure (`Refused` / `Uncertain`) into a [`ThreadToolFailure`], escaping any
/// free-form reason text from the other device (#13304, `thread/remote/outcome.ts` `outcomeFailure`).
pub fn relay_outcome_failure(
    outcome: &RemoteCommandOutcome,
    device: &str,
) -> Box<ThreadToolFailure> {
    let raw_reason = match outcome {
        RemoteCommandOutcome::Refused { reason, .. }
        | RemoteCommandOutcome::Uncertain { reason } => reason.as_str(),
        RemoteCommandOutcome::Ok { .. } => "internal",
    };
    let reason = escape_for_display(raw_reason);
    if matches!(outcome, RemoteCommandOutcome::Uncertain { .. }) {
        if raw_reason == "delivered_unconfirmed" {
            return Box::new(thread_tool_failure(
                "delivered_unconfirmed",
                format!(
                    "The command left this device and the relay accepted it for device {device}, but no answer came back in time (delivered_unconfirmed)."
                ),
                "Do not resend it: it may have run, or may still run. Read the thread to see whether it did.",
                Some(json!({ "device": device, "reason": reason })),
            ));
        }
        return Box::new(thread_tool_failure(
            "idempotency_uncertain",
            format!("The command to device {device} may or may not have run ({reason})."),
            "Read the thread before deciding whether to retry.",
            Some(json!({ "device": device, "reason": reason })),
        ));
    }
    if let Some(not_signed_in) = not_signed_in_failure(raw_reason, device) {
        return not_signed_in;
    }
    match raw_reason {
        "capability_missing" => Box::new(thread_tool_failure(
            "scope_denied",
            format!("Device {device} does not grant this command."),
            "Allow commands from this device on the other device, then retry.",
            Some(json!({ "device": device, "reason": reason })),
        )),
        "host_detached" => Box::new(thread_tool_failure(
            "host_unavailable",
            format!("Device {device} is not attached to the relay."),
            "Retry when that device is signed in and online.",
            Some(json!({ "device": device, "reason": reason })),
        )),
        "no_primary_endpoint" => Box::new(thread_tool_failure(
            "host_unavailable",
            format!(
                "Device {device} does not list its own engine, so nothing was opened (no_primary_endpoint)."
            ),
            "Start or restart omo on that device; if that does not help, update the device service on both devices. Then retry.",
            Some(json!({
                "device": device,
                "reason": "no_primary_endpoint",
                "remedy": "restart_omo_or_update_device_service",
            })),
        )),
        r if UNANSWERED_REMOTE_REASONS.contains(&r) => Box::new(thread_tool_failure(
            "host_unavailable",
            format!("Device {device} did not answer ({reason}); this call changed nothing there."),
            "Retry; nothing needs to be checked first.",
            Some(json!({ "device": device, "reason": reason })),
        )),
        "unknown_session" | "session_not_found" | "route_unlisted" => {
            Box::new(thread_tool_failure(
                "not_resumable",
                format!("The thread is not running on device {device} ({raw_reason})."),
                "Call thread_list (tool.thread_list() in an eval cell) to see the thread's current state, then retry when it runs again.",
                Some(json!({ "device": device, "reason": raw_reason })),
            ))
        }
        "chain_count_limit" => chain_count_limit_failure(device),
        "turn_conflict" => Box::new(thread_tool_failure(
            "turn_conflict",
            format!("Device {device} refused the steer: turn_conflict."),
            "Read the target again and steer with its current turn_id, or send with delivery follow_up.",
            Some(json!({ "device": device, "reason": "turn_conflict" })),
        )),
        "not_steerable" => Box::new(thread_tool_failure(
            "not_steerable",
            format!("Device {device} refused the steer: not_steerable."),
            "Send with delivery auto or follow_up; a steer needs a running turn.",
            Some(json!({ "device": device, "reason": "not_steerable" })),
        )),
        "record_too_large" => Box::new(thread_tool_failure(
            "unsupported",
            format!(
                "The thread on device {device} is too large to send back in one relay record (record_too_large)."
            ),
            "Read it on that device, or ask that thread for a summary with thread_send (tool.thread_send() in an eval cell).",
            Some(json!({ "device": device, "reason": reason })),
        )),
        _ => Box::new(thread_tool_failure(
            "unsupported",
            format!("Device {device} refused the command ({reason})."),
            "Update omo on that device, or address a thread on this device.",
            Some(json!({ "device": device, "reason": reason })),
        )),
    }
}

pub fn outcome_failure(
    address: &RemoteAddress,
    device: &str,
    outcome: &CommandOutcome,
    expected_turn_id: Option<i64>,
) -> Value {
    if let Some((conflict, current_turn_id)) = steer_conflict(outcome) {
        let mut details = Map::new();
        details.insert("device".into(), Value::String(device.to_owned()));
        if let Some(turn_id) = current_turn_id {
            details.insert("current_turn_id".into(), Value::String(turn_id));
        }
        return if conflict == "turn_conflict" {
            remote_failure(
                "turn_conflict",
                format!(
                    "Turn {} is no longer the running turn of {} on device {device}.",
                    expected_turn_id.map_or_else(|| "?".to_owned(), |id| id.to_string()),
                    address.session
                ),
                "Read the target again and retry with its current turn id, or send with delivery auto.",
                Some(Value::Object(details)),
            )
        } else {
            remote_failure(
                "not_steerable",
                format!(
                    "A steer to {} on device {device} was refused: not_steerable.",
                    address.session
                ),
                "Send with delivery auto or follow_up; a steer needs a running turn.",
                Some(Value::Object(details)),
            )
        };
    }

    let reason = outcome.reason.clone().unwrap_or_default();
    match outcome.state.as_str() {
        "engine_refused" => match reason.as_str() {
            "route_unlisted" => remote_failure(
                "not_resumable",
                format!(
                    "{}/{} is no longer listed on its device",
                    address.device, address.session
                ),
                "Call thread_list for the sessions that device runs now.",
                Some(json!({ "reason": reason })),
            ),
            "peer_unauthorized" => remote_failure(
                "scope_denied",
                format!(
                    "{}/{} refused the message ({reason})",
                    address.device, address.session
                ),
                "Ask the person on that device to turn remote control on (mhc remote control on).",
                Some(json!({ "reason": reason })),
            ),
            "pair_loop_guard" => {
                let data = outcome.receipt.as_ref();
                let burst = data.and_then(|d| d.get("burst")).and_then(Value::as_i64);
                let window_ms = data
                    .and_then(|d| d.get("window_ms"))
                    .and_then(Value::as_i64);
                let retry_after_ms = data
                    .and_then(|d| d.get("retry_after_ms"))
                    .and_then(Value::as_i64);
                let message = match (burst, window_ms) {
                    (Some(burst), Some(window_ms)) => pair_loop_guard_message(
                        "this device",
                        &format!("the session on device {device}"),
                        burst,
                        window_ms,
                    ),
                    _ => format!(
                        "Device {device} held this send back: this device sent that session its whole burst; it was not delivered."
                    ),
                };
                let next_action = match retry_after_ms {
                    Some(retry) => pair_loop_guard_next_action(retry),
                    None => "This looks like a send loop between these two sessions; wait before sending to that session again.".to_owned(),
                };
                let mut details = Map::new();
                details.insert("device".into(), Value::String(device.to_owned()));
                details.insert("guard".into(), Value::String("pair_loop".into()));
                if let Some(burst) = burst {
                    details.insert("burst".into(), json!(burst));
                }
                if let Some(window_ms) = window_ms {
                    details.insert("window_ms".into(), json!(window_ms));
                }
                if let Some(retry) = retry_after_ms {
                    details.insert("retry_after_ms".into(), json!(retry));
                }
                remote_failure(
                    "pair_loop_guard",
                    message,
                    &next_action,
                    Some(Value::Object(details)),
                )
            }
            "chain_count_limit" => remote_failure(
                "chain_count_limit",
                format!("Device {device} stopped the send: the delivery hop limit was reached."),
                "Break the cycle or start a new task instead of forwarding again.",
                Some(json!({ "device": device, "reason": reason })),
            ),
            "message_too_long" | "record_too_large" => remote_failure(
                "message_too_large",
                format!("The message exceeded device {device}'s limit ({reason})."),
                "Send a shorter message.",
                Some(json!({ "device": device, "reason": reason })),
            ),
            "method_unclassified"
            | "method_local_only"
            | "unsupported"
            | "engine_too_old"
            | "capability_missing" => remote_failure(
                "unsupported",
                format!("Device {device} cannot take this message ({reason})."),
                "Update omo and mhc on that device, then retry.",
                Some(json!({ "device": device, "reason": reason })),
            ),
            _ => remote_failure(
                "scope_denied",
                format!(
                    "{}/{} refused the message ({reason})",
                    address.device, address.session
                ),
                "Ask the person on that device to turn remote control on (mhc remote control on).",
                Some(json!({ "reason": reason })),
            ),
        },
        "not delivered: never_sent" | "not confirmed" => remote_failure(
            "host_unavailable",
            format!("{} did not answer; nothing was sent", address.device),
            "Start this device's device service (mhc remote daemon) and retry.",
            None,
        ),
        _ => remote_failure(
            "idempotency_uncertain",
            format!(
                "the message to {}/{} may or may not have arrived ({reason})",
                address.device, address.session
            ),
            "Read the session before sending again.",
            Some(json!({ "command_id": outcome.command_id })),
        ),
    }
}

pub fn result_of(
    address: &RemoteAddress,
    device: &str,
    outcome: &CommandOutcome,
    method: &str,
    planned_lane: &str,
    expected_turn_id: Option<i64>,
) -> Value {
    if steer_conflict(outcome).is_some() || outcome.state != "engine_applied" {
        return outcome_failure(address, device, outcome, expected_turn_id);
    }
    let receipt = outcome.receipt.as_ref();
    let kind = receipt
        .and_then(|value| value.get("kind").or_else(|| value.get("disposition")))
        .and_then(Value::as_str)
        .unwrap_or("handled");
    let turn_id = receipt
        .and_then(|value| value.get("turnId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    match kind {
        "held" => json!({
            "kind": "ok",
            "thread": address.session,
            "device": device,
            "delivery": {
                "kind": "queued",
                "queue_position": 1,
                "waiting_on": { "kind": "hold", "device": device },
            },
            "effective_mode": "follow_up",
            "command_id": outcome.command_id,
        }),
        "started" => json!({
            "kind": "ok",
            "thread": address.session,
            "device": device,
            "delivery": { "kind": "started", "turn_id": turn_id },
            "effective_mode": "start",
            "command_id": outcome.command_id,
        }),
        "steered" => json!({
            "kind": "ok",
            "thread": address.session,
            "device": device,
            "delivery": { "kind": "steered", "turn_id": turn_id },
            "effective_mode": "steer",
            "command_id": outcome.command_id,
        }),
        "queued" => json!({
            "kind": "ok",
            "thread": address.session,
            "device": device,
            "delivery": { "kind": "queued", "queue_position": 1 },
            "effective_mode": "follow_up",
            "command_id": outcome.command_id,
        }),
        "already_admitted" => json!({
            "kind": "ok",
            "thread": address.session,
            "device": device,
            "delivery": { "kind": "queued", "queue_position": 1 },
            "deduplicated": true,
            "effective_mode": "follow_up",
            "command_id": outcome.command_id,
        }),
        "handled" if method == "steer" => json!({
            "kind": "ok",
            "thread": address.session,
            "device": device,
            "delivery": { "kind": "steered", "turn_id": turn_id },
            "effective_mode": "steer",
            "command_id": outcome.command_id,
        }),
        other => json!({
            "kind": "ok",
            "thread": address.session,
            "device": device,
            "delivery": { "kind": other },
            "effective_mode": planned_lane,
            "command_id": outcome.command_id,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> RemoteAddress {
        RemoteAddress {
            device: "b".into(),
            session: "s-1".into(),
        }
    }

    fn applied(receipt: Value) -> CommandOutcome {
        CommandOutcome {
            command_id: "cmd-1".into(),
            state: "engine_applied".into(),
            reason: None,
            retryable: false,
            receipt: Some(receipt),
        }
    }

    fn refused(reason: &str, receipt: Option<Value>) -> CommandOutcome {
        CommandOutcome {
            command_id: "cmd-1".into(),
            state: "engine_refused".into(),
            reason: Some(reason.into()),
            retryable: false,
            receipt,
        }
    }

    #[test]
    fn engine_state_distinguishes_approval_compacting_mid_turn_and_idle() {
        let idle = engine_state_of(&json!({ "state": { "isStreaming": false } }));
        assert_eq!(idle.phase, RuntimePhase::Idle);
        let mid = engine_state_of(&json!({
            "state": { "isStreaming": true, "currentTurnId": "7", "pendingMessageCount": 2 }
        }));
        assert_eq!(mid.phase, RuntimePhase::MidTurn);
        assert_eq!(mid.turn_epoch, Some(7));
        assert_eq!(mid.pending, Some(2));
        let parked = engine_state_of(&json!({
            "state": { "isStreaming": true, "pendingQuestions": [{ "id": "q1" }] }
        }));
        assert_eq!(parked.phase, RuntimePhase::WaitingQuestion);
        let compacting = engine_state_of(&json!({
            "state": { "isStreaming": true, "isCompacting": true }
        }));
        assert_eq!(compacting.phase, RuntimePhase::Compacting);
    }

    #[test]
    fn plan_delivery_picks_steer_start_and_follow_up_across_states() {
        let mid =
            engine_state_of(&json!({ "state": { "isStreaming": true, "currentTurnId": "3" } }));
        let auto_steer = plan_delivery(&mid, GatewayDeliveryMode::Auto, None, "b", "s-1").unwrap();
        assert_eq!(auto_steer.method, "steer");
        assert_eq!(auto_steer.planned_lane, "steer");
        assert_eq!(auto_steer.expected_turn_id, Some(3));

        let idle = engine_state_of(&json!({ "state": { "isStreaming": false } }));
        let auto_start = plan_delivery(&idle, GatewayDeliveryMode::Auto, None, "b", "s-1").unwrap();
        assert_eq!(auto_start.method, "prompt");
        assert_eq!(auto_start.planned_lane, "start");

        let explicit_steer_idle =
            plan_delivery(&idle, GatewayDeliveryMode::Steer, Some(3), "b", "s-1").unwrap_err();
        assert_eq!(explicit_steer_idle["error"]["code"], "not_steerable");

        let waiting = engine_state_of(&json!({
            "state": { "isStreaming": true, "pendingQuestions": [{ "id": "q1" }] }
        }));
        let waiting_plan =
            plan_delivery(&waiting, GatewayDeliveryMode::Auto, None, "b", "s-1").unwrap();
        assert_eq!(waiting_plan.method, "follow_up");
        assert_eq!(waiting_plan.planned_lane, "follow_up");

        let compacting = engine_state_of(&json!({
            "state": { "isStreaming": true, "isCompacting": true }
        }));
        let compacting_steer =
            plan_delivery(&compacting, GatewayDeliveryMode::Steer, Some(3), "b", "s-1")
                .unwrap_err();
        assert_eq!(compacting_steer["error"]["code"], "not_steerable");
    }

    #[test]
    fn result_of_maps_held_started_queued_pair_loop_guard_and_steer_conflicts() {
        let held = result_of(
            &addr(),
            "b",
            &applied(json!({ "kind": "held", "holds": ["session_hold"] })),
            "prompt",
            "start",
            None,
        );
        assert_eq!(held["kind"], "ok");
        assert_eq!(held["effective_mode"], "follow_up");
        assert_eq!(held["delivery"]["kind"], "queued");
        assert_eq!(held["delivery"]["waiting_on"]["kind"], "hold");
        assert_eq!(held["delivery"]["waiting_on"]["device"], "b");

        let started = result_of(
            &addr(),
            "b",
            &applied(json!({ "disposition": "started", "turnId": "9" })),
            "prompt",
            "start",
            None,
        );
        assert_eq!(started["effective_mode"], "start");
        assert_eq!(started["delivery"]["kind"], "started");

        let already = result_of(
            &addr(),
            "b",
            &applied(json!({ "kind": "already_admitted" })),
            "follow_up",
            "follow_up",
            None,
        );
        assert_eq!(already["deduplicated"], true);
        assert_eq!(already["effective_mode"], "follow_up");

        let conflict = result_of(
            &addr(),
            "b",
            &applied(json!({ "kind": "turn_conflict", "turnId": "4" })),
            "steer",
            "steer",
            Some(3),
        );
        assert_eq!(conflict["kind"], "error");
        assert_eq!(conflict["error"]["code"], "turn_conflict");
        assert_eq!(conflict["error"]["details"]["current_turn_id"], "4");

        let refused_not_steerable = result_of(
            &addr(),
            "b",
            &refused("not_steerable", None),
            "steer",
            "steer",
            Some(3),
        );
        assert_eq!(refused_not_steerable["error"]["code"], "not_steerable");

        let loop_guard = result_of(
            &addr(),
            "b",
            &refused(
                "pair_loop_guard",
                Some(json!({ "burst": 8, "window_ms": 10_000, "retry_after_ms": 5_000 })),
            ),
            "prompt",
            "start",
            None,
        );
        assert_eq!(loop_guard["error"]["code"], "pair_loop_guard");
        assert_eq!(loop_guard["error"]["details"]["guard"], "pair_loop");
        assert_eq!(loop_guard["error"]["details"]["burst"], 8);

        let unlisted = result_of(
            &addr(),
            "b",
            &refused("route_unlisted", None),
            "prompt",
            "start",
            None,
        );
        assert_eq!(unlisted["error"]["code"], "not_resumable");

        let chain_limit = result_of(
            &addr(),
            "b",
            &refused("chain_count_limit", None),
            "prompt",
            "start",
            None,
        );
        assert_eq!(chain_limit["error"]["code"], "chain_count_limit");

        let too_large = result_of(
            &addr(),
            "b",
            &refused("record_too_large", None),
            "prompt",
            "start",
            None,
        );
        assert_eq!(too_large["error"]["code"], "message_too_large");

        let unauthorized = result_of(
            &addr(),
            "b",
            &refused("peer_unauthorized", None),
            "prompt",
            "start",
            None,
        );
        assert_eq!(unauthorized["error"]["code"], "scope_denied");
    }

    #[test]
    fn relay_outcome_failure_escapes_free_reason_text_and_maps_unanswered_and_gone() {
        let free = RemoteCommandOutcome::Refused {
            reason: "bad\u{202e}line\n\x1b[31m".to_owned(),
            detail: None,
        };
        let mapped = relay_outcome_failure(&free, "b");
        assert_eq!(mapped.code.as_str(), "unsupported");
        assert!(mapped.message.contains("\\u202e"));
        assert!(!mapped.message.contains('\n'));
        assert_eq!(
            mapped.details.as_ref().and_then(|d| d["reason"].as_str()),
            Some("bad\\u202eline\\u000a\\u001b[31m")
        );

        let lost = RemoteCommandOutcome::Uncertain {
            reason: "delivered_unconfirmed".to_owned(),
        };
        assert_eq!(
            relay_outcome_failure(&lost, "b").code.as_str(),
            "delivered_unconfirmed"
        );

        let gone = RemoteCommandOutcome::Refused {
            reason: "route_unlisted".to_owned(),
            detail: None,
        };
        assert_eq!(
            relay_outcome_failure(&gone, "b").code.as_str(),
            "not_resumable"
        );
    }
}
