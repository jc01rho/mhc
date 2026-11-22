use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

pub const ATTACH_TAKEOVER_RECORD: &str = "attach.takeover";
const HOLDER_PREFIX: &str = "attach.holder:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachHolder {
    pub takeover_id: String,
    pub session_id: String,
    pub holder: String,
    pub generation: u64,
}

#[async_trait]
pub trait AttachLedger: Send + Sync {
    async fn take_over(&self, session_id: &str, holder: &str) -> Result<AttachHolder, String>;
    async fn holder_of(&self, session_id: &str) -> Result<Option<AttachHolder>, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct DurableRecord {
    pub id: String,
    pub kind: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DurableStoreOp {
    Append {
        id: String,
        kind: String,
        data: Value,
    },
    Put {
        key: String,
        data: Value,
    },
}

#[async_trait]
pub trait DurableAttachStore: Send + Sync {
    fn mint_id(&self) -> String {
        Uuid::new_v4().to_string()
    }
    async fn get_document(&self, key: &str) -> Result<Option<Value>, String>;
    async fn commit(&self, epoch: u64, ops: Vec<DurableStoreOp>) -> Result<(), String>;
    async fn scan_records(&self) -> Result<Vec<DurableRecord>, String>;
}

fn holder_key(session_id: &str) -> String {
    let encoded = serde_json::to_string(session_id).unwrap_or_else(|_| format!("\"{session_id}\""));
    format!("{HOLDER_PREFIX}{encoded}")
}

fn decode_holder(value: &Value) -> Result<AttachHolder, String> {
    let Some(obj) = value.as_object() else {
        return Err("attach: a holder document is not an object".into());
    };
    let takeover_id = obj
        .get("takeoverId")
        .and_then(Value::as_str)
        .ok_or("attach: a holder document is malformed")?;
    let session_id = obj
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("attach: a holder document is malformed")?;
    let holder = obj
        .get("holder")
        .and_then(Value::as_str)
        .ok_or("attach: a holder document is malformed")?;
    let generation = obj
        .get("generation")
        .and_then(Value::as_u64)
        .filter(|&generation| generation >= 1)
        .ok_or("attach: a holder document is malformed")?;
    Ok(AttachHolder {
        takeover_id: takeover_id.to_owned(),
        session_id: session_id.to_owned(),
        holder: holder.to_owned(),
        generation,
    })
}

pub struct StoreAttachLedger {
    store: Arc<dyn DurableAttachStore>,
    epoch: u64,
    admission: Arc<AsyncMutex<()>>,
}

impl StoreAttachLedger {
    pub fn new(
        store: Arc<dyn DurableAttachStore>,
        epoch: u64,
        admission: Arc<AsyncMutex<()>>,
    ) -> Self {
        Self {
            store,
            epoch,
            admission,
        }
    }
}

#[async_trait]
impl AttachLedger for StoreAttachLedger {
    async fn holder_of(&self, session_id: &str) -> Result<Option<AttachHolder>, String> {
        let value = self.store.get_document(&holder_key(session_id)).await?;
        match value {
            Some(v) => decode_holder(&v).map(Some),
            None => Ok(None),
        }
    }

    async fn take_over(&self, session_id: &str, holder: &str) -> Result<AttachHolder, String> {
        let _guard = self.admission.lock().await;
        let current = self.holder_of(session_id).await?;
        if let Some(current) = &current
            && current.holder == holder
        {
            return Ok(current.clone());
        }
        let next = AttachHolder {
            takeover_id: self.store.mint_id(),
            session_id: session_id.to_owned(),
            holder: holder.to_owned(),
            generation: current.map_or(1, |c| c.generation + 1),
        };
        let encoded = json!({
            "takeoverId": next.takeover_id,
            "sessionId": next.session_id,
            "holder": next.holder,
            "generation": next.generation,
        });
        self.store
            .commit(
                self.epoch,
                vec![
                    DurableStoreOp::Append {
                        id: format!("{ATTACH_TAKEOVER_RECORD}:{}", next.takeover_id),
                        kind: ATTACH_TAKEOVER_RECORD.to_owned(),
                        data: encoded.clone(),
                    },
                    DurableStoreOp::Put {
                        key: holder_key(session_id),
                        data: encoded,
                    },
                ],
            )
            .await?;
        Ok(next)
    }
}

#[derive(Default)]
struct MemoryStoreState {
    docs: BTreeMap<String, Value>,
    records: Vec<DurableRecord>,
}

#[derive(Default)]
pub struct MemoryAttachStore {
    state: Mutex<MemoryStoreState>,
}

#[async_trait]
impl DurableAttachStore for MemoryAttachStore {
    async fn get_document(&self, key: &str) -> Result<Option<Value>, String> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .docs
            .get(key)
            .cloned())
    }

    async fn commit(&self, _epoch: u64, ops: Vec<DurableStoreOp>) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for op in ops {
            match op {
                DurableStoreOp::Append { id, kind, data } => {
                    state.records.push(DurableRecord { id, kind, data });
                }
                DurableStoreOp::Put { key, data } => {
                    state.docs.insert(key, data);
                }
            }
        }
        Ok(())
    }

    async fn scan_records(&self) -> Result<Vec<DurableRecord>, String> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .clone())
    }
}

pub fn create_memory_attach_ledger() -> (Arc<StoreAttachLedger>, Arc<MemoryAttachStore>) {
    let store = Arc::new(MemoryAttachStore::default());
    let ledger = Arc::new(StoreAttachLedger::new(
        store.clone(),
        1,
        Arc::new(AsyncMutex::new(())),
    ));
    (ledger, store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn take_over_increments_generation_and_is_idempotent_for_current_holder() {
        let (ledger, store) = create_memory_attach_ledger();
        assert_eq!(ledger.holder_of("session-1").await.unwrap(), None);

        let first = ledger.take_over("session-1", "device-a").await.unwrap();
        assert_eq!(first.holder, "device-a");
        assert_eq!(first.generation, 1);

        let retry = ledger.take_over("session-1", "device-a").await.unwrap();
        assert_eq!(retry, first);
        assert_eq!(store.scan_records().await.unwrap().len(), 1);

        let second = ledger.take_over("session-1", "device-b").await.unwrap();
        assert_eq!(second.holder, "device-b");
        assert_eq!(second.generation, 2);
        assert_eq!(store.scan_records().await.unwrap().len(), 2);
    }
}
