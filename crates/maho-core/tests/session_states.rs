use std::collections::BTreeMap;

use maho_core::agent_session::describe_compaction_rejection;
use maho_core::engine_turn_hold::{HoldKind, INTERRUPTED_RUN_HOLD_KEY, SessionHold};
use maho_core::host_restart_continuation::{
    HOST_DRAIN_HOLD_KEY, RESTART_CONTINUATION_LIMIT, RestartHoldReason, RestartRelease,
    SESSION_CONTROL_DELIVERY_TYPE, engine_started_last_run, restart_hold_data, restart_release,
};
use maho_core::messages::{
    convert_to_llm, sanitize_legacy_wire_turn_id, tool_result_is_error, tool_result_value_is_error,
};
use maho_core::session_archived::{archived_flag_of, is_session_archived, set_session_archived};
use maho_core::session_manager::{NewSessionOptions, SessionManager};
use maho_core::session_settled::{
    NOT_SETTLED, SessionOperatingState, SessionSettled, ThreadSettledBy, parse_session_settled,
    read_session_settled, resolve_session_operating_state, set_session_settled, settled_fields,
    settled_state_of,
};
use maho_core::stop_resume_directive::STOP_RESUME_CUSTOM_TYPE;
use maho_ext_api::{AgentToolResult, CompactionRejectionCause};
use serde_json::{Value, json};

fn hold(kind: HoldKind, key: &str) -> SessionHold {
    SessionHold {
        kind,
        key: key.to_owned(),
        since: "2026-11-19T00:00:00.000Z".to_owned(),
        data: None,
        source_counts: BTreeMap::new(),
    }
}

#[test]
fn session_operating_state_transitions_between_active_held_settled_and_archived() {
    let mut manager = SessionManager::in_memory("/workspace");
    assert_eq!(
        manager.operating_state(false),
        SessionOperatingState::Active
    );
    assert_eq!(manager.operating_state(true), SessionOperatingState::Held);
    assert!(!is_session_archived(&manager));
    assert_eq!(read_session_settled(&manager), NOT_SETTLED);

    let settled_state = SessionSettled {
        settled: true,
        by: Some(ThreadSettledBy::PrMerged {
            pull_request_number: Some(42),
        }),
    };
    let settled_entry = set_session_settled(&mut manager, &settled_state);
    assert_eq!(
        settled_state_of(&settled_entry, manager.session_id()),
        Some(settled_state.clone())
    );
    assert_eq!(read_session_settled(&manager), settled_state);
    assert_eq!(
        settled_fields(&manager.get_settled()).settled_by,
        Some(ThreadSettledBy::PrMerged {
            pull_request_number: Some(42),
        })
    );
    assert_eq!(
        manager.operating_state(false),
        SessionOperatingState::Settled
    );
    assert_eq!(
        manager.operating_state(true),
        SessionOperatingState::Settled
    );

    let archived_entry = set_session_archived(&mut manager, true);
    assert_eq!(
        archived_flag_of(&archived_entry, manager.session_id()),
        Some(true)
    );
    assert!(is_session_archived(&manager));
    assert_eq!(
        manager.operating_state(false),
        SessionOperatingState::Archived
    );

    set_session_archived(&mut manager, false);
    assert_eq!(
        manager.operating_state(false),
        SessionOperatingState::Settled
    );
    set_session_settled(&mut manager, &NOT_SETTLED);
    assert_eq!(
        manager.operating_state(false),
        SessionOperatingState::Active
    );
    assert_eq!(
        resolve_session_operating_state(false, false, true).as_str(),
        "held"
    );
}

#[test]
fn archived_and_settled_markers_are_scoped_to_their_own_session_id_and_validated() {
    let mut manager = SessionManager::in_memory("/workspace");
    let source_id = manager.session_id().to_owned();
    let foreign_archived = manager.append_custom(
        "session_archived",
        Some(json!({ "archived": true, "sessionId": "foreign-session-id" })),
    );
    let foreign_settled = manager.append_custom(
        "session_settled",
        Some(
            json!({ "settled": true, "by": { "kind": "user" }, "sessionId": "foreign-session-id" }),
        ),
    );
    assert_eq!(archived_flag_of(&foreign_archived, &source_id), None);
    assert_eq!(settled_state_of(&foreign_settled, &source_id), None);
    assert!(!manager.is_archived());
    assert!(!manager.get_settled().settled);

    manager.set_archived(true);
    manager.set_settled(&SessionSettled {
        settled: true,
        by: Some(ThreadSettledBy::User),
    });
    assert!(manager.is_archived() && manager.get_settled().settled);

    manager.new_session(Some(NewSessionOptions {
        id: Some("019a0000-0000-7000-8000-000000000002".to_owned()),
        ..Default::default()
    }));
    assert!(!manager.is_archived());
    assert_eq!(manager.get_settled(), NOT_SETTLED);

    assert_eq!(
        parse_session_settled(&json!({
            "settled": true,
            "by": { "kind": "agent", "threadId": "  t-1 ", "threadTitle": " worker " }
        })),
        Ok(SessionSettled {
            settled: true,
            by: Some(ThreadSettledBy::Agent {
                thread_id: Some("t-1".to_owned()),
                thread_title: Some("worker".to_owned()),
            }),
        })
    );
    assert_eq!(
        parse_session_settled(
            &json!({ "settled": false, "by": { "kind": "inactivity", "days": 7 } })
        ),
        Ok(NOT_SETTLED)
    );
    assert!(parse_session_settled(&json!({ "settled": "yes" })).is_err());
    assert!(
        parse_session_settled(
            &json!({ "settled": true, "by": { "kind": "pr-merged", "pullRequestNumber": 0 } })
        )
        .is_err()
    );
}

