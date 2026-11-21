//! SQLite storage: per-account sequence, uploads/objects, devices and pairing rows.
//!
//! One connection behind one mutex: every append runs inside it, so an account's sequence is
//! assigned strictly in order and the base check and the insert can never interleave with another
//! append.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use crate::wire::{EntryKind, EntryRow};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
    account TEXT NOT NULL,
    seq INTEGER NOT NULL,
    base_seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    author TEXT NOT NULL,
    digest TEXT NOT NULL,
    link TEXT NOT NULL,
    keyring_link TEXT,
    record_sig TEXT NOT NULL,
    object_id TEXT,
    batch_id TEXT,
    body TEXT,
    wraps TEXT,
    PRIMARY KEY (account, seq)
);
CREATE UNIQUE INDEX IF NOT EXISTS entries_batch_id ON entries (account, batch_id)
    WHERE batch_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS uploads (
    id TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    bytes BLOB
);
CREATE TABLE IF NOT EXISTS accounts (
    account TEXT PRIMARY KEY,
    rotation_pending INTEGER NOT NULL DEFAULT 0,
    sync_enabled INTEGER NOT NULL DEFAULT 1
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
);
";

/// Milliseconds a pairing row stays open.
pub const PENDING_TTL_MS: u64 = 10 * 60 * 1000;

#[derive(Debug)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// The served head of one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadState {
    pub seq: u64,
    pub link: String,
    pub keyring_link: String,
    pub current_epoch: u64,
    pub rotation_pending: bool,
}

/// What an append request carries besides its kind-specific fields.
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub base_seq: u64,
    pub kind: EntryKind,
    pub epoch: u64,
    pub author: Value,
    pub digest: String,
    pub link: String,
    pub keyring_link: Option<String>,
    pub record_sig: String,
    pub object_id: Option<String>,
    pub batch_id: Option<String>,
    pub body: Option<Value>,
    pub wraps: Option<Value>,
}

/// Why an append did not land.
#[derive(Debug, PartialEq, Eq)]
pub enum AppendRefusal {
    StaleBase,
}

/// One upload reservation.
#[derive(Debug, Clone)]
pub struct Upload {
    pub id: String,
    pub size: u64,
    pub sha256: String,
    pub filled: bool,
}

/// A device row as the account's device list shows it.
#[derive(Debug, Clone)]
pub struct DeviceRow {
    pub device: String,
    pub principal: String,
    pub revoked_at: Option<String>,
    pub sync_enabled: bool,
}

/// A pairing row.
#[derive(Debug, Clone)]
pub struct PendingRow {
    pub id: String,
    pub account: String,
    pub commitment: String,
    pub answer: Option<Value>,
    pub reveal: Option<Value>,
    pub expires_at: u64,
}

pub struct Store {
    db: Mutex<Connection>,
}

