//! Delivery receipts of the gateway store (omo v5.1.84 thread-gateway track): tool receipts (omo#14060, #14623), the
//! outcome a retained receipt reports (omo#14623), the refusal notices a consumer sends back, and a remote send's
//! retained receipt.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures::executor::block_on;
use rusqlite::params;
use serde_json::{Value, json};

use maho_omo_gateway::gateway_database_path;
use maho_omo_gateway::thread::adapter::{EndpointLiveness, GatewayEndpointPort};
use maho_omo_gateway::thread::constants::{
    GATEWAY_RECEIPT_RETENTION_MS, PAIR_BUCKET_BURST, PAIR_BUCKET_REFILL_MS, QUEUED_TTL_MS,
};
use maho_omo_gateway::thread::engine::{
    GatewayDeliverRequest, GatewayEngine, GatewayEngineOptions, GatewayResolution, GatewayTarget,
};
use maho_omo_gateway::thread::store::{
    GatewayStoreOptions, SqliteGatewayStore, create_gateway_store,
};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::store_ops::ReceiptOutcomeRequest;
use maho_omo_gateway::thread::store_tool_receipts::{
    REMOTE_SEND_OPERATION, ReconcileRemoteReceiptRequest, ToolReceiptBegin,
    ToolReceiptBeginRequest, ToolReceiptReviseRequest, ToolReceiptScope, ToolReceiptSettleRequest,
    ToolReceiptSettlement, ToolRecovery,
};
use maho_omo_gateway::thread::types::{
    ClaimOutcome, ClaimRequest, DeliveryState, EffectiveLane, EnqueueOutcome, EnqueueRequest,
    EnvelopeOrigin, GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender, Provenance,
    ReceiptOutcome, ReceiptRef, ReceiptRefusalReason, RefusalReason,
};

fn instance(
    dir: &tempfile::TempDir,
    instance_id: &str,
    clock: &Arc<AtomicI64>,
) -> Arc<SqliteGatewayStore> {
    let mut options = GatewayStoreOptions::new(dir.path().to_string_lossy().into_owned());
    options.instance_id = Some(instance_id.to_owned());
    let clock = Arc::clone(clock);
    options.now = Some(Arc::new(move || clock.load(Ordering::Relaxed)));
    create_gateway_store(options).expect("store")
}

fn clock(start: i64) -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(start))
}

fn scope(key: &str) -> ToolReceiptScope {
    ToolReceiptScope {
        principal: "session:caller-1".to_owned(),
        operation: "thread_send".to_owned(),
        idempotency_key: key.to_owned(),
    }
}

fn begin(key: &str, now: i64, args_hash: &str) -> ToolReceiptBeginRequest {
    ToolReceiptBeginRequest {
        scope: scope(key),
        now,
        args_hash: args_hash.to_owned(),
        retry_uncertain: false,
    }
}

fn begin_recovering(key: &str, now: i64, args_hash: &str) -> ToolReceiptBeginRequest {
    ToolReceiptBeginRequest {
        retry_uncertain: true,
        ..begin(key, now, args_hash)
    }
}

fn completed(key: &str, now: i64, result: Value) -> ToolReceiptSettleRequest {
    ToolReceiptSettleRequest {
        scope: scope(key),
        now,
        settlement: ToolReceiptSettlement::Completed { result },
    }
}

fn failed(key: &str, now: i64, note: &str) -> ToolReceiptSettleRequest {
    ToolReceiptSettleRequest {
        scope: scope(key),
        now,
        settlement: ToolReceiptSettlement::Uncertain {
            error_note: note.to_owned(),
        },
    }
}

fn uncertain(note: &str) -> ToolReceiptBegin {
    ToolReceiptBegin::Uncertain {
        error_note: Some(note.to_owned()),
    }
}

