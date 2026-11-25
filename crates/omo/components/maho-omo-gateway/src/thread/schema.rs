//! Gateway store schema.
//!
//! Upstream counterpart: `thread/gateway/schema.ts`. Steps are append-only; a store rewound to an
//! older version replays them, so every step is idempotent where it touches surviving objects.

use super::provenance_record::{PROVENANCE_OBJECT, PROVENANCE_TRIGGER};

/// A store whose schema is newer than this binary refuses to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewaySchemaVersionError {
    pub found: i64,
    pub supported: i64,
    pub scope: String,
}

impl GatewaySchemaVersionError {
    pub fn new(found: i64, supported: i64, scope: impl Into<String>) -> Self {
        Self {
            found,
            supported,
            scope: scope.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        "gateway_schema_too_new"
    }
}

impl std::fmt::Display for GatewaySchemaVersionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} schema {} is newer than this binary's supported version {}.",
            self.scope, self.found, self.supported
        )
    }
}

impl std::error::Error for GatewaySchemaVersionError {}

const DELIVERIES_TABLE: &str = r#"CREATE TABLE deliveries (
      delivery_id TEXT PRIMARY KEY,
      target_durable_id TEXT NOT NULL,
      seq INTEGER NOT NULL,
      sender TEXT NOT NULL,
      sender_turn TEXT,
      envelope TEXT NOT NULL,
      body TEXT NOT NULL,
      bytes INTEGER NOT NULL,
      mode_requested TEXT NOT NULL CHECK (mode_requested IN ('auto', 'steer', 'follow_up')),
      mode_effective TEXT CHECK (mode_effective IS NULL OR mode_effective IN ('steer', 'follow_up')),
      expected_turn_id INTEGER,
      state TEXT NOT NULL CHECK (state IN ('queued', 'admitting', 'admitted', 'applied', 'refused', 'uncertain')),
      reason TEXT,
      admitted_by TEXT,
      claimed_at INTEGER,
      attempt INTEGER NOT NULL DEFAULT 0,
      admission_kind TEXT,
      turn_epoch INTEGER,
      root_id TEXT NOT NULL,
      hop INTEGER NOT NULL,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL,
      binding_id TEXT,
      binding_revision INTEGER,
      UNIQUE (target_durable_id, seq)
    )"#;

const RECEIPTS_TABLE: &str = r#"CREATE TABLE receipts (
      principal TEXT NOT NULL,
      operation TEXT NOT NULL,
      idempotency_key TEXT NOT NULL,
      args_hash TEXT NOT NULL,
      status TEXT NOT NULL CHECK (status IN ('prepared', 'completed', 'uncertain')),
      delivery_id TEXT,
      owner_instance TEXT NOT NULL,
      result TEXT,
      error_note TEXT,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL,
      PRIMARY KEY (principal, operation, idempotency_key)
    )"#;

const OUTBOX_TABLE: &str = r#"CREATE TABLE outbox (
      cursor INTEGER PRIMARY KEY AUTOINCREMENT,
      binding_id TEXT NOT NULL,
      revision INTEGER NOT NULL,
      event_kind TEXT NOT NULL CHECK (event_kind IN ('milestone', 'report', 'question', 'completion', 'question_closed')),
      payload TEXT NOT NULL,
      state TEXT NOT NULL CHECK (state IN ('pending', 'acked')),
      provider_message_id TEXT,
      created_at INTEGER NOT NULL,
      acked_at INTEGER
    )"#;

