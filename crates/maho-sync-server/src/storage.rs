//! The server's storage: one SQLite file holding per-account rows in append order. A row is either a
//! keyring record (the account's trust chain: genesis, approval, rotation, recovery keys) or a batch
//! (an uploaded ciphertext object the devices sealed). Nothing here parses a payload: the digest,
//! epoch, link and signature travel through untouched, so the server can never read a memory.
//!
//! Besides the log it keeps what the client's own routes need: one row per device (so `GET
//! /v1/devices` can name a revoked one), the account and device sync switches, and the pairing rows.

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::DeviceToken;

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    /// The append started from a base the account's head has already moved past.
    Stale,
    /// A closed refusal code the client understands (`not_found`, `unknown_upload`, `digest_mismatch`).
    Refused(&'static str),
    NotFound,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(error) => error.fmt(f),
            Self::Stale => f.write_str("stale base"),
            Self::Refused(code) => write!(f, "refused: {code}"),
            Self::NotFound => f.write_str("not found"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

pub struct BatchAppend {
    pub upload_id: String,
    pub digest: Option<String>,
    pub base_seq: u64,
    pub idempotency_key: Option<String>,
    pub epoch: u64,
    pub link: String,
    pub record_sig: String,
}

pub struct KeyringAppend {
    pub record: Value,
    pub base_seq: u64,
    pub link: String,
    pub keyring_link: String,
    pub record_sig: String,
    pub wraps: Vec<Value>,
}

/// The served head of one account: batch rows carry no keyring_link or epoch, so reading
/// those two from the latest row overall would drop them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadRow {
    pub seq: u64,
    pub link: String,
    pub keyring_link: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub device: String,
    pub principal: String,
    pub revoked_at: Option<String>,
    pub sync_enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingRow {
    pub id: String,
    pub account: String,
    pub commitment: String,
    pub answer: Option<Value>,
    pub reveal: Option<Value>,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingField {
    Answer,
    Reveal,
}

#[derive(Clone)]
pub struct Store {
    connection: std::sync::Arc<std::sync::Mutex<Connection>>,
}

pub const PENDING_TTL_MS: u64 = 10 * 60 * 1000;

fn uint(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn sql_int(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The wire states a digest as unpadded base64url; the store keeps it as hex.
fn b64url_of_hex(text: &str) -> String {
    let bytes: Vec<u8> = text
        .as_bytes()
        .chunks_exact(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest_of(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The contract's batch id shape: 16 bytes as unpadded base64url (22 characters).
fn is_batch_id(value: &str) -> bool {
    value.len() == 22
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn upload_id_of(account: &str, size: usize) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    URL_SAFE_NO_PAD.encode(Sha256::digest(
        [
            account.as_bytes(),
            &nonce.to_be_bytes(),
            &nanos.to_be_bytes(),
            &size.to_be_bytes(),
        ]
        .concat(),
    ))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// RFC 8785-shaped serialization, so a keyring append's digest equals the one the device signed:
/// keys sorted by UTF-16 code units, minimal escapes, and integers only (the client refuses any
/// number that is not a safe integer before it signs).
pub fn canonical_json(value: &Value) -> Option<Vec<u8>> {
    let mut out = String::new();
    write_canonical(value, &mut out)?;
    Some(out.into_bytes())
}

fn write_canonical(value: &Value, out: &mut String) -> Option<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            let integer = number
                .as_u64()
                .map(|value| value.to_string())
                .or_else(|| number.as_i64().map(|value| value.to_string()))
                .or_else(|| {
                    number
                        .as_f64()
                        .filter(|v| v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0)
                        .map(|v| (v as i64).to_string())
                })?;
            out.push_str(&integer);
        }
        Value::String(text) => write_canonical_string(text, out),
        Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(value, out)?;
            }
            out.push(']');
        }
        Value::Object(values) => {
            let mut entries: Vec<(&String, &Value)> = values.iter().collect();
            entries.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
            out.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical_string(key, out);
                out.push(':');
                write_canonical(value, out)?;
            }
            out.push('}');
        }
    }
    Some(())
}

fn write_canonical_string(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            control if control < '\u{20}' => {
                let code = usize::from(u8::try_from(u32::from(control)).unwrap_or(0));
                out.push_str("\\u00");
                out.push(char::from(HEX[code / 16]));
                out.push(char::from(HEX[code % 16]));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

impl Store {
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        Self::prepare(connection)
    }

    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        Self::prepare(connection)
    }

    fn prepare(connection: Connection) -> Result<Self, StoreError> {
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS accounts (
                 account TEXT PRIMARY KEY,
                 rotation_pending INTEGER NOT NULL DEFAULT 0,
                 sync_enabled INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE IF NOT EXISTS entries (
                 account TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 base_seq INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 author_kind TEXT NOT NULL,
                 author_id TEXT NOT NULL,
                 author_sealing TEXT,
                 author_signing TEXT,
                 digest TEXT NOT NULL,
                 link TEXT NOT NULL,
                 keyring_link TEXT,
                 epoch INTEGER NOT NULL,
                 record_sig TEXT NOT NULL,
                 object_id TEXT,
                 batch_id TEXT,
                 record_json TEXT,
                 wraps_json TEXT,
                 PRIMARY KEY (account, seq)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS entries_idempotency
                 ON entries (account, batch_id) WHERE batch_id IS NOT NULL;
             CREATE TABLE IF NOT EXISTS objects (
                 account TEXT NOT NULL,
                 object_id TEXT NOT NULL,
                 digest TEXT NOT NULL,
                 bytes BLOB NOT NULL,
                 attached INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (account, object_id)
             );
             CREATE TABLE IF NOT EXISTS uploads (
                 account TEXT NOT NULL,
                 upload_id TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 digest TEXT,
                 PRIMARY KEY (account, upload_id)
             );
             CREATE TABLE IF NOT EXISTS devices (
                 device TEXT PRIMARY KEY,
                 account TEXT NOT NULL,
                 principal TEXT NOT NULL,
                 revoked_at TEXT,
                 sync_enabled INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE IF NOT EXISTS pending (
                 id TEXT PRIMARY KEY,
                 account TEXT NOT NULL,
                 commitment TEXT NOT NULL,
                 answer TEXT,
                 reveal TEXT,
                 expires_at INTEGER NOT NULL
             );",
        )?;
        Ok(Self {
            connection: std::sync::Arc::new(std::sync::Mutex::new(connection)),
        })
    }

    fn with<T>(
        &self,
        run: impl FnOnce(&mut Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| StoreError::Refused("poisoned"))?;
        run(&mut connection)
    }

    fn head(connection: &Connection, account: &str) -> Result<HeadRow, StoreError> {
        let (seq, link) = connection
            .query_row(
                "SELECT seq, link FROM entries WHERE account = ?1 ORDER BY seq DESC LIMIT 1",
                params![account],
                |row| Ok((uint(row.get::<_, i64>(0)?), row.get::<_, String>(1)?)),
            )
            .optional()?
            .unwrap_or((0, String::new()));
        let (keyring_link, epoch) = connection
            .query_row(
                "SELECT keyring_link, epoch FROM entries
                 WHERE account = ?1 AND kind = 'keyring' ORDER BY seq DESC LIMIT 1",
                params![account],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        uint(row.get::<_, i64>(1)?),
                    ))
                },
            )
            .optional()?
            .unwrap_or((String::new(), 0));
        Ok(HeadRow {
            seq,
            link,
            keyring_link,
            epoch,
        })
    }

    fn rotation_pending(connection: &Connection, account: &str) -> Result<bool, StoreError> {
        Ok(connection
            .query_row(
                "SELECT rotation_pending FROM accounts WHERE account = ?1",
                params![account],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|flag| flag != 0))
    }

    fn touch(connection: &Connection, account: &str) -> Result<(), StoreError> {
        connection.execute(
            "INSERT INTO accounts (account) VALUES (?1) ON CONFLICT(account) DO NOTHING",
            params![account],
        )?;
        Ok(())
    }

    pub fn head_of(&self, account: &str) -> Result<HeadRow, StoreError> {
        self.with(|connection| {
            Self::touch(connection, account)?;
            Self::head(connection, account)
        })
    }

    pub fn ensure_device(&self, device: &DeviceToken) -> Result<(), StoreError> {
        self.with(|connection| {
            Self::touch(connection, &device.account)?;
            connection.execute(
                "INSERT INTO devices (device, account, principal) VALUES (?1, ?2, ?3)
                 ON CONFLICT(device) DO UPDATE SET account = excluded.account,
                     principal = excluded.principal",
                params![device.device_id, device.account, device.sync_principal],
            )?;
            Ok(())
        })
    }

    /// Whether the account may accept a batch append now: a pending rotation freezes them all.
    pub fn rotation_clear(&self, account: &str) -> Result<bool, StoreError> {
        self.with(|connection| Ok(!Self::rotation_pending(connection, account)?))
    }

    /// Marks the account frozen until a rotation lands; an operator calls this when a device is revoked.
    pub fn set_rotation_pending(&self, account: &str, pending: bool) -> Result<(), StoreError> {
        self.with(|connection| {
            Self::touch(connection, account)?;
            connection.execute(
                "UPDATE accounts SET rotation_pending = ?2 WHERE account = ?1",
                params![account, i64::from(pending)],
            )?;
            Ok(())
        })
    }

    /// Records a device as revoked: the account freezes batch appends until a rotation excludes it.
    pub fn revoke_device(&self, account: &str) -> Result<(), StoreError> {
        self.set_rotation_pending(account, true)
    }

    pub fn revoke(&self, device: &str) -> Result<Option<String>, StoreError> {
        self.with(|connection| {
            let Some(account) = connection
                .query_row(
                    "SELECT account FROM devices WHERE device = ?1",
                    params![device],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(None);
            };
            let changed = connection.execute(
                "UPDATE devices SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                 WHERE device = ?1 AND revoked_at IS NULL",
                params![device],
            )?;
            if changed > 0 {
                Self::touch(connection, &account)?;
                connection.execute(
                    "UPDATE accounts SET rotation_pending = 1 WHERE account = ?1",
                    params![account],
                )?;
            }
            Ok(Some(account))
        })
    }

    pub fn devices(&self, account: &str) -> Result<Vec<DeviceRow>, StoreError> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT device, principal, revoked_at, sync_enabled FROM devices
                 WHERE account = ?1 ORDER BY device",
            )?;
            let rows = statement
                .query_map(params![account], |row: &Row<'_>| {
                    Ok(DeviceRow {
                        device: row.get(0)?,
                        principal: row.get(1)?,
                        revoked_at: row.get(2)?,
                        sync_enabled: row.get::<_, i64>(3)? != 0,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn is_revoked(&self, device: &str) -> Result<bool, StoreError> {
        self.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT revoked_at IS NOT NULL FROM devices WHERE device = ?1",
                    params![device],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false))
        })
    }

    pub fn set_account_sync(&self, account: &str, enabled: bool) -> Result<(), StoreError> {
        self.with(|connection| {
            Self::touch(connection, account)?;
            connection.execute(
                "UPDATE accounts SET sync_enabled = ?2 WHERE account = ?1",
                params![account, i64::from(enabled)],
            )?;
            Ok(())
        })
    }

    pub fn set_device_sync(
        &self,
        account: &str,
        device: &str,
        enabled: bool,
    ) -> Result<bool, StoreError> {
        self.with(|connection| {
            Ok(connection.execute(
                "UPDATE devices SET sync_enabled = ?3 WHERE device = ?1 AND account = ?2",
                params![device, account, i64::from(enabled)],
            )? > 0)
        })
    }

    pub fn open_pending(&self, account: &str, commitment: &str) -> Result<PendingRow, StoreError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = now_ms();
        let id = URL_SAFE_NO_PAD.encode(Sha256::digest(
            [
                account.as_bytes(),
                commitment.as_bytes(),
                &now.to_be_bytes(),
                &nonce.to_be_bytes(),
            ]
            .concat(),
        ));
        let row = PendingRow {
            id,
            account: account.to_string(),
            commitment: commitment.to_string(),
            answer: None,
            reveal: None,
            expires_at: now + PENDING_TTL_MS,
        };
        self.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO pending (id, account, commitment, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![row.id, row.account, row.commitment, sql_int(row.expires_at)],
            )?;
            Ok(())
        })?;
        Ok(row)
    }

    fn read_pending(row: &Row<'_>) -> rusqlite::Result<PendingRow> {
        let json_column = |value: Option<String>| {
            value.and_then(|text| serde_json::from_str::<Value>(&text).ok())
        };
        Ok(PendingRow {
            id: row.get("id")?,
            account: row.get("account")?,
            commitment: row.get("commitment")?,
            answer: json_column(row.get("answer")?),
            reveal: json_column(row.get("reveal")?),
            expires_at: uint(row.get("expires_at")?),
        })
    }

    pub fn pending(&self, account: &str, id: &str) -> Result<Option<PendingRow>, StoreError> {
        let now = sql_int(now_ms());
        self.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT * FROM pending WHERE id = ?1 AND account = ?2 AND expires_at > ?3",
                    params![id, account, now],
                    Self::read_pending,
                )
                .optional()?)
        })
    }

    pub fn list_pending(&self, account: &str) -> Result<Vec<PendingRow>, StoreError> {
        let now = sql_int(now_ms());
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT * FROM pending WHERE account = ?1 AND expires_at > ?2 ORDER BY id",
            )?;
            let rows = statement
                .query_map(params![account, now], Self::read_pending)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn set_pending_field(
        &self,
        account: &str,
        id: &str,
        field: PendingField,
        value: &Value,
    ) -> Result<bool, StoreError> {
        let now = sql_int(now_ms());
        let sql = match field {
            PendingField::Answer => {
                "UPDATE pending SET answer = ?3
                 WHERE id = ?1 AND account = ?2 AND answer IS NULL AND expires_at > ?4"
            }
            PendingField::Reveal => {
                "UPDATE pending SET reveal = ?3
                 WHERE id = ?1 AND account = ?2 AND reveal IS NULL AND answer IS NOT NULL
                     AND expires_at > ?4"
            }
        };
        self.with(|connection| {
            Ok(connection.execute(sql, params![id, account, value.to_string(), now])? > 0)
        })
    }

    pub fn summary(&self, account: &str) -> Result<Value, StoreError> {
        self.with(|connection| {
            Self::touch(connection, account)?;
            let head = Self::head(connection, account)?;
            let pending = Self::rotation_pending(connection, account)?;
            let oldest = connection
                .query_row(
                    "SELECT MIN(seq) FROM entries WHERE account = ?1",
                    params![account],
                    |row| Ok(row.get::<_, Option<i64>>(0)?.map(uint)),
                )
                .optional()?
                .flatten()
                .unwrap_or(1);
            Ok(json!({
                "head": {
                    "seq": head.seq,
                    "link": head.link,
                    "keyring_link": head.keyring_link,
                },
                "current_epoch": head.epoch,
                "rotation_pending": pending,
                "oldest_retained_seq": oldest,
                "snapshot": Value::Null,
            }))
        })
    }

    pub fn rows_after(
        &self,
        account: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<Value>, StoreError> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT seq, kind, author_kind, author_id, digest, object_id, batch_id
                 FROM entries WHERE account = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
            )?;
            let rows = statement
                .query_map(
                    params![account, sql_int(after), i64::try_from(limit).unwrap_or(500)],
                    |row| {
                        let batch_id: Option<String> = row.get(6)?;
                        Ok(json!({
                            "seq": uint(row.get::<_, i64>(0)?),
                            "kind": row.get::<_, String>(1)?,
                            "author": {
                                "kind": row.get::<_, String>(2)?,
                                "id": row.get::<_, String>(3)?,
                            },
                            "digest": b64url_of_hex(&row.get::<_, String>(4)?),
                            "object_id": row.get::<_, Option<String>>(5)?,
                            "batch_id": batch_id,
                        }))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn chain_rows_after(
        &self,
        account: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<Value>, StoreError> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT seq, base_seq, kind, account, author_kind, author_id, author_sealing,
                        author_signing, digest, link, keyring_link, epoch, record_sig, batch_id,
                        record_json, wraps_json
                 FROM entries WHERE account = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
            )?;
            let rows = statement
                .query_map(
                    params![account, sql_int(after), i64::try_from(limit).unwrap_or(500)],
                    Self::chain_row_json,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    fn chain_row_json(row: &Row<'_>) -> rusqlite::Result<Value> {
        let kind: String = row.get(2)?;
        let author_id: String = row.get(5)?;
        let sealing = row
            .get::<_, Option<String>>(6)?
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| author_id.clone());
        let signing = row
            .get::<_, Option<String>>(7)?
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| author_id.clone());
        let mut value = json!({
            "seq": uint(row.get::<_, i64>(0)?),
            "base_seq": uint(row.get::<_, i64>(1)?),
            "kind": kind,
            "account": row.get::<_, String>(3)?,
            "author": {
                "kind": row.get::<_, String>(4)?,
                "id": author_id,
                "sealing_public_key": sealing,
                "signing_public_key": signing,
            },
            "digest": b64url_of_hex(&row.get::<_, String>(8)?),
            "link": row.get::<_, String>(9)?,
            "keyring_link": row.get::<_, Option<String>>(10)?,
            "epoch": uint(row.get::<_, i64>(11)?),
            "record_sig": row.get::<_, String>(12)?,
        });
        if kind == "batch" {
            let batch_id: Option<String> = row.get(13)?;
            if let Some(id) = batch_id.filter(|id| is_batch_id(id))
                && let Some(object) = value.as_object_mut()
            {
                object.insert("batch_id".into(), Value::String(id));
            }
        } else {
            let record = row
                .get::<_, Option<String>>(14)?
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .unwrap_or(Value::Null);
            let wraps = row
                .get::<_, Option<String>>(15)?
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .unwrap_or_else(|| json!([]));
            if let Some(object) = value.as_object_mut() {
                object.insert("body".into(), record);
                object.insert("wraps".into(), wraps);
            }
        }
        Ok(value)
    }

    pub fn keyring_rows(&self, account: &str) -> Result<Vec<Value>, StoreError> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT seq, base_seq, kind, account, author_kind, author_id, author_sealing,
                        author_signing, digest, link, keyring_link, epoch, record_sig, batch_id,
                        record_json, wraps_json
                 FROM entries WHERE account = ?1 AND kind = 'keyring' ORDER BY seq ASC",
            )?;
            let rows = statement
                .query_map(params![account], Self::chain_row_json)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn recovery_wrap_rows(&self, account: &str) -> Result<Vec<Value>, StoreError> {
        let rows = self.keyring_rows(account)?;
        Ok(rows
            .into_iter()
            .flat_map(|row| {
                let record = json!({
                    "seq": row["seq"],
                    "digest": row["digest"],
                    "keyring_link": row["keyring_link"],
                });
                row["wraps"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |entry| json!({ "record": record, "entry": entry }))
            })
            .collect())
    }

    pub fn object(&self, account: &str, object_id: &str) -> Result<Vec<u8>, StoreError> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT bytes FROM objects WHERE account = ?1 AND object_id = ?2",
                    params![account, object_id],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?
                .ok_or(StoreError::NotFound)
        })
    }

    /// Reserves an object id for `size` bytes; an id the caller chose keeps its reservation.
    pub fn reserve(
        &self,
        account: &str,
        size: usize,
        digest: Option<String>,
    ) -> Result<String, StoreError> {
        self.with(|connection| {
            Self::touch(connection, account)?;
            let upload_id = upload_id_of(account, size);
            connection.execute(
                "INSERT INTO uploads (account, upload_id, size, digest) VALUES (?1, ?2, ?3, ?4)",
                params![account, upload_id, i64::try_from(size).unwrap_or(0), digest],
            )?;
            Ok(upload_id)
        })
    }

    pub fn upload(
        &self,
        account: &str,
        upload_id: &str,
        bytes: &[u8],
    ) -> Result<String, StoreError> {
        self.with(|connection| {
            let (expected_size, reserved) = connection
                .query_row(
                    "SELECT size, digest FROM uploads WHERE account = ?1 AND upload_id = ?2",
                    params![account, upload_id],
                    |row| {
                        Ok((
                            uint(row.get::<_, i64>(0)?),
                            row.get::<_, Option<String>>(1)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(StoreError::Refused("unknown_upload"))?;
            let digest = digest_of(bytes);
            if expected_size != bytes.len() as u64
                || reserved.as_deref().is_some_and(|want| want != digest)
            {
                return Err(StoreError::Refused("digest_mismatch"));
            }
            connection.execute(
                "INSERT INTO objects (account, object_id, digest, bytes) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(account, object_id) DO UPDATE SET digest = ?3, bytes = ?4",
                params![account, upload_id, digest, bytes],
            )?;
            Ok(digest)
        })
    }

    pub fn append_batch(
        &self,
        device: &DeviceToken,
        append: BatchAppend,
    ) -> Result<u64, StoreError> {
        let account = device.account.as_str();
        self.with(|connection| {
            Self::touch(connection, account)?;
            // A repost answers its original seq before the base or freeze check: a push resumed
            // after a crash must not duplicate, and a frozen account must not turn it into a refusal.
            if let Some(key) = &append.idempotency_key
                && let Some(existing) = connection
                    .query_row(
                        "SELECT seq FROM entries WHERE account = ?1 AND batch_id = ?2",
                        params![account, key],
                        |row| Ok(uint(row.get::<_, i64>(0)?)),
                    )
                    .optional()?
            {
                return Ok(existing);
            }
            if Self::rotation_pending(connection, account)? {
                return Err(StoreError::Stale);
            }
            let head = Self::head(connection, account)?.seq;
            if append.base_seq != head {
                return Err(StoreError::Stale);
            }
            let digest = connection
                .query_row(
                    "SELECT digest FROM objects WHERE account = ?1 AND object_id = ?2",
                    params![account, append.upload_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or(StoreError::Refused("unknown_upload"))?;
            if append.digest.as_deref().is_some_and(|want| want != digest) {
                return Err(StoreError::Refused("object_mismatch"));
            }
            connection.execute(
                "INSERT INTO entries (account, seq, base_seq, kind, author_kind, author_id,
                     author_sealing, author_signing, digest, link, keyring_link, epoch, record_sig,
                     object_id, batch_id, record_json, wraps_json)
                 VALUES (?1, ?2, ?3, 'batch', 'device', ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11,
                     ?12, NULL, NULL)",
                params![
                    account,
                    sql_int(head + 1),
                    sql_int(append.base_seq),
                    device.device_id,
                    device.sealing_public_key,
                    device.signing_public_key,
                    digest,
                    append.link,
                    sql_int(append.epoch),
                    append.record_sig,
                    append.upload_id,
                    append.idempotency_key,
                ],
            )?;
            connection.execute(
                "UPDATE objects SET attached = 1 WHERE account = ?1 AND object_id = ?2",
                params![account, append.upload_id],
            )?;
            Ok(head + 1)
        })
    }

    pub fn append_keyring(
        &self,
        device: &DeviceToken,
        append: KeyringAppend,
    ) -> Result<u64, StoreError> {
        let account = device.account.as_str();
        self.with(|connection| {
            Self::touch(connection, account)?;
            let head = Self::head(connection, account)?.seq;
            if append.base_seq != head {
                return Err(StoreError::Stale);
            }
            // The digest is the canonical hash of the record, which is exactly what the device
            // signed: the chain links records by that digest, so a different serialization would
            // make every keyring row fail verification on the client.
            let digest = digest_of(
                &canonical_json(&append.record).ok_or(StoreError::Refused("malformed_record"))?,
            );
            let epoch = append.record["epoch"].as_u64().unwrap_or(1);
            let keyring_link = if append.keyring_link.is_empty() {
                None
            } else {
                Some(append.keyring_link.clone())
            };
            connection.execute(
                "INSERT INTO entries (account, seq, base_seq, kind, author_kind, author_id,
                     author_sealing, author_signing, digest, link, keyring_link, epoch, record_sig,
                     object_id, batch_id, record_json, wraps_json)
                 VALUES (?1, ?2, ?3, 'keyring', 'device', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL,
                     NULL, ?12, ?13)",
                params![
                    account,
                    sql_int(head + 1),
                    sql_int(append.base_seq),
                    device.device_id,
                    device.sealing_public_key,
                    device.signing_public_key,
                    digest,
                    append.link,
                    keyring_link,
                    sql_int(epoch),
                    append.record_sig,
                    serde_json::to_string(&append.record).unwrap_or_else(|_| "null".into()),
                    serde_json::to_string(&append.wraps).unwrap_or_else(|_| "[]".into()),
                ],
            )?;
            // A landed keyring record is what ends a freeze: a rotation excludes the revoked devices.
            connection.execute(
                "UPDATE accounts SET rotation_pending = 0 WHERE account = ?1",
                params![account],
            )?;
            Ok(head + 1)
        })
    }
}

/// Single-use confirmation tokens per `(account, target)` for device sync disablement.
#[derive(Debug, Default)]
pub struct Confirmations {
    tokens: std::sync::Mutex<BTreeMap<(String, String), String>>,
}

impl Confirmations {
    pub fn issue(&self, account: &str, target: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
        let token = URL_SAFE_NO_PAD.encode(Sha256::digest(
            [
                account.as_bytes(),
                target.as_bytes(),
                &now_ms().to_be_bytes(),
                &nonce.to_be_bytes(),
            ]
            .concat(),
        ));
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert((account.to_string(), target.to_string()), token.clone());
        }
        token
    }

    pub fn spend(&self, account: &str, target: &str, token: &str) -> bool {
        let Ok(mut tokens) = self.tokens.lock() else {
            return false;
        };
        match tokens.get(&(account.to_string(), target.to_string())) {
            Some(issued) if issued == token => {
                tokens.remove(&(account.to_string(), target.to_string()));
                true
            }
            _ => false,
        }
    }
}
