//! Bounded event queue for a remote session watch (omo `thread/remote/watch-queue.ts`).
//!
//! Past `WATCH_QUEUE_MAX` (256) queued records, content is dropped (keeping the newest `session_status`) and a fresh
//! resync read is forced instead, so a backlog costs detail, never a turn end.

use std::collections::VecDeque;

use serde_json::Value;

pub const WATCH_QUEUE_MAX: usize = 256;

#[derive(Debug, Clone)]
pub enum WatchItem {
    Records {
        attach_id: u64,
        records: Vec<Value>,
        seqs: Option<Vec<u64>>,
    },
    Snapshot {
        attach_id: u64,
    },
    Resync,
    Ended {
        reason: String,
    },
}

#[derive(Default)]
pub struct WatchQueueState {
    pub items: VecDeque<WatchItem>,
    pub records: usize,
    pub closed: bool,
}

impl WatchQueueState {
    pub fn push(&mut self, item: WatchItem) {
        if self.closed {
            return;
        }
        if let WatchItem::Records { ref records, .. } = item {
            self.records += records.len();
        }
        self.items.push_back(item);
        if self.records > WATCH_QUEUE_MAX {
            self.compact();
        }
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.items.clear();
        self.records = 0;
    }

    pub fn pop(&mut self) -> Option<WatchItem> {
        if self.closed {
            return None;
        }
        let item = self.items.pop_front()?;
        if let WatchItem::Records { ref records, .. } = item {
            self.records = self.records.saturating_sub(records.len());
        }
        Some(item)
    }

    fn compact(&mut self) {
        let mut newest_status: Option<WatchItem> = None;
        let mut others: VecDeque<WatchItem> = VecDeque::new();
        for item in self.items.drain(..) {
            match item {
                WatchItem::Records {
                    attach_id,
                    records,
                    seqs,
                } => {
                    for (idx, record) in records.into_iter().enumerate() {
                        if record.get("type").and_then(Value::as_str) == Some("session_status") {
                            let seq = seqs.as_ref().and_then(|s| s.get(idx).copied());
                            newest_status = Some(WatchItem::Records {
                                attach_id,
                                records: vec![record],
                                seqs: seq.map(|s| vec![s]),
                            });
                        }
                    }
                }
                other => others.push_back(other),
            }
        }
        let reads_anyway = others.iter().any(|item| !matches!(item, WatchItem::Resync));
        self.records = usize::from(newest_status.is_some());
        if let Some(status) = newest_status {
            self.items.push_back(status);
        }
        self.items.extend(others);
        if !reads_anyway {
            self.items.push_back(WatchItem::Resync);
        }
    }
}