#[test]
fn a_call_this_instance_began_is_in_progress_conflicts_on_other_arguments_and_replays_once_completed()
 {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    let b = instance(&dir, "instance-b", &clock);
    assert_eq!(
        a.tool_receipt_begin(&begin("k-1", now, "h-1")).unwrap(),
        ToolReceiptBegin::Accepted
    );
    assert_eq!(
        a.tool_receipt_begin(&begin("k-1", now, "h-1")).unwrap(),
        ToolReceiptBegin::InProgress { recovery: None }
    );
    assert_eq!(
        a.tool_receipt_begin(&begin("k-1", now, "h-2")).unwrap(),
        ToolReceiptBegin::Conflict
    );
    assert!(
        a.tool_receipt_settle(&completed(
            "k-1",
            now,
            json!({ "thread_id": "t-1", "seq": 1 })
        ))
        .unwrap()
    );
    assert_eq!(
        b.tool_receipt_begin(&begin("k-1", now, "h-1")).unwrap(),
        ToolReceiptBegin::Replay {
            result: json!({ "thread_id": "t-1", "seq": 1 })
        }
    );
    assert_eq!(
        b.tool_receipt_begin(&begin("k-1", now, "h-2")).unwrap(),
        ToolReceiptBegin::Conflict
    );
}

#[test]
fn a_call_another_instance_never_settled_is_uncertain_and_its_owner_can_no_longer_settle_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    let b = instance(&dir, "instance-b", &clock);
    assert_eq!(
        a.tool_receipt_begin(&begin("k-2", now, "h-1")).unwrap(),
        ToolReceiptBegin::Accepted
    );
    let first = b.tool_receipt_begin(&begin("k-2", now, "h-1")).unwrap();
    match &first {
        ToolReceiptBegin::Uncertain {
            error_note: Some(note),
        } => assert!(
            note.contains("ended before recording its outcome"),
            "{note}"
        ),
        other => panic!("expected uncertain with a note, got {other:?}"),
    }
    assert_eq!(
        b.tool_receipt_begin(&begin("k-2", now, "h-1")).unwrap(),
        first
    );
    assert!(
        !a.tool_receipt_settle(&completed("k-2", now, json!({ "late": true })))
            .unwrap()
    );
    assert_eq!(
        a.tool_receipt_begin(&begin("k-2", now, "h-1")).unwrap(),
        first
    );
}

#[test]
fn a_side_effect_that_threw_is_uncertain_carrying_the_thrown_error_for_every_instance() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    let b = instance(&dir, "instance-b", &clock);
    a.tool_receipt_begin(&begin("k-3", now, "h-1")).unwrap();
    assert!(
        a.tool_receipt_settle(&failed("k-3", now, "transport exploded mid-delivery"))
            .unwrap()
    );
    let expected = uncertain("transport exploded mid-delivery");
    assert_eq!(
        a.tool_receipt_begin(&begin("k-3", now, "h-1")).unwrap(),
        expected
    );
    assert_eq!(
        b.tool_receipt_begin(&begin("k-3", now, "h-1")).unwrap(),
        expected
    );
}

#[test]
fn releasing_a_receipt_reruns_the_key_but_never_drops_another_instances_or_a_settled_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    let b = instance(&dir, "instance-b", &clock);
    a.tool_receipt_begin(&begin("k-5", now, "h-1")).unwrap();
    assert!(!b.tool_receipt_release(&scope("k-5")).unwrap());
    assert!(a.tool_receipt_release(&scope("k-5")).unwrap());
    assert_eq!(
        b.tool_receipt_begin(&begin("k-5", now, "h-1")).unwrap(),
        ToolReceiptBegin::Accepted
    );
    assert!(
        b.tool_receipt_settle(&completed("k-5", now, json!({ "ok": true })))
            .unwrap()
    );
    assert!(!b.tool_receipt_release(&scope("k-5")).unwrap());
    assert_eq!(
        a.tool_receipt_begin(&begin("k-5", now, "h-1")).unwrap(),
        ToolReceiptBegin::Replay {
            result: json!({ "ok": true })
        }
    );
}

