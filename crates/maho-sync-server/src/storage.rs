//! The server's storage: one SQLite file holding per-account rows in append order. A row is either a
//! keyring record (the account's trust chain: genesis, approval, rotation, recovery keys) or a batch
//! (an uploaded ciphertext object the devices sealed). Nothing here parses a payload: the digest,
//! epoch, link and signature travel through untouched, so the server can never read a memory.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, OptionalExtension, params};
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

#[derive(Clone)]
pub struct Store {
    connection: std::sync::Arc<std::sync::Mutex<Connection>>,
}

/// The account's newest row: sequence, link, keyring link and epoch.
type HeadRow = (u64, String, Option<String>, u64);

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
                 rotation_pending INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS entries (
                 account TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 author_kind TEXT NOT NULL,
                 author_id TEXT NOT NULL,
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

    fn head(connection: &Connection, account: &str) -> Result<Option<HeadRow>, StoreError> {
        let row = connection
            .query_row(
                "SELECT seq, link, keyring_link, epoch FROM entries WHERE account = ?1 ORDER BY seq DESC LIMIT 1",
                params![account],
                |row| {
                    Ok((
                        uint(row.get::<_, i64>(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        uint(row.get::<_, i64>(3)?),
                    ))
                },
            )
            .optional()?;
        Ok(row)
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
            let (seq, link, keyring_link, epoch) = head.unwrap_or((0, String::new(), None, 0));
            Ok(json!({
                "head": {
                    "seq": seq,
                    "link": link,
                    "keyring_link": keyring_link.unwrap_or_default(),
                },
                "current_epoch": epoch,
                "rotation_pending": pending,
                "oldest_retained_seq": oldest,
                "snapshot": Value::Null,
            }))
        })
    }

    /// Rows after `after`, in sequence order; `full` also carries each keyring row's body and wraps.
    pub fn rows_after(
        &self,
        account: &str,
        after: u64,
        limit: usize,
        full: bool,
    ) -> Result<Vec<Value>, StoreError> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT seq, kind, author_kind, author_id, digest, link, keyring_link, epoch, record_sig, object_id, batch_id, record_json, wraps_json
                 FROM entries WHERE account = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
            )?;
            let rows = statement
                .query_map(params![account, sql_int(after), i64::try_from(limit).unwrap_or(500)], |row| {
                    Self::row_json(row, full)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    fn row_json(row: &rusqlite::Row<'_>, full: bool) -> rusqlite::Result<Value> {
        let kind: String = row.get(1)?;
        let mut value = json!({
            "seq": uint(row.get::<_, i64>(0)?),
            "kind": kind,
            "author": { "kind": row.get::<_, String>(2)?, "id": row.get::<_, String>(3)? },
            "digest": b64url_of_hex(&row.get::<_, String>(4)?),
            "link": row.get::<_, String>(5)?,
            "keyring_link": row.get::<_, Option<String>>(6)?,
            "epoch": uint(row.get::<_, i64>(7)?),
            "record_sig": row.get::<_, String>(8)?,
        });
        let object_id = row.get::<_, Option<String>>(9)?;
        let batch_id = row.get::<_, Option<String>>(10)?;
        if let Some(object) = value.as_object_mut() {
            if kind == "batch" {
                object.insert(
                    "object_id".into(),
                    object_id.map_or(Value::Null, Value::String),
                );
                object.insert(
                    "batch_id".into(),
                    batch_id.map_or(Value::Null, Value::String),
                );
            } else if full {
                object.insert(
                    "body".into(),
                    row.get::<_, Option<String>>(11)?
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or(Value::Null),
                );
                object.insert(
                    "wraps".into(),
                    row.get::<_, Option<String>>(12)?
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_else(|| json!([])),
                );
            }
        }
        Ok(value)
    }

    pub fn keyring_rows(&self, account: &str) -> Result<Vec<Value>, StoreError> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT seq, kind, author_kind, author_id, digest, link, keyring_link, epoch, record_sig, object_id, batch_id, record_json, wraps_json
                 FROM entries WHERE account = ?1 AND kind = 'keyring' ORDER BY seq ASC",
            )?;
            let rows = statement
                .query_map(params![account], |row| Self::row_json(row, true))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn recovery_wrap_rows(&self, account: &str) -> Result<Vec<Value>, StoreError> {
        let rows = self.keyring_rows(account)?;
        Ok(rows
            .into_iter()
            .filter(|row| row["kind"] == "keyring")
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
            let reserved = connection
                .query_row(
                    "SELECT digest FROM uploads WHERE account = ?1 AND upload_id = ?2",
                    params![account, upload_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or(StoreError::Refused("unknown_upload"))?;
            let digest = digest_of(bytes);
            if reserved.as_deref().is_some_and(|want| want != digest) {
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
            if Self::rotation_pending(connection, account)? {
                return Err(StoreError::Stale);
            }
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
            let head = Self::head(connection, account)?.map_or(0, |(seq, ..)| seq);
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
            connection.execute(
                "INSERT INTO entries (account, seq, kind, author_kind, author_id, digest, link, keyring_link, epoch, record_sig, object_id, batch_id, record_json, wraps_json)
                 VALUES (?1, ?2, 'batch', 'device', ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9, NULL, NULL)",
                params![
                    account,
                    sql_int(head + 1),
                    device.device_id,
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
            let head = Self::head(connection, account)?.map_or(0, |(seq, ..)| seq);
            if append.base_seq != head {
                return Err(StoreError::Stale);
            }
            let digest = digest_of(
                &serde_json::to_vec(&append.record)
                    .map_err(|_| StoreError::Refused("malformed_record"))?,
            );
            let epoch = append.record["epoch"].as_u64().unwrap_or(1);
            let keyring_link = if append.keyring_link.is_empty() {
                None
            } else {
                Some(append.keyring_link.clone())
            };
            connection.execute(
                "INSERT INTO entries (account, seq, kind, author_kind, author_id, digest, link, keyring_link, epoch, record_sig, object_id, batch_id, record_json, wraps_json)
                 VALUES (?1, ?2, 'keyring', 'device', ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, ?9, ?10)",
                params![
                    account,
                    sql_int(head + 1),
                    device.device_id,
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
