//! Port of `local-api/chunking.ts`: a blob's events, and one attach's reassembly (one blob in flight,
//! chunks in order, sha256 checked at `blob_done`).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::errors::LocalApiError;
use super::limits::{LOCAL_BLOB_MAX, LOCAL_CHUNK_MAX, chunk_payload, sha256_hex};
use super::protocol::event_message;

/// `blob_open`, its chunks and `blob_done` for `data`; `target` carries `of` and `position`.
pub fn blob_events(attach: &str, id: &str, target: &Value, data: &[u8]) -> Vec<Value> {
    let mut open =
        json!({ "attach": attach, "id": id, "bytes": data.len(), "sha256": sha256_hex(data) });
    if let (Some(open), Some(target)) = (open.as_object_mut(), target.as_object()) {
        for (key, value) in target {
            open.insert(key.clone(), value.clone());
        }
    }
    let mut events = vec![event_message("session.blob_open", open)];
    for (off, b) in chunk_payload(data) {
        events.push(event_message(
            "session.blob_chunk",
            json!({ "attach": attach, "id": id, "off": off, "b": b }),
        ));
    }
    events.push(event_message(
        "session.blob_done",
        json!({ "attach": attach, "id": id }),
    ));
    events
}

#[derive(Clone, Debug, PartialEq)]
pub enum BlobOutcome {
    Blob { open: Value, data: Vec<u8> },
    Aborted { open: Value, reason: String },
    TooLarge { query_id: String, entry_id: String },
}

#[derive(Debug, Default)]
pub struct LocalBlobAssembler {
    open: Option<Value>,
    data: Vec<u8>,
}

fn field<'a>(event: &'a Value, key: &str) -> Option<&'a str> {
    event.get(key).and_then(Value::as_str)
}

impl LocalBlobAssembler {
    pub fn in_flight(&self) -> bool {
        self.open.is_some()
    }

    pub fn accept(&mut self, event: &Value) -> Result<Option<BlobOutcome>, LocalApiError> {
        let violation = LocalApiError::protocol_violation;
        match field(event, "kind") {
            Some("session.blob_open") => {
                if self.open.is_some() {
                    return Err(violation("one blob in flight per attach"));
                }
                let bytes = event
                    .get("bytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX);
                if bytes > LOCAL_BLOB_MAX as u64 {
                    return Err(violation("blob over LOCAL_BLOB_MAX"));
                }
                self.open = Some(event.clone());
                Ok(None)
            }
            Some("session.blob_chunk") => {
                let total = self.current(field(event, "id"))?;
                let chunk = STANDARD
                    .decode(field(event, "b").unwrap_or_default())
                    .map_err(|_| violation("blob chunk is not base64"))?;
                let end = self.data.len() + chunk.len();
                if event.get("off").and_then(Value::as_u64) != Some(self.data.len() as u64)
                    || end as u64 > total
                {
                    return Err(violation("blob chunk out of order"));
                }
                if chunk.len() != LOCAL_CHUNK_MAX && end as u64 != total {
                    return Err(violation("short blob chunk"));
                }
                self.data.extend_from_slice(&chunk);
                Ok(None)
            }
            Some("session.blob_done") => {
                let total = self.current(field(event, "id"))?;
                if self.data.len() as u64 != total {
                    return Err(violation("blob done before its last chunk"));
                }
                let open = self.open.take().unwrap_or(Value::Null);
                let data = std::mem::take(&mut self.data);
                if Some(sha256_hex(&data).as_str()) != field(&open, "sha256") {
                    return Err(violation("blob digest mismatch"));
                }
                Ok(Some(BlobOutcome::Blob { open, data }))
            }
            Some("session.blob_abort") => {
                let reason = field(event, "reason").unwrap_or_default().to_owned();
                if reason == "too_large" {
                    return Ok(Some(BlobOutcome::TooLarge {
                        query_id: field(event, "query_id").unwrap_or_default().to_owned(),
                        entry_id: field(event, "entry_id").unwrap_or_default().to_owned(),
                    }));
                }
                self.current(field(event, "id"))?;
                let open = self.open.take().unwrap_or(Value::Null);
                self.data.clear();
                Ok(Some(BlobOutcome::Aborted { open, reason }))
            }
            _ => Err(violation("not a blob event")),
        }
    }

    fn current(&self, id: Option<&str>) -> Result<u64, LocalApiError> {
        match &self.open {
            Some(open) if id.is_some() && field(open, "id") == id => {
                Ok(open.get("bytes").and_then(Value::as_u64).unwrap_or(0))
            }
            _ => Err(LocalApiError::protocol_violation("blob not open")),
        }
    }
}
