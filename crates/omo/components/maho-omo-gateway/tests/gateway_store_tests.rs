//! The gateway store through its public API (omo v5.1.80..v5.1.83 thread-gateway track): a store another binary
//! migrated past this one refuses every op and writes nothing (omo#14622), and a target nobody runs frees a backlog
//! slot as its oldest queued row expires (omo#14466).

use std::sync::Arc;

use maho_omo_gateway::thread::constants::{QUEUED_TTL_MS, TARGET_MAX_MESSAGES};
use maho_omo_gateway::thread::schema::{
    GATEWAY_MIGRATIONS, GATEWAY_READER_FLOOR_KEY, gateway_reader_floor,
};
use maho_omo_gateway::thread::store::{
    GatewayStoreOptions, SqliteGatewayStore, create_gateway_store,
};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::{
    DeliveryState, EnqueueOutcome, EnqueueRequest, EnvelopeOrigin, GatewayDeliveryMode, Provenance,
    ReceiptRef,
};
use maho_omo_gateway::{gateway_database_path, gateway_inbox_directory};

fn open(dir: &tempfile::TempDir) -> Arc<SqliteGatewayStore> {
    create_gateway_store(GatewayStoreOptions::new(
        dir.path().to_string_lossy().into_owned(),
    ))
    .expect("store")
}

fn send(delivery_id: &str, now: i64) -> EnqueueRequest {
    EnqueueRequest {
        provenance: Provenance {
            workspace: "w".into(),
            project: "p".into(),
            lane: None,
            session: None,
        },
        now,
        delivery_id: delivery_id.into(),
        target_durable_id: "lead".into(),
        sender_principal: "session:notifier".into(),
        rate_principal: None,
        sender_node: "notifier".into(),
        sender_turn: None,
        cause_delivery_id: None,
        claimed_root_id: None,
        origin: EnvelopeOrigin::Session {
            session: "notifier".into(),
        },
        actor: "notifier".into(),
        body: "alert".into(),
        mode: GatewayDeliveryMode::FollowUp,
        expected_turn_id: None,
        binding: None,
        receipt: ReceiptRef {
            idempotency_key: delivery_id.into(),
            args_hash: delivery_id.into(),
        },
        endpoint_kind: None,
    }
}

#[test]
fn a_store_another_binary_migrated_since_it_opened_refuses_every_op_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    let other = rusqlite::Connection::open(gateway_database_path(dir.path()))
        .expect("a newer binary's connection");
    let incompatible = GATEWAY_MIGRATIONS.len() + 1;
    other
        .execute_batch(&format!(
            "PRAGMA user_version = {incompatible};
             INSERT OR REPLACE INTO gateway_meta (key, value) VALUES ('{GATEWAY_READER_FLOOR_KEY}', '{incompatible}');"
        ))
        .expect("a newer binary migrates the store past this binary's reader floor");

    let read = store
        .list(Some("lead"), None)
        .expect_err("a read is refused");
    assert_eq!(read.code(), "gateway_schema_too_new");
    let write = store
        .enqueue(&send("d-1", 1_000))
        .expect_err("a write is refused");
    assert_eq!(write.code(), "gateway_schema_too_new");
    let rows: i64 = other
        .query_row("SELECT COUNT(*) FROM deliveries", [], |row| row.get(0))
        .expect("count deliveries");
    assert_eq!(rows, 0);
}

#[test]
fn a_newer_store_whose_reader_floor_this_binary_meets_keeps_reading_and_writing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    let other = rusqlite::Connection::open(gateway_database_path(dir.path()))
        .expect("a newer binary's connection");
    let floor: String = other
        .query_row(
            "SELECT value FROM gateway_meta WHERE key = ?1",
            [GATEWAY_READER_FLOOR_KEY],
            |row| row.get(0),
        )
        .expect("reader floor recorded on open");
    assert_eq!(
        floor,
        gateway_reader_floor(GATEWAY_MIGRATIONS.len() as i64).to_string()
    );

    let future = GATEWAY_MIGRATIONS.len() + 1;
    other
        .execute_batch(&format!(
            "PRAGMA user_version = {future};
             INSERT OR REPLACE INTO gateway_meta (key, value) VALUES ('{GATEWAY_READER_FLOOR_KEY}', '{}');",
            GATEWAY_MIGRATIONS.len()
        ))
        .expect("a newer reader-compatible binary migrates the store");

    let sent = store
        .enqueue(&send("d-compatible", 1_000))
        .expect("compatible floor keeps writes working");
    assert!(matches!(sent, EnqueueOutcome::Inserted { .. }), "{sent:?}");
    let listed = store
        .list(Some("lead"), None)
        .expect("compatible floor keeps reads working");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].delivery_id, "d-compatible");
}

#[test]
fn a_target_nobody_runs_frees_a_backlog_slot_as_its_oldest_queued_row_expires() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    let start = 1_000_000;
    let mut now = start;
    for index in 0..TARGET_MAX_MESSAGES {
        let sent = store
            .enqueue(&send(&format!("d-{index}"), now))
            .expect("enqueue");
        assert!(
            matches!(sent, EnqueueOutcome::Inserted { .. }),
            "row {index}: {sent:?}"
        );
        now += 5_000;
    }
    match store.enqueue(&send("d-full", now)).expect("enqueue") {
        EnqueueOutcome::Refused { code, details, .. } => {
            assert_eq!(code.as_str(), "queue_full");
            let details = details.expect("queue_full details");
            assert_eq!(details["reason"], "session_not_draining");
            assert_eq!(details["target_running"], false);
            assert_eq!(details["frees_at"], start + QUEUED_TTL_MS);
        }
        other => panic!("expected queue_full, got {other:?}"),
    }
    let oldest_wake = gateway_inbox_directory(dir.path(), "lead").join("d-0");
    assert!(oldest_wake.exists(), "a queued row keeps its wake");

    let sent = store
        .enqueue(&send("d-after", start + QUEUED_TTL_MS))
        .expect("enqueue");
    assert!(matches!(sent, EnqueueOutcome::Inserted { .. }), "{sent:?}");
    let oldest = store
        .list(Some("lead"), None)
        .expect("list")
        .into_iter()
        .find(|row| row.delivery_id == "d-0")
        .expect("the oldest row");
    assert_eq!(oldest.state, DeliveryState::Refused);
    assert_eq!(oldest.reason.as_deref(), Some("expired"));
    assert!(!oldest_wake.exists(), "the expired row's wake is gone");
}
