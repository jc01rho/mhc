//! The gateway store through its public API (omo v5.1.80..v5.1.83 thread-gateway and gateway-schema tracks): a store
//! another binary migrated past this one refuses every op and writes nothing (omo#14622) unless it recorded a reader
//! floor this binary meets (omo#14382), a target nobody runs frees a backlog slot as its oldest queued row expires
//! (omo#14466), and a collapse key leaves at most one undelivered message per key (omo#14428, #14431).

use std::sync::Arc;

use maho_omo_gateway::thread::constants::{QUEUED_TTL_MS, TARGET_MAX_MESSAGES};
use maho_omo_gateway::thread::schema::GATEWAY_MIGRATIONS;
use maho_omo_gateway::thread::store::{
    GatewayStoreOptions, SqliteGatewayStore, create_gateway_store,
};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::{
    AdmissionLedger, ClaimOutcome, ClaimRequest, DeliveryState, EffectiveLane, EnqueueOutcome,
    EnqueueRequest, EnvelopeOrigin, GatewayDeliveryMode, Provenance, ReceiptRef, ReconcileRequest,
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
        receipt: ReceiptRef {
            idempotency_key: delivery_id.into(),
            args_hash: delivery_id.into(),
        },
        endpoint_kind: None,
        collapse_key: None,
    }
}

#[test]
fn a_store_another_binary_migrated_since_it_opened_refuses_every_op_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    let other = rusqlite::Connection::open(gateway_database_path(dir.path()))
        .expect("a newer binary's connection");
    // A newer binary's migration this one cannot read records no reader floor this binary meets (omo#14382).
    other
        .execute_batch(&format!(
            "DELETE FROM gateway_meta WHERE key = 'reader_floor'; PRAGMA user_version = {}",
            GATEWAY_MIGRATIONS.len() + 1
        ))
        .expect("a newer binary migrates the store");

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

fn keyed(delivery_id: &str, now: i64, collapse_key: &str) -> EnqueueRequest {
    EnqueueRequest {
        collapse_key: Some(collapse_key.into()),
        ..send(delivery_id, now)
    }
}

fn other_binary(dir: &tempfile::TempDir) -> rusqlite::Connection {
    rusqlite::Connection::open(gateway_database_path(dir.path()))
        .expect("another binary's connection")
}

fn reader_floor(connection: &rusqlite::Connection) -> Option<String> {
    connection
        .query_row(
            "SELECT value FROM gateway_meta WHERE key = 'reader_floor'",
            [],
            |row| row.get(0),
        )
        .ok()
}

fn user_version(connection: &rusqlite::Connection) -> i64 {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version")
}

#[test]
fn a_store_this_binary_migrates_records_reader_floor_14() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _store = open(&dir);
    let other = other_binary(&dir);
    assert_eq!(reader_floor(&other).as_deref(), Some("14"));
    assert_eq!(user_version(&other), GATEWAY_MIGRATIONS.len() as i64);
}

#[test]
fn a_newer_store_whose_reader_floor_this_binary_meets_stays_usable() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = open(&dir);
    let newer = GATEWAY_MIGRATIONS.len() + 1;
    other_binary(&dir)
        .execute_batch(&format!(
            "PRAGMA user_version = {newer}; INSERT OR REPLACE INTO gateway_meta (key, value) VALUES ('reader_floor', '{}')",
            GATEWAY_MIGRATIONS.len()
        ))
        .expect("a newer binary migrates the store by reader-compatible steps");

    let sent = running
        .enqueue(&send("d-1", 1_000))
        .expect("a running session still sends");
    assert!(matches!(sent, EnqueueOutcome::Inserted { .. }), "{sent:?}");
    let opened = open(&dir);
    assert_eq!(
        opened
            .list(Some("lead"), None)
            .expect("a session opened afterwards reads the store")
            .len(),
        1
    );
    assert_eq!(user_version(&other_binary(&dir)), newer as i64);
}

