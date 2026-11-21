//! Wire shapes the sync client reads, and the row the server stores for each one.
//!
//! The engine's transport reads `GET /v1/sync/batches?after=n` (a reduced row: seq, kind, the
//! author's kind and id, digest, object id and batch id) and the account chain reads
//! `GET /v1/sync/entries?after=n` (the full row, including a keyring record's body and wraps).
//! Both come from one stored entry; the server never looks inside a batch object.

use serde_json::{Value, json};

/// Whether an entry carries a batch object or only a keyring record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Batch,
    Keyring,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Keyring => "keyring",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "batch" => Some(Self::Batch),
            "keyring" => Some(Self::Keyring),
            _ => None,
        }
    }
}

/// One stored sequence entry.
#[derive(Debug, Clone)]
pub struct EntryRow {
    pub seq: u64,
    pub base_seq: u64,
    pub kind: EntryKind,
    pub account: String,
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

/// The contract's batch id shape: 16 bytes as unpadded base64url (22 characters).
fn is_batch_id(value: &str) -> bool {
    value.len() == 22
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

impl EntryRow {
    /// The id the client's envelope parser accepts, or `None`: the append's idempotency key is
    /// whatever the caller chose, and only a contract-shaped one may appear as a row's batch id.
    fn wire_batch_id(&self) -> Option<&str> {
        self.batch_id.as_deref().filter(|id| is_batch_id(id))
    }

    /// The full row `GET /v1/sync/entries` serves: exactly the keys the client's envelope parser
    /// accepts for this kind, so a keyring row carries its body and wraps and a batch row carries
    /// neither.
    pub fn full(&self) -> Value {
        match self.kind {
            EntryKind::Batch => {
                let mut row = json!({
                    "seq": self.seq,
                    "base_seq": self.base_seq,
                    "kind": "batch",
                    "account": self.account,
                    "epoch": self.epoch,
                    "author": self.author,
                    "digest": self.digest,
                    "link": self.link,
                    "keyring_link": Value::Null,
                    "record_sig": self.record_sig,
                });
                if let Some(batch_id) = self.wire_batch_id()
                    && let Some(object) = row.as_object_mut()
                {
                    object.insert("batch_id".into(), json!(batch_id));
                }
                row
            }
            EntryKind::Keyring => json!({
                "seq": self.seq,
                "base_seq": self.base_seq,
                "kind": "keyring",
                "account": self.account,
                "epoch": self.epoch,
                "author": self.author,
                "digest": self.digest,
                "link": self.link,
                "keyring_link": self.keyring_link.clone().unwrap_or_default(),
                "record_sig": self.record_sig,
                "body": self.body.clone().unwrap_or(Value::Null),
                "wraps": self.wraps.clone().unwrap_or_else(|| json!([])),
            }),
        }
    }

    /// The reduced row `GET /v1/sync/batches` serves: a keyring row names no object.
    pub fn listing(&self) -> Value {
        json!({
            "seq": self.seq,
            "kind": self.kind.as_str(),
            "author": {
                "kind": self.author["kind"].as_str().unwrap_or("device"),
                "id": self.author["id"]
                    .as_str()
                    .or_else(|| self.author["credential_id"].as_str())
                    .unwrap_or_default(),
            },
            "digest": self.digest,
            "object_id": self.object_id,
            "batch_id": self.wire_batch_id(),
        })
    }

    /// `GET /v1/sync/recovery-wraps` rows for this keyring entry.
    pub fn recovery_wraps(&self) -> Vec<Value> {
        let record = json!({
            "seq": self.seq,
            "digest": self.digest,
            "keyring_link": self.keyring_link.clone().unwrap_or_default(),
        });
        self.wraps
            .as_ref()
            .and_then(Value::as_array)
            .map(|wraps| {
                wraps
                    .iter()
                    .map(|wrap| json!({ "record": record.clone(), "entry": wrap }))
                    .collect()
            })
            .unwrap_or_default()
    }
}