#[test]
fn an_uncertain_receipt_is_reserved_for_one_recovering_retry_at_a_time() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    a.tool_receipt_begin(&begin("r-1", now, "h")).unwrap();
    a.tool_receipt_settle(&failed("r-1", now, "first cause"))
        .unwrap();
    assert_eq!(
        a.tool_receipt_begin(&begin_recovering("r-1", now, "h"))
            .unwrap(),
        ToolReceiptBegin::Retry {
            error_note: Some("first cause".to_owned())
        }
    );
    assert_eq!(
        a.tool_receipt_begin(&begin_recovering("r-1", now, "h"))
            .unwrap(),
        ToolReceiptBegin::InProgress {
            recovery: Some(ToolRecovery {
                error_note: "first cause".to_owned()
            })
        }
    );
}

#[test]
fn only_a_recovery_reservations_in_progress_carries_its_first_cause() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    a.tool_receipt_begin(&begin_recovering("r-7f", now, "h"))
        .unwrap();
    assert_eq!(
        a.tool_receipt_begin(&begin_recovering("r-7f", now, "h"))
            .unwrap(),
        ToolReceiptBegin::InProgress { recovery: None }
    );
    a.tool_receipt_begin(&begin("r-7r", now, "h")).unwrap();
    a.tool_receipt_settle(&failed("r-7r", now, "first cause"))
        .unwrap();
    a.tool_receipt_begin(&begin_recovering("r-7r", now, "h"))
        .unwrap();
    assert_eq!(
        a.tool_receipt_begin(&begin_recovering("r-7r", now, "h"))
            .unwrap(),
        ToolReceiptBegin::InProgress {
            recovery: Some(ToolRecovery {
                error_note: "first cause".to_owned()
            })
        }
    );
}

#[test]
fn a_reserved_retry_that_succeeds_completes_the_receipt_for_every_later_call() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    let b = instance(&dir, "instance-b", &clock);
    a.tool_receipt_begin(&begin("r-2", now, "h")).unwrap();
    a.tool_receipt_settle(&failed("r-2", now, "first cause"))
        .unwrap();
    a.tool_receipt_begin(&begin_recovering("r-2", now, "h"))
        .unwrap();
    assert!(
        a.tool_receipt_settle(&completed("r-2", now, json!({ "thread": "t" })))
            .unwrap()
    );
    assert_eq!(
        b.tool_receipt_begin(&begin_recovering("r-2", now, "h"))
            .unwrap(),
        ToolReceiptBegin::Replay {
            result: json!({ "thread": "t" })
        }
    );
}

#[test]
fn a_reserved_retry_that_fails_or_whose_process_ended_stays_uncertain_under_the_first_cause() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    let b = instance(&dir, "instance-b", &clock);
    a.tool_receipt_begin(&begin("r-3", now, "h")).unwrap();
    a.tool_receipt_settle(&failed("r-3", now, "first cause"))
        .unwrap();
    a.tool_receipt_begin(&begin_recovering("r-3", now, "h"))
        .unwrap();
    assert!(
        a.tool_receipt_settle(&failed("r-3", now, "second cause"))
            .unwrap()
    );
    assert_eq!(
        a.tool_receipt_begin(&begin("r-3", now, "h")).unwrap(),
        uncertain("first cause")
    );
    assert_eq!(
        a.tool_receipt_begin(&begin_recovering("r-3", now, "h"))
            .unwrap(),
        ToolReceiptBegin::Retry {
            error_note: Some("first cause".to_owned())
        }
    );
    assert_eq!(
        b.tool_receipt_begin(&begin("r-3", now, "h")).unwrap(),
        uncertain("first cause"),
        "the crashed retry is uncertain under the first cause, not the crash note"
    );
}

#[test]
fn a_caller_that_may_not_recover_reserves_nothing_and_a_reserved_retry_is_never_released() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    a.tool_receipt_begin(&begin("r-5", now, "h")).unwrap();
    a.tool_receipt_settle(&failed("r-5", now, "first cause"))
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            a.tool_receipt_begin(&begin("r-5", now, "h")).unwrap(),
            uncertain("first cause")
        );
    }
    a.tool_receipt_begin(&begin("r-6", now, "h")).unwrap();
    a.tool_receipt_settle(&failed("r-6", now, "first cause"))
        .unwrap();
    a.tool_receipt_begin(&begin_recovering("r-6", now, "h"))
        .unwrap();
    assert!(!a.tool_receipt_release(&scope("r-6")).unwrap());
    a.tool_receipt_settle(&failed("r-6", now, "later")).unwrap();
    assert_eq!(
        a.tool_receipt_begin(&begin("r-6", now, "h")).unwrap(),
        uncertain("first cause")
    );
}