#[test]
fn a_newer_store_without_a_reader_floor_this_binary_meets_is_refused_and_left_untouched() {
    let newer = GATEWAY_MIGRATIONS.len() + 1;
    let cases = [
        (
            "a floor above this binary",
            format!("INSERT OR REPLACE INTO gateway_meta (key, value) VALUES ('reader_floor', '{newer}')"),
        ),
        (
            "no floor recorded",
            "DELETE FROM gateway_meta WHERE key = 'reader_floor'".to_owned(),
        ),
        ("no gateway_meta table", "DROP TABLE gateway_meta".to_owned()),
    ]
    .into_iter()
    .chain(
        ["", " 14", "14abc", "-1", "1e1", "14.0", "0"]
            .into_iter()
            .map(|floor| {
                (
                    "a floor that is not a plain positive integer",
                    format!("INSERT OR REPLACE INTO gateway_meta (key, value) VALUES ('reader_floor', '{floor}')"),
                )
            }),
    );
    for (case, floor) in cases {
        let dir = tempfile::tempdir().expect("temp dir");
        drop(open(&dir));
        other_binary(&dir)
            .execute_batch(&format!("PRAGMA user_version = {newer}; {floor}"))
            .expect("a newer binary migrates the store");
        let Err(error) = create_gateway_store(GatewayStoreOptions::new(
            dir.path().to_string_lossy().into_owned(),
        )) else {
            panic!("{case} ({floor}): a newer store this binary cannot read is refused");
        };
        assert_eq!(error.code(), "gateway_schema_too_new", "{case} ({floor})");
        assert_eq!(
            user_version(&other_binary(&dir)),
            newer as i64,
            "{case} ({floor})"
        );
    }
}

#[test]
fn a_store_at_this_schema_without_its_reader_floor_gets_it_recorded_on_open() {
    for floor in [
        "DELETE FROM gateway_meta WHERE key = 'reader_floor'",
        "INSERT OR REPLACE INTO gateway_meta (key, value) VALUES ('reader_floor', 'abc')",
    ] {
        let dir = tempfile::tempdir().expect("temp dir");
        drop(open(&dir));
        other_binary(&dir)
            .execute_batch(floor)
            .expect("a binary from before the floor left the store");
        let _store = open(&dir);
        assert_eq!(
            reader_floor(&other_binary(&dir)).as_deref(),
            Some("14"),
            "{floor}"
        );
    }
}

#[test]
fn a_repeat_under_a_collapse_key_supersedes_the_queued_one_whose_retry_replays_the_refusal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    for (id, now) in [("d-1", 1_000), ("d-2", 1_001)] {
        let sent = store.enqueue(&keyed(id, now, "alert")).expect("enqueue");
        assert!(
            matches!(sent, EnqueueOutcome::Inserted { .. }),
            "{id}: {sent:?}"
        );
    }

    let rows = store.list(Some("lead"), None).expect("list");
    let state = |id: &str| {
        rows.iter()
            .find(|row| row.delivery_id == id)
            .map(|row| (row.state, row.reason.clone()))
    };
    assert_eq!(
        state("d-1"),
        Some((DeliveryState::Refused, Some("superseded".to_owned())))
    );
    assert_eq!(state("d-2"), Some((DeliveryState::Queued, None)));
    let inbox = gateway_inbox_directory(dir.path(), "lead");
    assert!(
        !inbox.join("d-1").exists(),
        "the superseded row's wake is gone"
    );
    assert!(inbox.join("d-2").exists(), "the repeat keeps its wake");
    match store
        .enqueue(&keyed("d-1", 1_002, "alert"))
        .expect("retry under the superseded row's key")
    {
        EnqueueOutcome::Replay { result } => {
            assert_eq!(result["kind"], "error");
            assert_eq!(result["error"]["code"], "not_resumable");
        }
        other => panic!("expected the refusal replayed, got {other:?}"),
    }
}

#[test]
fn a_repeat_the_session_already_took_an_earlier_one_for_is_absorbed_into_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    let first = store
        .enqueue(&keyed("d-1", 1_000, "alert"))
        .expect("enqueue");
    assert!(
        matches!(first, EnqueueOutcome::Inserted { .. }),
        "{first:?}"
    );
    let claimed = store
        .claim(&ClaimRequest {
            now: 1_001,
            delivery_id: "d-1".into(),
            self_identity: store.identity().expect("identity"),
            lane: EffectiveLane::FollowUp,
        })
        .expect("claim");
    assert!(
        matches!(claimed, ClaimOutcome::Claimed { .. }),
        "{claimed:?}"
    );

    match store
        .enqueue(&keyed("d-2", 1_002, "alert"))
        .expect("enqueue")
    {
        EnqueueOutcome::Collapsed { row, .. } => assert_eq!(row.delivery_id, "d-1"),
        other => panic!("expected the repeat absorbed, got {other:?}"),
    }
    let ids: Vec<String> = store
        .list(Some("lead"), None)
        .expect("list")
        .into_iter()
        .map(|row| row.delivery_id)
        .collect();
    assert_eq!(ids, vec!["d-1".to_owned()]);
}