const BINDINGS_V7_TABLE: &str = r#"CREATE TABLE bindings_v7 (
      seq INTEGER PRIMARY KEY AUTOINCREMENT,
      binding_id TEXT NOT NULL UNIQUE,
      schema_version INTEGER NOT NULL DEFAULT 1,
      revision INTEGER NOT NULL CHECK (revision >= 1),
      status TEXT NOT NULL CHECK (status IN ('active', 'detached', 'expired')),
      platform TEXT NOT NULL CHECK (platform IN ('discord', 'telegram', 'slack', 'notion', 'feishu', 'herdr', 'custom', 'whatsapp')),
      account_id TEXT NOT NULL,
      chat_id TEXT NOT NULL,
      thread_id TEXT NOT NULL,
      root_message_id TEXT,
      progress_message_id TEXT,
      session_realm_id TEXT NOT NULL,
      session_durable_id TEXT NOT NULL,
      direction_inbound INTEGER NOT NULL CHECK (direction_inbound IN (0, 1)),
      direction_outbound INTEGER NOT NULL CHECK (direction_outbound IN (0, 1)),
      inbound_mode TEXT NOT NULL CHECK (inbound_mode IN ('auto', 'follow_up')),
      outbound_events TEXT NOT NULL,
      policy_id TEXT NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      lease_started_at TEXT NOT NULL,
      ttl_seconds INTEGER,
      expires_at TEXT,
      CHECK (direction_inbound = 1 OR direction_outbound = 1)
    )"#;

/// The `deliveries` table as v15 rebuilds it: `mode_effective` takes `start`, the lane an idle session's delivery
/// takes (omo#14333). SQLite cannot widen a CHECK in place, so the step copies every v14 column and the rowid over.
const DELIVERIES_V15_TABLE: &str = r#"CREATE TABLE deliveries_v15 (
      delivery_id TEXT PRIMARY KEY,
      target_durable_id TEXT NOT NULL,
      seq INTEGER NOT NULL,
      sender TEXT NOT NULL,
      sender_turn TEXT,
      envelope TEXT NOT NULL,
      body TEXT NOT NULL,
      bytes INTEGER NOT NULL,
      mode_requested TEXT NOT NULL CHECK (mode_requested IN ('auto', 'steer', 'follow_up')),
      mode_effective TEXT CHECK (mode_effective IS NULL OR mode_effective IN ('start', 'steer', 'follow_up')),
      expected_turn_id INTEGER,
      state TEXT NOT NULL CHECK (state IN ('queued', 'admitting', 'admitted', 'applied', 'refused', 'uncertain')),
      reason TEXT,
      admitted_by TEXT,
      claimed_at INTEGER,
      attempt INTEGER NOT NULL DEFAULT 0,
      admission_kind TEXT,
      turn_epoch INTEGER,
      root_id TEXT NOT NULL,
      hop INTEGER NOT NULL,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL,
      binding_id TEXT,
      binding_revision INTEGER,
      actor_user_id TEXT,
      wait_kind TEXT CHECK (wait_kind IS NULL OR wait_kind = 'approval'),
      ledger_id TEXT,
      verbatim_text TEXT,
      verbatim_display_text TEXT,
      UNIQUE (target_durable_id, seq)
    )"#;