#[test]
fn a_completed_receipt_older_than_thirty_days_makes_its_key_a_new_call() {
    let dir = tempfile::tempdir().expect("temp dir");
    let start = 1_000_000;
    let clock = clock(start);
    let a = instance(&dir, "instance-a", &clock);
    assert_eq!(
        a.tool_receipt_begin(&begin("k-4", start, "h-1")).unwrap(),
        ToolReceiptBegin::Accepted
    );
    a.tool_receipt_settle(&completed("k-4", start, json!({ "ok": true })))
        .unwrap();
    let retained = start + GATEWAY_RECEIPT_RETENTION_MS - 1;
    assert_eq!(
        a.tool_receipt_begin(&begin("k-4", retained, "h-1"))
            .unwrap(),
        ToolReceiptBegin::Replay {
            result: json!({ "ok": true })
        }
    );
    assert_eq!(
        a.tool_receipt_begin(&begin("k-4", retained + 2, "h-1"))
            .unwrap(),
        ToolReceiptBegin::Accepted
    );
}

#[test]
fn a_revised_receipt_replays_what_later_became_of_the_call_and_only_its_settler_revises_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let now = 1_000;
    let clock = clock(now);
    let a = instance(&dir, "instance-a", &clock);
    let b = instance(&dir, "instance-b", &clock);
    a.tool_receipt_begin(&begin("v-1", now, "h")).unwrap();
    a.tool_receipt_settle(&completed("v-1", now, json!({ "delivery": "queued" })))
        .unwrap();
    let revision = |result: Value| ToolReceiptReviseRequest {
        scope: scope("v-1"),
        now,
        result,
    };
    assert!(
        !b.tool_receipt_revise(&revision(json!({ "delivery": "other" })))
            .unwrap()
    );
    assert!(
        a.tool_receipt_revise(&revision(json!({ "error": "turn_conflict" })))
            .unwrap()
    );
    assert_eq!(
        b.tool_receipt_begin(&begin("v-1", now, "h")).unwrap(),
        ToolReceiptBegin::Replay {
            result: json!({ "error": "turn_conflict" })
        }
    );
}

fn provenance() -> Provenance {
    Provenance {
        workspace: "w".to_owned(),
        project: "p".to_owned(),
        lane: None,
        session: None,
    }
}

fn engine_over(store: &Arc<SqliteGatewayStore>, clock: &Arc<AtomicI64>) -> GatewayEngine {
    let clock = Arc::clone(clock);
    GatewayEngine::new(GatewayEngineOptions {
        sender_provenance: Some(Arc::new(provenance)),
        store: store.clone(),
        endpoints: GatewayEndpointPort::inert(),
        resolve: Arc::new(|target: &str, _all_scope: bool| {
            let durable_id = target.to_owned();
            Box::pin(async move {
                GatewayResolution::Ok(GatewayTarget {
                    durable_id,
                    endpoint: None,
                    liveness: EndpointLiveness::Dead,
                })
            })
        }),
        now: Some(Arc::new(move || clock.load(Ordering::Relaxed))),
    })
}

fn deliver_request(text: &str, key: &str) -> GatewayDeliverRequest {
    let mut request = GatewayDeliverRequest::new(
        GatewaySender::Session {
            durable_id: "sender".to_owned(),
            name: None,
            turn_id: None,
            cause_delivery_id: None,
        },
        "target",
        text,
    );
    request.idempotency_key = Some(key.to_owned());
    request
}

