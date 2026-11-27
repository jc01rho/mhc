//! The gateway store schema through its public API (omo v5.1.80..v5.1.83 gateway-schema track). A v10 store, the one an
//! mhc install holds today, migrates to the current schema without losing a row (v11..v16: omo#13014, #13037, #14333,
//! #14428); a store another binary left at v14 keeps every column through the v15 rebuild of `deliveries`; and a rebuild
//! that would leave a foreign-key violation keeps the store where it was. The binding-era tables stay in the schema
//! after the binding removal (omo#14312), and nothing the store does writes them (omo#14382).

use std::sync::Arc;

use maho_omo_gateway::gateway_database_path;
use maho_omo_gateway::thread::schema::GATEWAY_MIGRATIONS;
use maho_omo_gateway::thread::store::{
    GatewayStoreOptions, SqliteGatewayStore, create_gateway_store,
};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::store_ops::StoreError;
use maho_omo_gateway::thread::types::{
    ClaimOutcome, ClaimRequest, DeliveryState, EffectiveLane, EnqueueOutcome, EnqueueRequest,
    EnvelopeOrigin, GatewayDeliveryMode, Provenance, ReceiptRef,
};
use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;

type Rows = Vec<Vec<SqlValue>>;

/// Every `deliveries` column the steps after v10 add, in the order the migrated table carries them.
const SINCE_V10: [&str; 5] = [
    "wait_kind",
    "ledger_id",
    "verbatim_text",
    "verbatim_display_text",
    "collapse_key",
];

const V13_V14_TABLES: [&str; 5] = [
    "intake_commands",
    "intake_marks",
    "session_wait",
    "wake_counts",
    "woken_sessions",
];

const CLAIMANT: &str =
    r#"{"pid":1,"process_start_time":null,"instance_id":"i-1","runtime_instance":null}"#;

fn open(dir: &tempfile::TempDir) -> Result<Arc<SqliteGatewayStore>, StoreError> {
    create_gateway_store(GatewayStoreOptions::new(
        dir.path().to_string_lossy().into_owned(),
    ))
}

fn connect(dir: &tempfile::TempDir) -> Connection {
    Connection::open(gateway_database_path(dir.path())).expect("another connection to the store")
}

fn store_at(dir: &tempfile::TempDir, version: usize) -> Connection {
    let path = gateway_database_path(dir.path());
    std::fs::create_dir_all(path.parent().expect("gateway directory")).expect("gateway directory");
    let db = Connection::open(path).expect("open the fixture store");
    db.execute_batch("PRAGMA foreign_keys = ON; BEGIN")
        .expect("begin");
    for step in &GATEWAY_MIGRATIONS[..version] {
        for statement in *step {
            db.execute_batch(statement).expect("migration statement");
        }
    }
    db.execute_batch(&format!("PRAGMA user_version = {version}; COMMIT"))
        .expect("commit");
    db
}

fn envelope(id: &str) -> String {
    format!(
        r#"{{"provenance":{{"workspace":"w","project":"p"}},"origin":{{"session":"A"}},"actor":"A","via":["A"],"root_id":"root-1","hop":1,"delivery_id":"{id}"}}"#
    )
}

struct V10Delivery<'a> {
    id: &'a str,
    seq: i64,
    mode_effective: Option<&'a str>,
    state: &'a str,
    reason: Option<&'a str>,
    admitted_by: Option<&'a str>,
    binding: Option<&'a str>,
    actor_user_id: Option<&'a str>,
}

impl<'a> V10Delivery<'a> {
    fn new(id: &'a str, seq: i64, state: &'a str) -> Self {
        Self {
            id,
            seq,
            mode_effective: None,
            state,
            reason: None,
            admitted_by: None,
            binding: None,
            actor_user_id: None,
        }
    }
}