/// Every migration step, in order; `user_version` counts them.
pub static GATEWAY_MIGRATIONS: &[&[&str]] = &[
    &[
        DELIVERIES_TABLE,
        "CREATE INDEX deliveries_target_state_seq ON deliveries (target_durable_id, state, seq)",
        "CREATE INDEX deliveries_root ON deliveries (root_id)",
        "CREATE INDEX deliveries_sender_turn ON deliveries (sender, sender_turn)",
        RECEIPTS_TABLE,
        "CREATE INDEX receipts_expiry ON receipts (expires_at)",
        r#"CREATE TABLE causal_roots (
      root_id TEXT PRIMARY KEY,
      origin_principal TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL
    )"#,
        r#"CREATE TABLE causal_edges (
      root_id TEXT NOT NULL,
      from_durable_id TEXT NOT NULL,
      to_durable_id TEXT NOT NULL,
      delivery_id TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      PRIMARY KEY (root_id, from_durable_id, to_durable_id)
    )"#,
        r#"CREATE TABLE rate_buckets (
      sender TEXT NOT NULL,
      target_durable_id TEXT NOT NULL,
      tokens REAL NOT NULL,
      updated_at INTEGER NOT NULL,
      PRIMARY KEY (sender, target_durable_id)
    )"#,
        r#"CREATE TABLE session_meta (
      durable_id TEXT PRIMARY KEY,
      next_seq INTEGER NOT NULL,
      applied_seq INTEGER NOT NULL DEFAULT 0
    )"#,
        // `seq` is the rowid the bindings snapshot watermarks on: AUTOINCREMENT never hands a
        // deleted binding's rowid to a later one.
        "CREATE TABLE bindings (\n      seq INTEGER PRIMARY KEY AUTOINCREMENT,\n      binding_id TEXT NOT NULL UNIQUE,\n      schema_version INTEGER NOT NULL DEFAULT 1,\n      revision INTEGER NOT NULL CHECK (revision >= 1),\n      status TEXT NOT NULL CHECK (status IN ('active', 'detached', 'expired')),\n      platform TEXT NOT NULL CHECK (platform IN ('discord', 'telegram', 'slack', 'notion', 'feishu', 'herdr', 'custom', 'whatsapp')),\n      account_id TEXT NOT NULL,\n      chat_id TEXT NOT NULL,\n      thread_id TEXT NOT NULL,\n      root_message_id TEXT,\n      progress_message_id TEXT,\n      session_realm_id TEXT NOT NULL,\n      session_durable_id TEXT NOT NULL,\n      direction_inbound INTEGER NOT NULL CHECK (direction_inbound IN (0, 1)),\n      direction_outbound INTEGER NOT NULL CHECK (direction_outbound IN (0, 1)),\n      inbound_mode TEXT NOT NULL CHECK (inbound_mode IN ('auto', 'follow_up')),\n      outbound_events TEXT NOT NULL,\n      policy_id TEXT NOT NULL,\n      created_at TEXT NOT NULL,\n      updated_at TEXT NOT NULL,\n      lease_started_at TEXT NOT NULL,\n      ttl_seconds INTEGER,\n      expires_at TEXT,\n      CHECK (direction_inbound = 1 OR direction_outbound = 1)\n    )",
        "CREATE UNIQUE INDEX bindings_one_active_thread ON bindings (platform, account_id, chat_id, thread_id) WHERE status = 'active'",
        "CREATE INDEX bindings_session ON bindings (session_durable_id, status)",
        OUTBOX_TABLE,
        "CREATE INDEX outbox_binding_cursor ON outbox (binding_id, cursor)",
        r#"CREATE TABLE gateway_meta (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL
    )"#,
    ],
    // v2: the relay side of bindings (incarnation, question/answer columns, consumer cursors, arms).
    &[
        "ALTER TABLE session_meta ADD COLUMN incarnation TEXT",
        "ALTER TABLE outbox ADD COLUMN session_durable_id TEXT",
        "ALTER TABLE outbox ADD COLUMN reply_token TEXT",
        "ALTER TABLE outbox ADD COLUMN ui_request_id TEXT",
        "ALTER TABLE outbox ADD COLUMN incarnation TEXT",
        "ALTER TABLE outbox ADD COLUMN question_state TEXT CHECK (question_state IS NULL OR question_state IN ('pending', 'answered', 'expired', 'cancelled'))",
        "ALTER TABLE outbox ADD COLUMN answer TEXT",
        "ALTER TABLE outbox ADD COLUMN answered_at INTEGER",
        "ALTER TABLE outbox ADD COLUMN outcome TEXT CHECK (outcome IS NULL OR outcome IN ('completed', 'failed', 'cancelled'))",
        "CREATE UNIQUE INDEX outbox_reply_token ON outbox (reply_token) WHERE reply_token IS NOT NULL",
        r#"CREATE TABLE outbox_cursors (
      binding_id TEXT PRIMARY KEY,
      acked_cursor INTEGER NOT NULL,
      updated_at INTEGER NOT NULL
    )"#,
        // One row per arm: a later run's arm must not replace the arm of a run that settled while its
        // completion write was still outstanding.
        r#"CREATE TABLE completion_arms (
      arm_seq INTEGER PRIMARY KEY AUTOINCREMENT,
      session_durable_id TEXT NOT NULL,
      binding_id TEXT NOT NULL,
      revision INTEGER NOT NULL,
      text TEXT NOT NULL,
      armed_at INTEGER NOT NULL
    )"#,
        "CREATE INDEX completion_arms_session ON completion_arms (session_durable_id, armed_at)",
        "INSERT OR IGNORE INTO gateway_meta (key, value) VALUES ('realm_id', 'realm-' || lower(hex(randomblob(16))))",
        "INSERT OR IGNORE INTO gateway_meta (key, value) VALUES ('token_secret', lower(hex(randomblob(32))))",
    ],
    // v3: which extension UI request a question row answers, and whether a claimed answer is in flight.
    &[
        "ALTER TABLE outbox ADD COLUMN ui_request_kind TEXT CHECK (ui_request_kind IS NULL OR ui_request_kind IN ('question', 'select', 'confirm', 'input', 'editor'))",
        "ALTER TABLE outbox ADD COLUMN answer_state TEXT CHECK (answer_state IS NULL OR answer_state IN ('in_flight', 'delivered'))",
    ],
    // v4: who answered a question.
    &["ALTER TABLE outbox ADD COLUMN answered_by TEXT"],
    // v5: the session's published control endpoint, fenced by its existing incarnation.
    &[
        "ALTER TABLE session_meta ADD COLUMN endpoint_socket TEXT",
        "ALTER TABLE session_meta ADD COLUMN endpoint_kind TEXT CHECK (endpoint_kind IS NULL OR endpoint_kind IN ('tui', 'rpc_host'))",
    ],
    // v6: the store extension registry and the connector author an extension enqueue records.
    &[
        "CREATE TABLE extension_schema (name TEXT PRIMARY KEY, version INTEGER NOT NULL, updated_at INTEGER NOT NULL)",
        "ALTER TABLE deliveries ADD COLUMN actor_user_id TEXT",
        "CREATE TABLE extension_objects (type TEXT NOT NULL, name TEXT NOT NULL, owner TEXT, PRIMARY KEY (type, name))",
        "INSERT INTO extension_objects (type, name, owner) SELECT type, name, NULL FROM sqlite_schema",
    ],
    // v7: `whatsapp` is a binding platform; SQLite cannot widen a CHECK in place, so `bindings` is rebuilt.
    // This step runs with foreign_keys OFF, and the AUTOINCREMENT high-water mark is carried over.
    &[
        BINDINGS_V7_TABLE,
        "INSERT INTO bindings_v7 SELECT seq, binding_id, schema_version, revision, status, platform, account_id, chat_id, thread_id, root_message_id, progress_message_id, session_realm_id, session_durable_id, direction_inbound, direction_outbound, inbound_mode, outbound_events, policy_id, created_at, updated_at, lease_started_at, ttl_seconds, expires_at FROM bindings",
        "UPDATE sqlite_sequence SET seq = (SELECT seq FROM sqlite_sequence WHERE name = 'bindings') WHERE name = 'bindings_v7'",
        "DROP TABLE bindings",
        "ALTER TABLE bindings_v7 RENAME TO bindings",
        "CREATE UNIQUE INDEX bindings_one_active_thread ON bindings (platform, account_id, chat_id, thread_id) WHERE status = 'active'",
        "CREATE INDEX bindings_session ON bindings (session_durable_id, status)",
    ],
    // v8: new deliveries require sender provenance; old envelopes are not rewritten.
    &[PROVENANCE_TRIGGER, PROVENANCE_OBJECT],
    // v9: who thread_create'd which session, and each extension's persisted descriptor. A store a build
    // from before the renumber migrated already sits at v8 with these tables but without the provenance
    // trigger, so this step replays v8 (idempotent) and creates only what is missing.
    &[
        PROVENANCE_TRIGGER,
        PROVENANCE_OBJECT,
        r#"CREATE TABLE IF NOT EXISTS thread_creations (
      creator_durable_id TEXT NOT NULL,
      created_durable_id TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      PRIMARY KEY (creator_durable_id, created_durable_id)
    )"#,
        "CREATE TABLE IF NOT EXISTS extension_registrations (name TEXT PRIMARY KEY, descriptor_json TEXT NOT NULL, updated_at INTEGER NOT NULL)",
        r#"INSERT OR IGNORE INTO extension_objects (type, name, owner)
     SELECT s.type, s.name, NULL FROM sqlite_schema s
     WHERE NOT EXISTS (SELECT 1 FROM extension_objects e WHERE e.type = s.type AND e.name = s.name COLLATE NOCASE)"#,
    ],
    // v10: a receipts table (kept for existing stores; no longer written).
    &[
        PROVENANCE_TRIGGER,
        PROVENANCE_OBJECT,
        r#"CREATE TABLE IF NOT EXISTS toolkit_replay_receipts (
      replay_key TEXT PRIMARY KEY,
      op TEXT NOT NULL,
      args_sha256 TEXT NOT NULL,
      result_json TEXT NOT NULL,
      turn_at INTEGER NOT NULL,
      created_at INTEGER NOT NULL
    )"#,
        "CREATE INDEX IF NOT EXISTS toolkit_replay_receipts_turn_at ON toolkit_replay_receipts (turn_at)",
        r#"INSERT OR IGNORE INTO extension_objects (type, name, owner)
     SELECT s.type, s.name, NULL FROM sqlite_schema s
     WHERE NOT EXISTS (SELECT 1 FROM extension_objects e WHERE e.type = s.type AND e.name = s.name COLLATE NOCASE)"#,
    ],
    // v11: what the receiver was parked on when its drain queued a delivery (omo#13014); NULL when it was not, and
    // on every older row.
    &[
        "ALTER TABLE deliveries ADD COLUMN wait_kind TEXT CHECK (wait_kind IS NULL OR wait_kind = 'approval')",
    ],
    // v12: a delivery taken in from another device for a session that is not running (omo#13037). `ledger_id` is the
    // engine's ledger key for it; the two text columns hold the sender's rendered text verbatim. All three are NULL
    // on every other row, which behaves exactly as before.
    &[
        "ALTER TABLE deliveries ADD COLUMN ledger_id TEXT",
        "ALTER TABLE deliveries ADD COLUMN verbatim_text TEXT",
        "ALTER TABLE deliveries ADD COLUMN verbatim_display_text TEXT",
        "CREATE UNIQUE INDEX deliveries_ledger ON deliveries (ledger_id) WHERE ledger_id IS NOT NULL",
        "INSERT OR IGNORE INTO extension_objects (type, name, owner) VALUES ('index', 'deliveries_ledger', NULL)",
    ],
    // v13: one row per session a wake asked for or opened and that has not ended (omo#13037). Idempotent, so a store
    // rewound to v12 re-runs it without touching the rows it holds.
    &[
        r#"CREATE TABLE IF NOT EXISTS woken_sessions (
      durable_id TEXT PRIMARY KEY,
      session_file TEXT NOT NULL,
      host_instance TEXT,
      state TEXT NOT NULL CHECK (state IN ('requested', 'opening', 'open', 'closing')),
      requested_at INTEGER NOT NULL,
      opened_at INTEGER,
      first_waiting_at INTEGER,
      lease_owner TEXT,
      lease_expires_at INTEGER,
      generation INTEGER NOT NULL DEFAULT 0,
      attempts INTEGER NOT NULL DEFAULT 0,
      next_attempt_at INTEGER
    )"#,
        "CREATE INDEX IF NOT EXISTS woken_sessions_state ON woken_sessions (state)",
        "INSERT OR IGNORE INTO extension_objects (type, name, owner) VALUES ('table', 'woken_sessions', NULL)",
        "INSERT OR IGNORE INTO extension_objects (type, name, owner) VALUES ('index', 'woken_sessions_state', NULL)",
    ],
    // v14: what the carrier and the consumer keep beside the woken row (omo#13037): the session's wait and hold, the
    // consecutive wake count, and the intake marks and commands. Idempotent like v13.
    &[
        r#"CREATE TABLE IF NOT EXISTS session_wait (
      durable_id TEXT PRIMARY KEY,
      wait TEXT NOT NULL CHECK (wait IN ('held', 'waiting')),
      hold_kinds TEXT NOT NULL,
      file_size INTEGER NOT NULL,
      written_at INTEGER NOT NULL
    )"#,
        r#"CREATE TABLE IF NOT EXISTS wake_counts (
      durable_id TEXT PRIMARY KEY,
      count INTEGER NOT NULL,
      human_input_revision INTEGER
    )"#,
        r#"CREATE TABLE IF NOT EXISTS intake_marks (
      sender_device TEXT NOT NULL,
      session_id TEXT NOT NULL,
      last_command_id TEXT NOT NULL,
      updated_at INTEGER NOT NULL,
      PRIMARY KEY (sender_device, session_id)
    )"#,
        "INSERT OR IGNORE INTO extension_objects (type, name, owner) VALUES ('table', 'session_wait', NULL)",
        "INSERT OR IGNORE INTO extension_objects (type, name, owner) VALUES ('table', 'wake_counts', NULL)",
        r#"CREATE TABLE IF NOT EXISTS intake_commands (
      ledger_id TEXT PRIMARY KEY,
      command_id TEXT NOT NULL
    )"#,
        "INSERT OR IGNORE INTO extension_objects (type, name, owner) VALUES ('table', 'intake_marks', NULL)",
        "INSERT OR IGNORE INTO extension_objects (type, name, owner) VALUES ('table', 'intake_commands', NULL)",
    ],
    // v15: `start` is a lane (omo#14333). SQLite cannot widen a CHECK in place, so `deliveries` is rebuilt the way v7
    // rebuilt `bindings`: every column and rowid carried over, the four indexes and the v8 provenance trigger
    // recreated. It runs with foreign_keys OFF and a foreign_key_check on `deliveries` before the bump.
    &[
        DELIVERIES_V15_TABLE,
        r#"INSERT INTO deliveries_v15 (rowid, delivery_id, target_durable_id, seq, sender, sender_turn, envelope, body, bytes, mode_requested, mode_effective, expected_turn_id, state, reason, admitted_by, claimed_at, attempt, admission_kind, turn_epoch, root_id, hop, created_at, updated_at, expires_at, binding_id, binding_revision, actor_user_id, wait_kind, ledger_id, verbatim_text, verbatim_display_text)
     SELECT rowid, delivery_id, target_durable_id, seq, sender, sender_turn, envelope, body, bytes, mode_requested, mode_effective, expected_turn_id, state, reason, admitted_by, claimed_at, attempt, admission_kind, turn_epoch, root_id, hop, created_at, updated_at, expires_at, binding_id, binding_revision, actor_user_id, wait_kind, ledger_id, verbatim_text, verbatim_display_text FROM deliveries"#,
        "DROP TABLE deliveries",
        "ALTER TABLE deliveries_v15 RENAME TO deliveries",
        "CREATE INDEX deliveries_target_state_seq ON deliveries (target_durable_id, state, seq)",
        "CREATE INDEX deliveries_root ON deliveries (root_id)",
        "CREATE INDEX deliveries_sender_turn ON deliveries (sender, sender_turn)",
        "CREATE UNIQUE INDEX deliveries_ledger ON deliveries (ledger_id) WHERE ledger_id IS NOT NULL",
        PROVENANCE_TRIGGER,
        PROVENANCE_OBJECT,
    ],
    // v16: a sender's collapse key, so a repeat replaces its earlier undelivered delivery to the same target
    // (omo#14428).
    &[
        "ALTER TABLE deliveries ADD COLUMN collapse_key TEXT",
        "CREATE INDEX deliveries_collapse ON deliveries (target_durable_id, sender, collapse_key) WHERE collapse_key IS NOT NULL",
    ],
];