#[test]
fn a_retained_receipt_exposes_each_refusal_without_changing_its_original_admission() {
    for reason in [
        RefusalReason::Expired,
        RefusalReason::NotSteerable,
        RefusalReason::TurnConflict,
        RefusalReason::TargetClosed,
    ] {
        let dir = tempfile::tempdir().expect("temp dir");
        let clock = clock(1_000_000);
        let store = instance(&dir, "instance-a", &clock);
        let engine = engine_over(&store, &clock);
        let first = block_on(engine.deliver(deliver_request("one", "same")));
        let (delivery_id, delivery) = match first {
            GatewayDeliveryResult::Ok {
                delivery_id,
                delivery,
                ..
            } => (delivery_id, delivery),
            other => panic!("expected an admission, got {other:?}"),
        };
        assert!(
            store
                .refuse_queued(clock.load(Ordering::Relaxed), &delivery_id, reason)
                .expect("refuse")
        );
        let replay = block_on(engine.deliver(deliver_request("one", "same")));
        match replay {
            GatewayDeliveryResult::Ok {
                delivery: replayed,
                delivery_id: replayed_id,
                deduplicated,
                outcome,
                ..
            } => {
                assert_eq!(replayed, delivery, "{reason:?}");
                assert_eq!(replayed_id, delivery_id);
                assert!(deduplicated);
                assert_eq!(
                    outcome,
                    Some(ReceiptOutcome::Refused {
                        reason: ReceiptRefusalReason::parse(reason.as_str())
                            .expect("a receipt reason"),
                    })
                );
            }
            other => panic!("expected a replay, got {other:?}"),
        }
        let rows = store.list(Some("target"), None).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, DeliveryState::Refused);
        assert_eq!(rows[0].reason.as_deref(), Some(reason.as_str()));
        assert_eq!(rows[0].attempt, 0);
        assert!(
            store
                .refusal_notices(clock.load(Ordering::Relaxed))
                .expect("notices")
                .is_empty()
        );
    }
}

#[test]
fn a_retry_of_a_send_receipted_with_binding_and_author_null_still_replays() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = clock(1_000_000);
    let store = instance(&dir, "instance-a", &clock);
    let engine = engine_over(&store, &clock);
    let mut request = deliver_request("hello", "pre-upgrade");
    request.mode = Some(GatewayDeliveryMode::FollowUp);
    let first = block_on(engine.deliver(request.clone()));
    let first_id = match first {
        GatewayDeliveryResult::Ok { delivery_id, .. } => delivery_id,
        other => panic!("expected an admission, got {other:?}"),
    };
    let legacy = json!({
        "target": "target",
        "text": "hello",
        "mode": "follow_up",
        "expected_turn_id": null,
        "binding": null,
        "author": null,
    });
    let legacy_hash = {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(serde_json::to_vec(&legacy).expect("legacy arguments"));
        format!("{:x}", hasher.finalize())
    };
    let other_binary =
        rusqlite::Connection::open(gateway_database_path(dir.path())).expect("another connection");
    other_binary
        .execute(
            "UPDATE receipts SET args_hash = ?1 WHERE idempotency_key = ?2",
            params![legacy_hash, "pre-upgrade"],
        )
        .expect("rewrite the receipt as the previous build wrote it");

    match block_on(engine.deliver(request)) {
        GatewayDeliveryResult::Ok {
            delivery_id,
            deduplicated,
            ..
        } => {
            assert_eq!(delivery_id, first_id);
            assert!(deduplicated);
        }
        other => panic!("the unchanged retry must replay, got {other:?}"),
    }
}

fn send(delivery_id: &str, target: &str, now: i64) -> EnqueueRequest {
    EnqueueRequest {
        provenance: provenance(),
        now,
        delivery_id: delivery_id.to_owned(),
        target_durable_id: target.to_owned(),
        sender_principal: "session:notifier".to_owned(),
        rate_principal: None,
        sender_node: "notifier".to_owned(),
        sender_turn: None,
        cause_delivery_id: None,
        claimed_root_id: None,
        origin: EnvelopeOrigin::Session {
            session: "notifier".to_owned(),
        },
        actor: "notifier".to_owned(),
        body: "alert".to_owned(),
        mode: GatewayDeliveryMode::FollowUp,
        expected_turn_id: None,
        receipt: ReceiptRef {
            idempotency_key: delivery_id.to_owned(),
            args_hash: delivery_id.to_owned(),
        },
        endpoint_kind: None,
        collapse_key: None,
    }
}