#[test]
fn host_restart_continuation_resumes_engine_runs_and_stops_crash_loops() {
    let engine_branch = vec![
        json!({ "type": "custom", "customType": "engine-turn-start", "data": { "customType": "goal-continuation" } }),
        json!({ "type": "message", "message": { "role": "assistant", "stopReason": "toolUse" } }),
    ];
    assert!(engine_started_last_run(&engine_branch));
    assert_eq!(
        restart_release(
            &[
                hold(HoldKind::Unattended, HOST_DRAIN_HOLD_KEY),
                hold(HoldKind::SessionHold, INTERRUPTED_RUN_HOLD_KEY)
            ],
            &engine_branch
        ),
        Some(RestartRelease::Resume {
            interrupted_run: true
        })
    );

    let person_branch =
        vec![json!({ "type": "message", "message": { "role": "user", "content": "deploy" } })];
    assert!(!engine_started_last_run(&person_branch));
    assert_eq!(
        restart_release(
            &[
                hold(HoldKind::Unattended, HOST_DRAIN_HOLD_KEY),
                hold(HoldKind::SessionHold, INTERRUPTED_RUN_HOLD_KEY)
            ],
            &person_branch
        ),
        Some(RestartRelease::Release {
            keys: vec![HOST_DRAIN_HOLD_KEY.to_owned()]
        })
    );

    let no_progress = vec![
        json!({ "type": "custom", "customType": "engine-turn-start", "data": { "customType": "goal-continuation" } }),
        json!({ "type": "custom_message", "customType": STOP_RESUME_CUSTOM_TYPE, "details": { "resumeCause": "interrupted" } }),
    ];
    assert_eq!(
        restart_hold_data(true, &no_progress),
        Some(json!({ "reason": RestartHoldReason::RepeatedRestartNoProgress.as_str() }))
    );

    let mut limit_branch = vec![
        json!({ "type": "custom", "customType": "engine-turn-start", "data": { "customType": "goal-continuation" } }),
    ];
    for _ in 0..RESTART_CONTINUATION_LIMIT {
        limit_branch.push(json!({ "type": "custom_message", "customType": STOP_RESUME_CUSTOM_TYPE, "details": { "resumeCause": "interrupted" } }));
        limit_branch.push(
            json!({ "type": "message", "message": { "role": "assistant", "stopReason": "stop" } }),
        );
    }
    assert_eq!(
        restart_hold_data(true, &limit_branch),
        Some(json!({ "reason": RestartHoldReason::RepeatedRestartLimit.as_str() }))
    );
    limit_branch.insert(
        3,
        json!({ "type": "custom_message", "customType": SESSION_CONTROL_DELIVERY_TYPE, "details": { "delivery_id": "peer-1", "source": "session_control", "deliverAs": "followUp" } }),
    );
    assert_eq!(restart_hold_data(true, &limit_branch), None);
}

#[test]
fn user_message_client_message_id_and_nested_is_error_and_legacy_turn_id_are_preserved() {
    let user = maho_ai::types::UserMessage {
        content: maho_ai::types::UserContent::Text("hello".to_owned()),
        timestamp: 123,
        client_message_id: Some("client-msg-77".to_owned()),
    };
    let serialized = serde_json::to_value(&user).expect("serialize user message");
    let converted = convert_to_llm(&[
        serialized,
        json!({ "role": "custom", "customType": "note", "content": "projected", "display": true, "timestamp": 124, "clientMessageId": "client-msg-78" }),
    ]);
    assert_eq!(converted[0]["clientMessageId"], "client-msg-77");
    assert_eq!(converted[1]["clientMessageId"], "client-msg-78");

    let mut top_level_error = AgentToolResult::text("failed without throwing");
    top_level_error.is_error = Some(true);
    assert!(tool_result_is_error(&top_level_error));
    assert!(tool_result_value_is_error(
        &json!({ "content": [], "isError": true })
    ));
    assert!(tool_result_value_is_error(
        &json!({ "content": [], "details": { "isError": true } })
    ));
    assert!(!tool_result_value_is_error(
        &json!({ "content": [], "isError": false })
    ));

    let mut start_event: Value =
        json!({ "type": "agent_start", "runId": "r-1", "turnId": "t-legacy" });
    let mut settled_event: Value =
        json!({ "type": "agent_settled", "reason": "session_closed", "turnId": "t-legacy" });
    sanitize_legacy_wire_turn_id(&mut start_event);
    sanitize_legacy_wire_turn_id(&mut settled_event);
    assert!(start_event.get("turnId").is_none() && start_event["runId"] == "r-1");
    assert!(settled_event.get("turnId").is_none() && settled_event["reason"] == "session_closed");
    assert!(
        describe_compaction_rejection(CompactionRejectionCause::TransientFailure)
            .contains("transient provider error")
    );
}