#[test]
fn a_repeat_refused_after_superseding_the_queued_one_rolls_back_and_the_queued_one_keeps_its_wake()
{
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    let mut refused = None;
    for index in 0..20 {
        match store
            .enqueue(&keyed(&format!("d-{index}"), 1_000, "alert"))
            .expect("enqueue")
        {
            EnqueueOutcome::Inserted { .. } => {}
            EnqueueOutcome::Refused { code, .. } => {
                refused = Some(code.as_str().to_owned());
                break;
            }
            other => panic!("send {index}: {other:?}"),
        }
    }
    assert_eq!(refused.as_deref(), Some("pair_loop_guard"));

    let queued: Vec<String> = store
        .list(Some("lead"), None)
        .expect("list")
        .into_iter()
        .filter(|row| row.state == DeliveryState::Queued)
        .map(|row| row.delivery_id)
        .collect();
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert!(
        gateway_inbox_directory(dir.path(), "lead")
            .join(&queued[0])
            .exists(),
        "the queued row keeps its wake"
    );
}

#[test]
fn an_expired_row_taken_in_from_another_device_is_left_to_its_target() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    let start = 1_000_000;
    let sent = store.enqueue(&send("d-intake", start)).expect("enqueue");
    assert!(matches!(sent, EnqueueOutcome::Inserted { .. }), "{sent:?}");
    other_binary(&dir)
        .execute_batch(
            "UPDATE deliveries SET ledger_id = 'device:b/d-intake' WHERE delivery_id = 'd-intake'",
        )
        .expect("the row becomes one taken in from another device");

    let later = store
        .enqueue(&send("d-later", start + QUEUED_TTL_MS))
        .expect("enqueue");
    assert!(
        matches!(later, EnqueueOutcome::Inserted { .. }),
        "{later:?}"
    );
    let intake = store
        .list(Some("lead"), None)
        .expect("list")
        .into_iter()
        .find(|row| row.delivery_id == "d-intake")
        .expect("the intake row");
    assert_eq!(intake.state, DeliveryState::Queued);
    assert_eq!(intake.ledger_id.as_deref(), Some("device:b/d-intake"));
}

#[test]
fn a_row_taken_in_from_another_device_settles_and_is_continued_by_its_ledger_key() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir);
    for id in ["d-written", "d-claimed"] {
        let sent = store.enqueue(&send(id, 1_000)).expect("enqueue");
        assert!(
            matches!(sent, EnqueueOutcome::Inserted { .. }),
            "{id}: {sent:?}"
        );
    }
    other_binary(&dir)
        .execute_batch("UPDATE deliveries SET ledger_id = 'device:b/' || delivery_id")
        .expect("the rows become ones taken in from another device");
    let me = store.identity().expect("identity");
    let claimed = store
        .claim(&ClaimRequest {
            now: 1_001,
            delivery_id: "d-claimed".into(),
            self_identity: me.clone(),
            lane: EffectiveLane::FollowUp,
        })
        .expect("claim");
    assert!(
        matches!(claimed, ClaimOutcome::Claimed { .. }),
        "{claimed:?}"
    );

    let reconciled = store
        .reconcile(&ReconcileRequest {
            now: 1_002,
            target_durable_id: "lead".into(),
            self_identity: me,
            ledger: AdmissionLedger {
                pending: vec!["device:b/d-claimed".into()],
                emitted: vec!["device:b/d-written".into()],
            },
            session_path: None,
        })
        .expect("reconcile");
    assert!(reconciled.queued.is_empty(), "{:?}", reconciled.queued);
    let states: Vec<(String, DeliveryState)> = store
        .list(Some("lead"), None)
        .expect("list")
        .into_iter()
        .map(|row| (row.delivery_id, row.state))
        .collect();
    assert_eq!(
        states,
        vec![
            ("d-written".to_owned(), DeliveryState::Applied),
            ("d-claimed".to_owned(), DeliveryState::Admitted),
        ]
    );

    let continued = store
        .enqueue(&EnqueueRequest {
            target_durable_id: "third".into(),
            sender_principal: "session:lead".into(),
            sender_node: "lead".into(),
            origin: EnvelopeOrigin::Session {
                session: "lead".into(),
            },
            actor: "lead".into(),
            cause_delivery_id: Some("device:b/d-written".into()),
            ..send("d-continued", 1_003)
        })
        .expect("enqueue");
    assert!(
        matches!(continued, EnqueueOutcome::Inserted { .. }),
        "{continued:?}"
    );
}