fn outcome_of(
    store: &Arc<SqliteGatewayStore>,
    delivery_id: &str,
    now: i64,
    session_path: Option<String>,
) -> ReceiptOutcome {
    store
        .receipt_outcome(&ReceiptOutcomeRequest {
            now,
            principal: "session:notifier".to_owned(),
            delivery_id: delivery_id.to_owned(),
            target_durable_id: "target".to_owned(),
            session_path,
        })
        .expect("receipt outcome")
}

#[test]
fn a_queued_row_is_pending_until_its_lifetime_passes_and_then_settles_expired() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = clock(1_000);
    let store = instance(&dir, "instance-a", &clock);
    let sent = store
        .enqueue(&send("d-1", "target", 1_000))
        .expect("enqueue");
    assert!(matches!(sent, EnqueueOutcome::Inserted { .. }), "{sent:?}");
    assert_eq!(
        outcome_of(&store, "d-1", 1_001, None),
        ReceiptOutcome::Pending
    );
    assert_eq!(
        outcome_of(&store, "d-1", 1_000 + QUEUED_TTL_MS, None),
        ReceiptOutcome::Refused {
            reason: ReceiptRefusalReason::Expired
        }
    );
    let row = store.list(Some("target"), None).expect("list").remove(0);
    assert_eq!(row.state, DeliveryState::Refused);
    assert_eq!(row.reason.as_deref(), Some("expired"));
    let marker = maho_omo_gateway::gateway_inbox_directory(dir.path(), "target").join("d-1");
    assert!(!marker.exists(), "the settled row's wake is gone");
}

#[test]
fn a_claimed_row_has_no_outcome_until_the_session_file_proves_it_was_written() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = clock(1_000);
    let store = instance(&dir, "instance-a", &clock);
    store
        .enqueue(&send("d-1", "target", 1_000))
        .expect("enqueue");
    let claimed = store
        .claim(&ClaimRequest {
            now: 1_001,
            delivery_id: "d-1".to_owned(),
            self_identity: store.identity().expect("identity"),
            lane: EffectiveLane::FollowUp,
        })
        .expect("claim");
    assert!(
        matches!(claimed, ClaimOutcome::Claimed { .. }),
        "{claimed:?}"
    );
    assert_eq!(
        outcome_of(&store, "d-1", 1_002, None),
        ReceiptOutcome::OutcomeUnavailable
    );
    let session = dir.path().join("session.jsonl");
    std::fs::write(&session, "{\"type\":\"session\"}\n").expect("write a session file");
    assert_eq!(
        outcome_of(
            &store,
            "d-1",
            1_003,
            Some(session.to_string_lossy().into_owned())
        ),
        ReceiptOutcome::OutcomeUnavailable,
        "a transcript without the delivery proves nothing"
    );
    let written = json!({
        "type": "custom_message",
        "customType": "session_control_delivery",
        "details": { "delivery_id": "d-1" },
    });
    std::fs::write(&session, format!("{written}\n")).expect("write the delivery");
    assert_eq!(
        outcome_of(
            &store,
            "d-1",
            1_004,
            Some(session.to_string_lossy().into_owned())
        ),
        ReceiptOutcome::Applied
    );
    let row = store.list(Some("target"), None).expect("list").remove(0);
    assert_eq!(row.state, DeliveryState::Applied);
}

#[test]
fn a_device_receipt_without_a_row_is_applied_only_when_the_session_file_holds_its_device_scoped_id()
{
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = clock(1_000);
    let store = instance(&dir, "instance-a", &clock);
    let request = |session_path: Option<String>| ReceiptOutcomeRequest {
        now: 1_000,
        principal: "device:peer".to_owned(),
        delivery_id: "d-1".to_owned(),
        target_durable_id: "target".to_owned(),
        session_path,
    };
    assert_eq!(
        store.receipt_outcome(&request(None)).expect("outcome"),
        ReceiptOutcome::OutcomeUnavailable
    );
    let session = dir.path().join("live.jsonl");
    let session_path = Some(session.to_string_lossy().into_owned());
    assert_eq!(
        store
            .receipt_outcome(&request(session_path.clone()))
            .expect("outcome"),
        ReceiptOutcome::OutcomeUnavailable,
        "a session file that does not exist proves nothing"
    );
    let written = json!({
        "type": "custom_message",
        "customType": "session_control_delivery",
        "details": { "delivery_id": "device:peer/d-1" },
    });
    std::fs::write(&session, format!("{written}\n")).expect("write the delivery");
    assert_eq!(
        store
            .receipt_outcome(&request(session_path))
            .expect("outcome"),
        ReceiptOutcome::Applied
    );
}