fn insert_v10(db: &Connection, row: V10Delivery<'_>) {
    let V10Delivery {
        id,
        seq,
        mode_effective,
        state,
        reason,
        admitted_by,
        binding,
        actor_user_id,
    } = row;
    db.execute(
        "INSERT INTO deliveries (delivery_id, target_durable_id, seq, sender, sender_turn, envelope, body, bytes, mode_requested, mode_effective, expected_turn_id, state, reason, admitted_by, claimed_at, attempt, admission_kind, turn_epoch, root_id, hop, created_at, updated_at, expires_at, binding_id, binding_revision, actor_user_id) VALUES (?1, 'B', ?2, 'session:A', 'turn-1', ?3, ?4, 6, 'auto', ?5, 3, ?6, ?7, ?8, 100, 1, 'queued', 3, 'root-1', 1, 10, 20, 30, ?9, ?10, ?11)",
        rusqlite::params![
            id,
            seq,
            envelope(id),
            format!("body {id}"),
            mode_effective,
            state,
            reason,
            admitted_by,
            binding,
            binding.map(|_| 1_i64),
            actor_user_id,
        ],
    )
    .expect("insert a v10 delivery");
}

fn rows(db: &Connection, sql: &str) -> Rows {
    let mut statement = db.prepare(sql).expect("prepare");
    let width = statement.column_count();
    let mapped = statement
        .query_map([], |row| {
            (0..width)
                .map(|index| row.get::<_, SqlValue>(index))
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("query");
    mapped.collect::<Result<Rows, _>>().expect("rows")
}

fn text(value: &SqlValue) -> String {
    match value {
        SqlValue::Text(text) => text.clone(),
        other => format!("{other:?}"),
    }
}

fn texts(db: &Connection, sql: &str) -> Vec<String> {
    rows(db, sql)
        .iter()
        .map(|row| row.iter().map(text).collect::<Vec<_>>().join(":"))
        .collect()
}

fn user_version(db: &Connection) -> i64 {
    db.query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version")
}

fn delivery_columns(db: &Connection) -> Vec<String> {
    texts(db, "SELECT name FROM pragma_table_info('deliveries')")
}

fn delivery_objects(db: &Connection) -> Vec<String> {
    texts(
        db,
        "SELECT type, name FROM sqlite_schema WHERE tbl_name = 'deliveries' ORDER BY type, name",
    )
}

fn thread_state_tables(db: &Connection) -> Vec<Rows> {
    [
        "bindings",
        "outbox",
        "session_meta",
        "receipts",
        "causal_roots",
        "thread_creations",
        "alpha_refs",
    ]
    .iter()
    .map(|table| rows(db, &format!("SELECT rowid, * FROM {table} ORDER BY rowid")))
    .collect()
}

#[test]
fn a_v10_store_migrates_to_the_current_schema_and_keeps_every_row() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = store_at(&dir, 10);
    insert_v10(
        &db,
        V10Delivery {
            mode_effective: Some("steer"),
            admitted_by: Some(CLAIMANT),
            ..V10Delivery::new("d-gone", 1, "applied")
        },
    );
    insert_v10(
        &db,
        V10Delivery {
            mode_effective: Some("steer"),
            admitted_by: Some(CLAIMANT),
            actor_user_id: Some("U1"),
            ..V10Delivery::new("d-steer", 2, "admitted")
        },
    );
    insert_v10(
        &db,
        V10Delivery {
            mode_effective: Some("follow_up"),
            admitted_by: Some(CLAIMANT),
            binding: Some("bnd-1"),
            ..V10Delivery::new("d-follow", 3, "applied")
        },
    );
    insert_v10(&db, V10Delivery::new("d-queued", 4, "queued"));
    insert_v10(
        &db,
        V10Delivery {
            reason: Some("expired"),
            ..V10Delivery::new("d-refused", 5, "refused")
        },
    );
    // A deleted row leaves a rowid gap the rebuild must not close.
    db.execute_batch(
        r#"DELETE FROM deliveries WHERE delivery_id = 'd-gone';
        INSERT INTO bindings (binding_id, revision, status, platform, account_id, chat_id, thread_id, session_realm_id, session_durable_id, direction_inbound, direction_outbound, inbound_mode, outbound_events, policy_id, created_at, updated_at, lease_started_at) VALUES ('bnd-1', 1, 'active', 'slack', 'acct', 'C1', '@chat', 'realm-1', 'B', 1, 1, 'auto', '[]', 'policy', '2026-11-25T00:00:00.000Z', '2026-11-25T00:00:00.000Z', '2026-11-25T00:00:00.000Z');
        INSERT INTO outbox (binding_id, revision, event_kind, payload, state, created_at, session_durable_id) VALUES ('bnd-1', 1, 'report', '{"text":"hi"}', 'pending', 10, 'B');
        INSERT INTO session_meta (durable_id, next_seq, applied_seq) VALUES ('B', 6, 3);
        INSERT INTO receipts (principal, operation, idempotency_key, args_hash, status, delivery_id, owner_instance, created_at, updated_at, expires_at) VALUES ('session:A', 'deliver', 'k-1', 'h', 'completed', 'd-follow', 'i-1', 10, 20, 99);
        INSERT INTO causal_roots (root_id, origin_principal, created_at, expires_at) VALUES ('root-1', 'session:A', 10, 99);
        INSERT INTO thread_creations (creator_durable_id, created_durable_id, created_at) VALUES ('A', 'B', 10);
        CREATE TABLE alpha_refs (id INTEGER PRIMARY KEY, delivery_id TEXT REFERENCES deliveries ON DELETE CASCADE);
        INSERT INTO alpha_refs (id, delivery_id) VALUES (1, 'd-follow'), (2, 'd-queued');"#,
    )
    .expect("v10 fixture rows");
    let deliveries_before = rows(&db, "SELECT rowid, * FROM deliveries ORDER BY rowid");
    let columns_before = delivery_columns(&db);
    let objects_before = delivery_objects(&db);
    let registry_before = texts(
        &db,
        "SELECT type, name FROM extension_objects ORDER BY type, name",
    );
    let others_before = thread_state_tables(&db);
    drop(db);

    let store = open(&dir).expect("the current build opens the v10 store");

    let db = connect(&dir);
    assert_eq!(user_version(&db), GATEWAY_MIGRATIONS.len() as i64);
    let expected_deliveries: Rows = deliveries_before
        .iter()
        .map(|row| {
            let mut row = row.clone();
            row.resize(row.len() + SINCE_V10.len(), SqlValue::Null);
            row
        })
        .collect();
    assert_eq!(
        rows(&db, "SELECT rowid, * FROM deliveries ORDER BY rowid"),
        expected_deliveries
    );
    let mut expected_columns = columns_before;
    expected_columns.extend(SINCE_V10.iter().map(|name| (*name).to_owned()));
    assert_eq!(delivery_columns(&db), expected_columns);
    let mut expected_objects = objects_before;
    expected_objects.extend([
        "index:deliveries_collapse".to_owned(),
        "index:deliveries_ledger".to_owned(),
    ]);
    expected_objects.sort();
    assert_eq!(delivery_objects(&db), expected_objects);
    assert_eq!(thread_state_tables(&db), others_before);
    let mut expected_registry = registry_before;
    expected_registry.extend(
        [
            "index:deliveries_ledger",
            "index:woken_sessions_state",
            "table:intake_commands",
            "table:intake_marks",
            "table:session_wait",
            "table:wake_counts",
            "table:woken_sessions",
        ]
        .iter()
        .map(|entry| (*entry).to_owned()),
    );
    expected_registry.sort();
    assert_eq!(
        texts(
            &db,
            "SELECT type, name FROM extension_objects ORDER BY type, name"
        ),
        expected_registry
    );
    assert_eq!(
        texts(
            &db,
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name IN ('woken_sessions', 'session_wait', 'wake_counts', 'intake_marks', 'intake_commands') ORDER BY name"
        ),
        V13_V14_TABLES.to_vec()
    );
    assert!(rows(&db, "PRAGMA foreign_key_check").is_empty());
    assert_eq!(
        texts(
            &db,
            "SELECT value FROM gateway_meta WHERE key = 'reader_floor'"
        ),
        vec!["14".to_owned()]
    );
    // The v8 provenance trigger came back with the rebuilt table: a delivery without provenance is still refused.
    let bare = db
        .execute_batch(
            "INSERT INTO deliveries (delivery_id, target_durable_id, seq, sender, envelope, body, bytes, mode_requested, state, root_id, hop, created_at, updated_at, expires_at) VALUES ('d-bare', 'B', 9, 'session:A', '{\"origin\":{}}', 'x', 1, 'auto', 'queued', 'root-1', 1, 1, 1, 1)",
        )
        .expect_err("a delivery without provenance is refused");
    assert!(
        bare.to_string().contains("delivery provenance is required"),
        "{bare}"
    );
    drop(db);

    // The migrated store serves the thread's rows, and a delivery can record the start lane v15 widened the CHECK for.
    let listed = store.list(Some("B"), None).expect("list");
    assert_eq!(
        listed
            .iter()
            .map(|row| (row.delivery_id.as_str(), row.state))
            .collect::<Vec<_>>(),
        vec![
            ("d-steer", DeliveryState::Admitted),
            ("d-follow", DeliveryState::Applied),
            ("d-queued", DeliveryState::Queued),
            ("d-refused", DeliveryState::Refused),
        ]
    );
    let claimed = store
        .claim(&ClaimRequest {
            now: 20,
            delivery_id: "d-queued".into(),
            self_identity: store.identity().expect("identity"),
            lane: EffectiveLane::Start,
        })
        .expect("claim");
    match claimed {
        ClaimOutcome::Claimed { row } => assert_eq!(row.mode_effective, Some(EffectiveLane::Start)),
        other => panic!("expected the queued row claimed, got {other:?}"),
    }
}

/// A v14 delivery with every column set, the later-added ones included, so a dropped or reordered column shows.
fn insert_v14(db: &Connection, id: &str, seq: i64, effective: Option<&str>, ledger: Option<&str>) {
    db.execute(
        "INSERT INTO deliveries (delivery_id, target_durable_id, seq, sender, sender_turn, envelope, body, bytes, mode_requested, mode_effective, expected_turn_id, state, reason, admitted_by, claimed_at, attempt, admission_kind, turn_epoch, root_id, hop, created_at, updated_at, expires_at, binding_id, binding_revision, actor_user_id, wait_kind, ledger_id, verbatim_text, verbatim_display_text) VALUES (?1, 'B', ?2, 'session:A', 'turn-1', ?3, ?4, 6, 'auto', ?5, 3, 'admitted', NULL, ?6, 100, 1, 'queued', 3, 'root-1', 1, 10, 20, 30, 'bnd-1', 1, 'U1', 'approval', ?7, ?8, ?9)",
        rusqlite::params![
            id,
            seq,
            envelope(id),
            format!("body {id}"),
            effective,
            CLAIMANT,
            ledger,
            ledger.map(|_| "verbatim"),
            ledger.map(|_| "shown"),
        ],
    )
    .expect("insert a v14 delivery");
}

fn v14_fixture(dir: &tempfile::TempDir, extra: &str) {
    let db = store_at(dir, 14);
    insert_v14(&db, "d-steer", 1, Some("steer"), None);
    insert_v14(&db, "d-follow", 2, Some("follow_up"), None);
    insert_v14(&db, "d-intake", 3, None, Some("device:b/d-intake"));
    db.execute_batch("DELETE FROM deliveries WHERE delivery_id = 'd-steer'")
        .expect("leave a rowid gap");
    insert_v14(&db, "d-steer", 4, Some("steer"), None);
    db.execute_batch(extra).expect("fixture extras");
}

#[test]
fn a_v14_store_another_binary_left_keeps_every_column_through_the_v15_rebuild() {
    let dir = tempfile::tempdir().expect("temp dir");
    v14_fixture(&dir, "");
    let db = connect(&dir);
    let before = rows(&db, "SELECT rowid, * FROM deliveries ORDER BY rowid");
    drop(db);

    let _store = open(&dir).expect("the current build opens the v14 store");

    let db = connect(&dir);
    assert_eq!(user_version(&db), GATEWAY_MIGRATIONS.len() as i64);
    let expected: Rows = before
        .iter()
        .map(|row| {
            let mut row = row.clone();
            row.push(SqlValue::Null);
            row
        })
        .collect();
    assert_eq!(
        rows(&db, "SELECT rowid, * FROM deliveries ORDER BY rowid"),
        expected
    );
    // The ledger index came back with the rebuilt table: a second row under one ledger key is refused.
    let duplicate = db
        .execute(
            "UPDATE deliveries SET ledger_id = 'device:b/d-intake' WHERE delivery_id = 'd-follow'",
            [],
        )
        .expect_err("one ledger key holds one row");
    assert!(duplicate.to_string().contains("UNIQUE"), "{duplicate}");
}

#[test]
fn a_rebuild_that_would_leave_a_foreign_key_violation_keeps_the_store_at_v14_untouched() {
    let dir = tempfile::tempdir().expect("temp dir");
    v14_fixture(
        &dir,
        "PRAGMA foreign_keys = OFF;
        CREATE TABLE alpha_refs (id INTEGER PRIMARY KEY, delivery_id TEXT REFERENCES deliveries);
        INSERT INTO alpha_refs (id, delivery_id) VALUES (1, 'd-missing');
        PRAGMA foreign_keys = ON;",
    );
    let db = connect(&dir);
    let before = (
        rows(&db, "SELECT rowid, * FROM deliveries ORDER BY rowid"),
        delivery_columns(&db),
        delivery_objects(&db),
    );
    drop(db);

    let Err(error) = open(&dir) else {
        panic!("a rebuild that leaves a violation is refused");
    };
    assert_eq!(error.code(), "gateway_migration_failed");
    assert!(
        error.message().contains("deliveries"),
        "{}",
        error.message()
    );

    let db = connect(&dir);
    assert_eq!(user_version(&db), 14);
    assert_eq!(
        (
            rows(&db, "SELECT rowid, * FROM deliveries ORDER BY rowid"),
            delivery_columns(&db),
            delivery_objects(&db),
        ),
        before
    );
}

/// A follow-up from session A to session B, which the v14 fixture's deliveries already address.
fn send_to_b(delivery_id: &str, now: i64) -> EnqueueRequest {
    EnqueueRequest {
        provenance: Provenance {
            workspace: "w".into(),
            project: "p".into(),
            lane: None,
            session: None,
        },
        now,
        delivery_id: delivery_id.into(),
        target_durable_id: "B".into(),
        sender_principal: "session:A".into(),
        rate_principal: None,
        sender_node: "A".into(),
        sender_turn: None,
        cause_delivery_id: None,
        claimed_root_id: None,
        origin: EnvelopeOrigin::Session {
            session: "A".into(),
        },
        actor: "A".into(),
        body: format!("body of {delivery_id}"),
        mode: GatewayDeliveryMode::FollowUp,
        expected_turn_id: None,
        receipt: ReceiptRef {
            idempotency_key: format!("k-{delivery_id}"),
            args_hash: format!("h-{delivery_id}"),
        },
        endpoint_kind: None,
        collapse_key: None,
    }
}

/// Every row of the binding-era tables, which stay in the schema so an omo still on schema 14 keeps reading them.
fn binding_era_rows(db: &Connection) -> Vec<Rows> {
    ["bindings", "outbox", "outbox_cursors", "completion_arms"]
        .iter()
        .map(|table| rows(db, &format!("SELECT rowid, * FROM {table} ORDER BY rowid")))
        .collect()
}

#[test]
fn a_v14_store_with_binding_era_rows_keeps_them_untouched_through_a_send_a_claim_and_a_list() {
    let dir = tempfile::tempdir().expect("temp dir");
    v14_fixture(
        &dir,
        r#"INSERT INTO bindings (binding_id, revision, status, platform, account_id, chat_id, thread_id, session_realm_id, session_durable_id, direction_inbound, direction_outbound, inbound_mode, outbound_events, policy_id, created_at, updated_at, lease_started_at) VALUES ('bnd-1', 1, 'active', 'slack', 'acct', 'C1', '@chat', 'realm-1', 'B', 1, 1, 'auto', '["report"]', 'policy', '2026-11-25T00:00:00.000Z', '2026-11-25T00:00:00.000Z', '2026-11-25T00:00:00.000Z');
        INSERT INTO outbox (binding_id, revision, event_kind, payload, state, created_at) VALUES ('bnd-1', 1, 'report', '{}', 'pending', 1);
        INSERT INTO outbox_cursors (binding_id, acked_cursor, updated_at) VALUES ('bnd-1', 0, 1);
        INSERT INTO completion_arms (session_durable_id, binding_id, revision, text, armed_at) VALUES ('B', 'bnd-1', 1, 'done', 1);"#,
    );
    let db = connect(&dir);
    let before = binding_era_rows(&db);
    assert!(
        before.iter().all(|table| table.len() == 1),
        "one row in each binding-era table: {before:?}"
    );
    drop(db);

    let store = open(&dir).expect("the current build opens the v14 store");
    let sent = store.enqueue(&send_to_b("d-new", 40)).expect("enqueue");
    assert!(matches!(sent, EnqueueOutcome::Inserted { .. }), "{sent:?}");
    let claimed = store
        .claim(&ClaimRequest {
            now: 41,
            delivery_id: "d-new".into(),
            self_identity: store.identity().expect("identity"),
            lane: EffectiveLane::FollowUp,
        })
        .expect("claim");
    assert!(
        matches!(claimed, ClaimOutcome::Claimed { .. }),
        "{claimed:?}"
    );
    let listed = store.list(Some("B"), None).expect("list");
    assert!(listed.iter().any(|row| row.delivery_id == "d-new"));

    assert_eq!(binding_era_rows(&connect(&dir)), before);
}

/// omo 7f068c4d32: a foreign client that takes itself for the last connection removes the `-wal` and `-shm` files an
/// open connection uses, and the writes made after that land in an unlinked WAL no other process reads.
#[cfg(unix)]
#[test]
fn a_store_whose_wal_files_a_foreign_close_removed_writes_where_other_connections_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(&dir).expect("the store opens");
    let database = gateway_database_path(dir.path());
    store
        .enqueue(&send_to_b("d-before", 40))
        .expect("the send before the removal");
    // Another process folds the WAL into the database file, so only the later send depends on the files' fate.
    let busy: i64 = connect(&dir)
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .expect("checkpoint");
    assert_eq!(busy, 0, "the checkpoint ran to the end");
    for suffix in ["-wal", "-shm"] {
        let mut name = database.clone().into_os_string();
        name.push(suffix);
        std::fs::remove_file(&name).expect("a foreign close removes the file");
    }

    store
        .enqueue(&send_to_b("d-after", 41))
        .expect("the send after the removal");

    assert_eq!(
        texts(
            &connect(&dir),
            "SELECT delivery_id FROM deliveries ORDER BY seq"
        ),
        vec!["d-before".to_owned(), "d-after".to_owned()]
    );
}