fn int(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn uint(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn json_column(text: Option<String>) -> Option<Value> {
    text.and_then(|text| serde_json::from_str(&text).ok())
}

fn read_entry(row: &Row<'_>) -> rusqlite::Result<EntryRow> {
    let kind: String = row.get("kind")?;
    let author: String = row.get("author")?;
    Ok(EntryRow {
        seq: uint(row.get("seq")?),
        base_seq: uint(row.get("base_seq")?),
        kind: EntryKind::parse(&kind).unwrap_or(EntryKind::Batch),
        account: row.get("account")?,
        epoch: uint(row.get("epoch")?),
        author: serde_json::from_str(&author).unwrap_or(Value::Null),
        digest: row.get("digest")?,
        link: row.get("link")?,
        keyring_link: row.get("keyring_link")?,
        record_sig: row.get("record_sig")?,
        object_id: row.get("object_id")?,
        batch_id: row.get("batch_id")?,
        body: json_column(row.get("body")?),
        wraps: json_column(row.get("wraps")?),
    })
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// RFC 3339 UTC timestamp (second precision) for a millisecond instant.
pub fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Civil-from-days (H. Hinnant): proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

impl Store {
    pub fn open(path: &Path) -> StoreResult<Self> {
        let db = if crate::is_memory_db(path) {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn with<T>(&self, f: impl FnOnce(&mut Connection) -> StoreResult<T>) -> StoreResult<T> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| StoreError("store lock poisoned".into()))?;
        f(&mut db)
    }

    /// Registers a device from the token file; an existing row keeps its revocation.
    pub fn ensure_device(&self, account: &str, device: &str, principal: &str) -> StoreResult<()> {
        self.with(|db| {
            db.execute(
                "INSERT INTO devices (device, account, principal) VALUES (?1, ?2, ?3)
                 ON CONFLICT(device) DO UPDATE SET account = excluded.account,
                     principal = excluded.principal",
                params![device, account, principal],
            )?;
            db.execute(
                "INSERT OR IGNORE INTO accounts (account) VALUES (?1)",
                params![account],
            )?;
            Ok(())
        })
    }

    fn head_of(db: &Connection, account: &str) -> StoreResult<HeadState> {
        let (seq, link) = db
            .query_row(
                "SELECT seq, link FROM entries WHERE account = ?1 ORDER BY seq DESC LIMIT 1",
                params![account],
                |row| Ok((uint(row.get(0)?), row.get::<_, String>(1)?)),
            )
            .optional()?
            .unwrap_or((0, String::new()));
        let (keyring_link, current_epoch) = db
            .query_row(
                "SELECT keyring_link, epoch FROM entries
                 WHERE account = ?1 AND kind = 'keyring' ORDER BY seq DESC LIMIT 1",
                params![account],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        uint(row.get(1)?),
                    ))
                },
            )
            .optional()?
            .unwrap_or((String::new(), 0));
        let rotation_pending = db
            .query_row(
                "SELECT rotation_pending FROM accounts WHERE account = ?1",
                params![account],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|flag| flag != 0);
        Ok(HeadState {
            seq,
            link,
            keyring_link,
            current_epoch,
            rotation_pending,
        })
    }

    pub fn head(&self, account: &str) -> StoreResult<HeadState> {
        self.with(|db| Self::head_of(db, account))
    }

    /// The seq an earlier append under `batch_id` landed at, if any.
    pub fn seq_of_batch(&self, account: &str, batch_id: &str) -> StoreResult<Option<u64>> {
        self.with(|db| {
            Ok(db
                .query_row(
                    "SELECT seq FROM entries WHERE account = ?1 AND batch_id = ?2",
                    params![account, batch_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(uint))
        })
    }

    /// Appends at `head + 1`. A repost of a landed `batch_id` answers its original seq and is
    /// checked first; then a pending rotation refuses batch rows (a keyring row lands and clears
    /// it); then `base_seq` must equal the head.
    pub fn append(
        &self,
        account: &str,
        entry: NewEntry,
    ) -> StoreResult<Result<(u64, bool), AppendRefusal>> {
        self.with(|db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if let Some(batch_id) = &entry.batch_id {
                let landed = tx
                    .query_row(
                        "SELECT seq FROM entries WHERE account = ?1 AND batch_id = ?2",
                        params![account, batch_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                if let Some(seq) = landed {
                    return Ok(Ok((uint(seq), false)));
                }
            }
            let head = Self::head_of(&tx, account)?;
            if (entry.kind == EntryKind::Batch && head.rotation_pending)
                || entry.base_seq != head.seq
            {
                return Ok(Err(AppendRefusal::StaleBase));
            }
            let seq = head.seq + 1;
            tx.execute(
                "INSERT INTO entries (account, seq, base_seq, kind, epoch, author, digest, link,
                     keyring_link, record_sig, object_id, batch_id, body, wraps)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    account,
                    int(seq),
                    int(entry.base_seq),
                    entry.kind.as_str(),
                    int(entry.epoch),
                    entry.author.to_string(),
                    entry.digest,
                    entry.link,
                    entry.keyring_link,
                    entry.record_sig,
                    entry.object_id,
                    entry.batch_id,
                    entry.body.map(|body| body.to_string()),
                    entry.wraps.map(|wraps| wraps.to_string()),
                ],
            )?;
            if entry.kind == EntryKind::Keyring {
                tx.execute(
                    "INSERT INTO accounts (account, rotation_pending) VALUES (?1, 0)
                     ON CONFLICT(account) DO UPDATE SET rotation_pending = 0",
                    params![account],
                )?;
            }
            tx.commit()?;
            Ok(Ok((seq, true)))
        })
    }

    /// Entries with `seq > after`, in order, at most `limit`.
    pub fn entries_after(
        &self,
        account: &str,
        after: u64,
        limit: usize,
        kind: Option<EntryKind>,
    ) -> StoreResult<Vec<EntryRow>> {
        self.with(|db| {
            let mut statement = db.prepare(
                "SELECT * FROM entries WHERE account = ?1 AND seq > ?2
                     AND (?3 IS NULL OR kind = ?3)
                 ORDER BY seq LIMIT ?4",
            )?;
            let rows = statement
                .query_map(
                    params![
                        account,
                        int(after),
                        kind.map(EntryKind::as_str),
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    read_entry,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn reserve(&self, account: &str, size: u64, sha256: &str) -> StoreResult<String> {
        self.with(|db| {
            let seed = format!("upload\0{account}\0{sha256}\0{size}\0{}", now_ms());
            let mut id: String = crate::canonical::digest_of(seed.as_bytes())
                .chars()
                .take(22)
                .collect();
            // Two reservations in the same millisecond with the same digest would collide; extend
            // the id with a counter until it is free.
            let mut salt = 0u32;
            while db
                .query_row("SELECT 1 FROM uploads WHERE id = ?1", params![id], |_| {
                    Ok(())
                })
                .optional()?
                .is_some()
            {
                salt += 1;
                id = crate::canonical::digest_of(format!("{seed}\0{salt}").as_bytes())
                    .chars()
                    .take(22)
                    .collect();
            }
            db.execute(
                "INSERT INTO uploads (id, account, size, sha256) VALUES (?1, ?2, ?3, ?4)",
                params![id, account, int(size), sha256],
            )?;
            Ok(id)
        })
    }

    pub fn upload(&self, account: &str, id: &str) -> StoreResult<Option<Upload>> {
        self.with(|db| {
            Ok(db
                .query_row(
                    "SELECT id, size, sha256, bytes IS NOT NULL FROM uploads
                     WHERE id = ?1 AND account = ?2",
                    params![id, account],
                    |row| {
                        Ok(Upload {
                            id: row.get(0)?,
                            size: uint(row.get(1)?),
                            sha256: row.get(2)?,
                            filled: row.get(3)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    pub fn fill(&self, account: &str, id: &str, bytes: &[u8]) -> StoreResult<()> {
        self.with(|db| {
            db.execute(
                "UPDATE uploads SET bytes = ?3 WHERE id = ?1 AND account = ?2",
                params![id, account, bytes],
            )?;
            Ok(())
        })
    }

    /// An object's bytes, only for its own account.
    pub fn object(&self, account: &str, id: &str) -> StoreResult<Option<Vec<u8>>> {
        self.with(|db| {
            Ok(db
                .query_row(
                    "SELECT bytes FROM uploads WHERE id = ?1 AND account = ?2 AND bytes IS NOT NULL",
                    params![id, account],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?)
        })
    }

    pub fn devices(&self, account: &str) -> StoreResult<Vec<DeviceRow>> {
        self.with(|db| {
            let mut statement = db.prepare(
                "SELECT device, principal, revoked_at, sync_enabled FROM devices
                 WHERE account = ?1 ORDER BY device",
            )?;
            let rows = statement
                .query_map(params![account], |row| {
                    Ok(DeviceRow {
                        device: row.get(0)?,
                        principal: row.get(1)?,
                        revoked_at: row.get(2)?,
                        sync_enabled: row.get::<_, i64>(3)? != 0,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn is_revoked(&self, device: &str) -> StoreResult<bool> {
        self.with(|db| {
            Ok(db
                .query_row(
                    "SELECT revoked_at IS NOT NULL FROM devices WHERE device = ?1",
                    params![device],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false))
        })
    }

    /// Marks `device` revoked and sets its account's `rotation_pending`: until a keyring append
    /// lands, batch appends answer `stale_base`. Returns the account, or `None` for an unknown
    /// device. Revoking twice keeps the first timestamp.
    pub fn revoke(&self, device: &str) -> StoreResult<Option<String>> {
        self.with(|db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let Some(account) = tx
                .query_row(
                    "SELECT account FROM devices WHERE device = ?1",
                    params![device],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(None);
            };
            let changed = tx.execute(
                "UPDATE devices SET revoked_at = ?2 WHERE device = ?1 AND revoked_at IS NULL",
                params![device, rfc3339(now_ms())],
            )?;
            if changed > 0 {
                tx.execute(
                    "INSERT INTO accounts (account, rotation_pending) VALUES (?1, 1)
                     ON CONFLICT(account) DO UPDATE SET rotation_pending = 1",
                    params![account],
                )?;
            }
            tx.commit()?;
            Ok(Some(account))
        })
    }

    pub fn set_account_sync(&self, account: &str, enabled: bool) -> StoreResult<()> {
        self.with(|db| {
            db.execute(
                "INSERT INTO accounts (account, sync_enabled) VALUES (?1, ?2)
                 ON CONFLICT(account) DO UPDATE SET sync_enabled = excluded.sync_enabled",
                params![account, enabled],
            )?;
            Ok(())
        })
    }

    /// Turns sync on or off for one device of `account`; false when the device is not there.
    pub fn set_device_sync(&self, account: &str, device: &str, enabled: bool) -> StoreResult<bool> {
        self.with(|db| {
            Ok(db.execute(
                "UPDATE devices SET sync_enabled = ?3 WHERE device = ?1 AND account = ?2",
                params![device, account, enabled],
            )? > 0)
        })
    }

    pub fn open_pending(&self, account: &str, commitment: &str) -> StoreResult<PendingRow> {
        let now = now_ms();
        let id: String = crate::canonical::digest_of(
            format!("pending\0{account}\0{commitment}\0{now}").as_bytes(),
        )
        .chars()
        .take(22)
        .collect();
        let row = PendingRow {
            id,
            account: account.to_string(),
            commitment: commitment.to_string(),
            answer: None,
            reveal: None,
            expires_at: now + PENDING_TTL_MS,
        };
        self.with(|db| {
            db.execute(
                "INSERT OR REPLACE INTO pending (id, account, commitment, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![row.id, row.account, row.commitment, int(row.expires_at)],
            )?;
            Ok(())
        })?;
        Ok(row)
    }

    fn read_pending(row: &Row<'_>) -> rusqlite::Result<PendingRow> {
        Ok(PendingRow {
            id: row.get("id")?,
            account: row.get("account")?,
            commitment: row.get("commitment")?,
            answer: json_column(row.get("answer")?),
            reveal: json_column(row.get("reveal")?),
            expires_at: uint(row.get("expires_at")?),
        })
    }

    pub fn pending(&self, account: &str, id: &str) -> StoreResult<Option<PendingRow>> {
        let now = int(now_ms());
        self.with(|db| {
            Ok(db
                .query_row(
                    "SELECT * FROM pending WHERE id = ?1 AND account = ?2 AND expires_at > ?3",
                    params![id, account, now],
                    Self::read_pending,
                )
                .optional()?)
        })
    }

    pub fn list_pending(&self, account: &str) -> StoreResult<Vec<PendingRow>> {
        let now = int(now_ms());
        self.with(|db| {
            let mut statement = db.prepare(
                "SELECT * FROM pending WHERE account = ?1 AND expires_at > ?2 ORDER BY id",
            )?;
            let rows = statement
                .query_map(params![account, now], Self::read_pending)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Sets the answer (`column = "answer"`) or reveal (`"reveal"`) once.
    pub fn set_pending_field(
        &self,
        account: &str,
        id: &str,
        column: PendingField,
        value: &Value,
    ) -> StoreResult<bool> {
        let now = int(now_ms());
        let sql = match column {
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
        self.with(|db| Ok(db.execute(sql, params![id, account, value.to_string(), now])? > 0))
    }
}

/// Which half of a pairing row a write fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingField {
    Answer,
    Reveal,
}