fn device_refusal(
    store: &Arc<SqliteGatewayStore>,
    connection: &rusqlite::Connection,
    index: usize,
    now: i64,
) {
    let delivery_id = format!("d-{index}");
    let mut sent = send(&delivery_id, &format!("target-{index}"), now);
    sent.sender_principal = "device:peer".to_owned();
    sent.origin = EnvelopeOrigin::Session {
        session: "caller".to_owned(),
    };
    let outcome = store.enqueue(&sent).expect("enqueue");
    assert!(
        matches!(outcome, EnqueueOutcome::Inserted { .. }),
        "{outcome:?}"
    );
    connection
        .execute(
            "UPDATE deliveries SET ledger_id = ?1 WHERE delivery_id = ?2",
            params![format!("device:peer/{delivery_id}"), delivery_id],
        )
        .expect("a row taken in from another device");
    assert!(
        store
            .refuse_queued(now, &delivery_id, RefusalReason::Expired)
            .expect("refuse")
    );
}

#[test]
fn refusal_notice_claims_are_durable_bounded_and_rate_limited_per_original_peer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = clock(1_000_000);
    let store = instance(&dir, "instance-a", &clock);
    let connection =
        rusqlite::Connection::open(gateway_database_path(dir.path())).expect("another connection");
    let burst = PAIR_BUCKET_BURST as usize;
    let now = clock.load(Ordering::Relaxed);
    for index in 0..=burst {
        device_refusal(&store, &connection, index, now);
    }
    let candidates = store.refusal_notices(now).expect("candidates");
    assert_eq!(candidates.len(), burst + 1);
    assert!(candidates.len() <= 32);
    let first = &candidates[0];
    assert_eq!(first.device, "peer");
    assert_eq!(first.session, "caller");
    assert_eq!(first.reason, ReceiptRefusalReason::Expired);
    assert_eq!(
        first.delivery_id, first.row_id,
        "the sender's own id, not the ledger key"
    );
    let claims: Vec<bool> = candidates
        .iter()
        .map(|notice| {
            store
                .claim_refusal_notice(now, notice, false)
                .expect("claim")
        })
        .collect();
    assert_eq!(claims.iter().filter(|claimed| **claimed).count(), burst);
    for notice in &candidates {
        assert!(
            !store
                .claim_refusal_notice(now, notice, false)
                .expect("claim"),
            "a notice is claimed once"
        );
    }
    let later = now + PAIR_BUCKET_REFILL_MS as i64;
    let remaining = store.refusal_notices(later).expect("remaining");
    assert_eq!(remaining.len(), 1);
    assert!(
        store
            .claim_refusal_notice(later, &remaining[0], false)
            .expect("claim after the refill")
    );
    assert!(store.refusal_notices(later).expect("none left").is_empty());
}

#[test]
fn a_suppressed_claim_records_the_notice_without_spending_a_token() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = clock(1_000_000);
    let store = instance(&dir, "instance-a", &clock);
    let connection =
        rusqlite::Connection::open(gateway_database_path(dir.path())).expect("another connection");
    let now = clock.load(Ordering::Relaxed);
    for index in 0..=(PAIR_BUCKET_BURST as usize) {
        device_refusal(&store, &connection, index, now);
    }
    let candidates = store.refusal_notices(now).expect("candidates");
    for notice in &candidates {
        assert!(
            store
                .claim_refusal_notice(now, notice, true)
                .expect("suppressed claim")
        );
    }
    assert!(store.refusal_notices(now).expect("none left").is_empty());
}