pub const GATEWAY_TABLES: &[&str] = &[
    "deliveries",
    "receipts",
    "causal_roots",
    "causal_edges",
    "rate_buckets",
    "session_meta",
    "bindings",
    "outbox",
    "gateway_meta",
    "outbox_cursors",
    "completion_arms",
    "extension_schema",
    "extension_objects",
    "thread_creations",
    "extension_registrations",
    "toolkit_replay_receipts",
    "woken_sessions",
    "session_wait",
    "wake_counts",
    "intake_marks",
    "intake_commands",
];

/// Migration indexes (0-based) that rebuild a table to widen a CHECK, by the table they rebuild: the step runs with
/// foreign_keys OFF and refuses the bump when it left a foreign-key violation referencing that table.
pub const GATEWAY_TABLE_REBUILDS: &[(usize, &str)] = &[(6, "bindings"), (14, "deliveries")];

/// The table migration step `version` rebuilds, or none when the step only alters.
pub fn gateway_rebuild_table(version: i64) -> Option<&'static str> {
    usize::try_from(version)
        .ok()
        .and_then(|index| {
            GATEWAY_TABLE_REBUILDS
                .iter()
                .find(|(step, _)| *step == index)
        })
        .map(|(_, table)| *table)
}

/// Key under which a migrated store records the oldest schema whose code may keep using it (omo#14382).
pub const GATEWAY_READER_FLOOR_KEY: &str = "reader_floor";

