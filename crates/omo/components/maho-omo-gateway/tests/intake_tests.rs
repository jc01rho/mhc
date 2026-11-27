//! The store's intake of a record another device attested (omo#13037 design 5.5, `store-worker.ts` `intake`): a record
//! for a session that is not running is committed as one row, its receipt answered from that row, and a repeat from the
//! same device answers the same row instead of a second delivery.

use std::sync::Arc;

use maho_omo_gateway::thread::store::{
    GatewayStoreOptions, SqliteGatewayStore, create_gateway_store,
};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::{
    DeliveryState, IntakeOutcome, IntakeRecord, IntakeRequest, IntakeSender, Provenance,
};

fn open(dir: &tempfile::TempDir) -> Arc<SqliteGatewayStore> {
    create_gateway_store(GatewayStoreOptions::new(
        dir.path().to_string_lossy().into_owned(),
    ))
    .expect("store")
}

fn record(delivery_id: &str) -> IntakeRecord {
    IntakeRecord {
        session_id: "lead".into(),
        delivery_id: delivery_id.into(),
        text: "relayed".into(),
        display_text: "relayed".into(),
        sender: IntakeSender {
            kind: "agent".into(),
            session_id: Some("peer".into()),
            name: Some("peer".into()),
        },
        deliver_as: "followUp".into(),
        chain_count: 2,
    }
}

fn intake(delivery_id: &str, device: &str, now: i64) -> IntakeRequest {
    IntakeRequest {
        now,
        attested_device: device.into(),
        record: record(delivery_id),
        provenance: Provenance {
            workspace: "remote".into(),
            project: "remote".into(),
            lane: None,
            session: None,
        },
        target_session_kind: "interactive".into(),
        chain_count_limit: None,
    }
}

fn queued(outcome: &IntakeOutcome) -> (&str, &str, i64, bool) {
    match outcome {
        IntakeOutcome::Queued {
            row_id,
            ledger_id,
            queue_position,
            deduplicated,
            ..
        } => (row_id, ledger_id, *queue_position, *deduplicated),
        other => panic!("expected a queued intake, got {other:?}"),
    }
}

#[test]
fn an_attested_record_is_one_queued_row_under_its_ledger_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    let outcome = store
        .intake(&intake("delivery-1", "laptop", 1_000))
        .expect("intake");
    let (row_id, ledger_id, position, deduplicated) = queued(&outcome);
    assert_eq!(ledger_id, "device:laptop/delivery-1");
    assert_eq!(position, 1);
    assert!(!deduplicated);
    let row = store
        .list(Some("lead"), None)
        .expect("list")
        .into_iter()
        .find(|row| row.delivery_id == row_id)
        .expect("the intake's row");
    assert_eq!(row.state, DeliveryState::Queued);
    assert_eq!(row.ledger_id.as_deref(), Some("device:laptop/delivery-1"));
    assert_eq!(row.verbatim_text.as_deref(), Some("relayed"));
    // The record already crossed one device, so its row continues that chain rather than starting at one.
    assert_eq!(row.hop, 2);
}

#[test]
fn a_repeat_from_the_same_device_answers_the_first_row_and_stores_nothing_new() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    let first = store
        .intake(&intake("delivery-1", "laptop", 1_000))
        .expect("first");
    let second = store
        .intake(&intake("delivery-1", "laptop", 1_001))
        .expect("second");
    let (first_row, _, _, first_deduplicated) = queued(&first);
    let (second_row, _, _, second_deduplicated) = queued(&second);
    assert_eq!(first_row, second_row);
    assert!(!first_deduplicated);
    assert!(second_deduplicated);
    assert_eq!(store.list(Some("lead"), None).expect("list").len(), 1);
}

#[test]
fn the_same_delivery_id_from_another_device_is_a_second_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    let first = store
        .intake(&intake("delivery-1", "laptop", 1_000))
        .expect("first");
    let second = store
        .intake(&intake("delivery-1", "desktop", 1_001))
        .expect("second");
    assert_ne!(queued(&first).0, queued(&second).0);
    assert_eq!(queued(&second).1, "device:desktop/delivery-1");
    assert_eq!(store.list(Some("lead"), None).expect("list").len(), 2);
}

#[test]
fn a_steer_to_a_session_that_is_not_running_is_a_turn_conflict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    let mut request = intake("delivery-1", "laptop", 1_000);
    request.record.deliver_as = "steer".into();
    match store.intake(&request).expect("intake") {
        IntakeOutcome::Refused { code, .. } => assert_eq!(code.as_str(), "turn_conflict"),
        other => panic!("expected turn_conflict, got {other:?}"),
    }
    assert!(store.list(Some("lead"), None).expect("list").is_empty());
}

#[test]
fn a_record_from_a_non_agent_sender_is_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    let mut request = intake("delivery-1", "laptop", 1_000);
    request.record.sender.kind = "person".into();
    match store.intake(&request).expect("intake") {
        IntakeOutcome::Refused { code, .. } => assert_eq!(code.as_str(), "unsupported"),
        other => panic!("expected unsupported, got {other:?}"),
    }
}

#[test]
fn a_chain_over_this_machines_limit_is_refused_with_its_numbers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    let mut request = intake("delivery-1", "laptop", 1_000);
    request.chain_count_limit = Some(1);
    match store.intake(&request).expect("intake") {
        IntakeOutcome::Refused { code, details, .. } => {
            assert_eq!(code.as_str(), "unsupported");
            let details = details.expect("details");
            assert_eq!(details["reason"], "chain_count_limit");
            assert_eq!(details["chain_count"], 2);
            assert_eq!(details["limit"], 1);
        }
        other => panic!("expected a chain limit refusal, got {other:?}"),
    }
}

#[test]
fn a_child_target_session_is_not_resumable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    let mut request = intake("delivery-1", "laptop", 1_000);
    request.target_session_kind = "child".into();
    match store.intake(&request).expect("intake") {
        IntakeOutcome::Refused { code, .. } => assert_eq!(code.as_str(), "not_resumable"),
        other => panic!("expected not_resumable, got {other:?}"),
    }
}

#[test]
fn a_malformed_record_is_refused_before_anything_is_stored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    let mut request = intake("delivery-1", "laptop", 1_000);
    request.record.chain_count = 0;
    match store.intake(&request).expect("intake") {
        IntakeOutcome::Refused { code, .. } => assert_eq!(code.as_str(), "invalid_arguments"),
        other => panic!("expected invalid_arguments, got {other:?}"),
    }
    assert!(store.list(Some("lead"), None).expect("list").is_empty());
}

#[test]
fn a_repeat_with_other_content_under_the_same_ledger_key_is_a_conflict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir);
    store
        .intake(&intake("delivery-1", "laptop", 1_000))
        .expect("first");
    let mut changed = intake("delivery-1", "laptop", 1_001);
    changed.record.text = "something else".into();
    changed.record.display_text = "something else".into();
    match store.intake(&changed).expect("intake") {
        IntakeOutcome::Refused { code, .. } => assert_eq!(code.as_str(), "idempotency_conflict"),
        other => panic!("expected idempotency_conflict, got {other:?}"),
    }
}