fn remote_receipt_row(connection: &rusqlite::Connection, outcome: Option<Value>) {
    let mut result = json!({
        "kind": "ok",
        "thread_id": "t-1",
        "device": "peer",
        "delivery_id": "d-1",
    });
    if let Some(outcome) = outcome {
        result["outcome"] = outcome;
    }
    connection
        .execute(
            "INSERT INTO receipts (principal, operation, idempotency_key, args_hash, status, owner_instance, result, created_at, updated_at, expires_at) VALUES ('session:caller', ?1, 'key-1', 'h', 'completed', 'instance-a', ?2, 1, 1, ?3)",
            params![
                REMOTE_SEND_OPERATION,
                result.to_string(),
                1_000_000 + GATEWAY_RECEIPT_RETENTION_MS
            ],
        )
        .expect("a remote send's receipt");
}

fn reconcile(outcome: ReceiptOutcome, device: &str, key: &str) -> ReconcileRemoteReceiptRequest {
    ReconcileRemoteReceiptRequest {
        now: 1_000,
        principal: "session:caller".to_owned(),
        idempotency_key: key.to_owned(),
        delivery_id: "d-1".to_owned(),
        target_durable_id: "t-1".to_owned(),
        device: device.to_owned(),
        outcome,
    }
}

#[test]
fn a_remote_receipt_is_found_by_its_delivery_id_and_keeps_the_first_terminal_outcome() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = clock(1_000);
    let store = instance(&dir, "instance-a", &clock);
    let connection =
        rusqlite::Connection::open(gateway_database_path(dir.path())).expect("another connection");
    assert!(
        store
            .remote_receipt(1_000, "session:caller", "d-1")
            .expect("lookup")
            .is_none()
    );
    remote_receipt_row(&connection, None);
    let found = store
        .remote_receipt(1_000, "session:caller", "d-1")
        .expect("lookup")
        .expect("the retained receipt");
    assert_eq!(found.idempotency_key, "key-1");
    assert_eq!(found.result["thread_id"], "t-1");
    assert!(
        store
            .remote_receipt(1_000, "session:other", "d-1")
            .expect("lookup")
            .is_none(),
        "another session's receipt is not this caller's"
    );

    assert_eq!(
        store
            .reconcile_remote_receipt(&reconcile(ReceiptOutcome::Admitted, "peer", "key-1"))
            .expect("reconcile"),
        ReceiptOutcome::Admitted
    );
    assert_eq!(
        store
            .reconcile_remote_receipt(&reconcile(ReceiptOutcome::Applied, "peer", "key-1"))
            .expect("reconcile"),
        ReceiptOutcome::Applied
    );
    assert_eq!(
        store
            .reconcile_remote_receipt(&reconcile(
                ReceiptOutcome::Refused {
                    reason: ReceiptRefusalReason::Expired
                },
                "peer",
                "key-1"
            ))
            .expect("reconcile"),
        ReceiptOutcome::Applied,
        "a terminal fact already recorded stays"
    );
    let stored = store
        .remote_receipt(1_000, "session:caller", "d-1")
        .expect("lookup")
        .expect("the retained receipt");
    assert_eq!(stored.result["outcome"], json!({ "kind": "applied" }));
}

#[test]
fn a_remote_outcome_for_another_device_thread_or_key_is_not_recorded() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = clock(1_000);
    let store = instance(&dir, "instance-a", &clock);
    let connection =
        rusqlite::Connection::open(gateway_database_path(dir.path())).expect("another connection");
    remote_receipt_row(&connection, None);
    for request in [
        reconcile(ReceiptOutcome::Applied, "elsewhere", "key-1"),
        reconcile(ReceiptOutcome::Applied, "peer", "other-key"),
        ReconcileRemoteReceiptRequest {
            target_durable_id: "t-2".to_owned(),
            ..reconcile(ReceiptOutcome::Applied, "peer", "key-1")
        },
    ] {
        assert_eq!(
            store.reconcile_remote_receipt(&request).expect("reconcile"),
            ReceiptOutcome::OutcomeUnavailable
        );
    }
    let stored = store
        .remote_receipt(1_000, "session:caller", "d-1")
        .expect("lookup")
        .expect("the retained receipt");
    assert!(stored.result.get("outcome").is_none());
}