/// Per migration (same index as `GATEWAY_MIGRATIONS`): the oldest gateway schema whose code may keep reading and
/// writing a store this step has reached. A step that only adds what older code never reads, or widens a CHECK to a
/// value older code passes through as data, keeps the floor; a step that changes what a column older code uses sets it
/// to its own version. Steps up to v14 predate the floor and claim nothing; v15's `start` lane and v16's nullable
/// `collapse_key` are both values a v14 reader passes through.
const GATEWAY_READER_FLOORS: [i64; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 14, 14];

/// The oldest schema a binary must know to use a store at `version`: the highest floor of the steps that reached it.
pub fn gateway_reader_floor(version: i64) -> i64 {
    let steps = usize::try_from(version)
        .unwrap_or(0)
        .min(GATEWAY_MIGRATIONS.len());
    GATEWAY_READER_FLOORS
        .iter()
        .take(steps)
        .fold(0, |floor, step| floor.max(*step))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_is_non_empty_and_the_count_is_the_supported_version() {
        assert_eq!(GATEWAY_MIGRATIONS.len(), 16);
        for (index, step) in GATEWAY_MIGRATIONS.iter().enumerate() {
            assert!(!step.is_empty(), "step {index} is empty");
        }
        assert_eq!(
            GATEWAY_TABLE_REBUILDS.to_vec(),
            vec![(6usize, "bindings"), (14usize, "deliveries")]
        );
    }

    #[test]
    fn every_step_has_a_reader_floor_and_the_v15_and_v16_rebuilds_keep_fourteen() {
        assert_eq!(GATEWAY_READER_FLOORS.len(), GATEWAY_MIGRATIONS.len());
        assert_eq!(gateway_reader_floor(0), 0);
        assert_eq!(gateway_reader_floor(13), 13);
        assert_eq!(gateway_reader_floor(14), 14);
        assert_eq!(gateway_reader_floor(15), 14);
        assert_eq!(gateway_reader_floor(16), 14);
    }

    #[test]
    fn the_rebuild_lookup_names_the_altered_steps_only() {
        assert_eq!(gateway_rebuild_table(6), Some("bindings"));
        assert_eq!(gateway_rebuild_table(14), Some("deliveries"));
        assert_eq!(gateway_rebuild_table(13), None);
        assert_eq!(gateway_rebuild_table(15), None);
        assert_eq!(gateway_rebuild_table(-1), None);
    }

    #[test]
    fn a_newer_store_is_refused_with_its_code() {
        let error = GatewaySchemaVersionError::new(17, 16, "Gateway");
        assert_eq!(error.code(), "gateway_schema_too_new");
        assert_eq!(
            error.to_string(),
            "Gateway schema 17 is newer than this binary's supported version 16."
        );
    }
}
