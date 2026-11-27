//! Stamps closed and reopened session states onto remote listing rows until a newer snapshot arrives
//! (`thread/remote/closed-rows.ts`; omo #14170, #14273, #14280).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::thread::adapter::BoxFuture;

use super::ports::{
    RemoteCommandOutcome, RemoteDevice, RemoteListing, RemoteLocateOutcome, RemoteLookupOutcome,
    RemoteSessionRow, RemoteSessionsPort, RemoteThreadSurface, RowSource, SessionCommandOptions,
};

#[derive(Debug, Clone)]
enum Mark {
    Closed {
        updated_ms: i64,
        file: Option<String>,
    },
    Reopened {
        updated_ms: i64,
        key: String,
    },
}

impl Mark {
    fn updated_ms(&self) -> i64 {
        match self {
            Self::Closed { updated_ms, .. } | Self::Reopened { updated_ms, .. } => *updated_ms,
        }
    }
}

fn mark_key(row: &RemoteSessionRow) -> String {
    let id = row
        .durable_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .unwrap_or(&row.session_id);
    format!("{}\0{id}", row.device)
}

#[derive(Default)]
struct MarkLedger {
    marks: Mutex<HashMap<String, Mark>>,
}

impl MarkLedger {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Mark>> {
        self.marks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn settle(&self, mut row: RemoteSessionRow) -> RemoteSessionRow {
        let key = mark_key(&row);
        let mut marks = self.lock();
        let Some(mark) = marks.get(&key).cloned() else {
            return row;
        };
        let overtaken = row.updated_ms > mark.updated_ms()
            || matches!(&mark, Mark::Reopened { key: reopened_key, .. } if row.session_id == *reopened_key);
        if overtaken {
            marks.remove(&key);
            return row;
        }
        match mark {
            Mark::Closed { file, .. } => {
                row.state = "ended".to_owned();
                if let Some(file) = file {
                    row.session_file = Some(file);
                }
            }
            Mark::Reopened {
                key: reopened_key, ..
            } => {
                row.session_id = reopened_key;
                if row.state == "ended" {
                    row.state = "idle".to_owned();
                }
                row.source = Some(RowSource::Live);
            }
        }
        row
    }
}

/// Wraps a [`RemoteSessionsPort`] so local close and reopen outcomes remain visible across `sessions` and `listing`
/// calls until the account listing catches up.
pub struct ClosedRowsPort {
    inner: Arc<dyn RemoteSessionsPort>,
    ledger: Arc<MarkLedger>,
}

impl ClosedRowsPort {
    pub fn new(inner: Arc<dyn RemoteSessionsPort>) -> Self {
        Self {
            inner,
            ledger: Arc::new(MarkLedger::default()),
        }
    }
}

impl RemoteSessionsPort for ClosedRowsPort {
    fn this_device(&self) -> BoxFuture<Option<String>> {
        self.inner.this_device()
    }

    fn this_device_name(&self) -> BoxFuture<Option<String>> {
        self.inner.this_device_name()
    }

    fn devices(&self) -> BoxFuture<Result<Vec<RemoteDevice>, String>> {
        self.inner.devices()
    }

    fn sessions(&self, device: Option<String>) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
        let fut = self.inner.sessions(device);
        let ledger = Arc::clone(&self.ledger);
        Box::pin(async move {
            let rows = fut.await?;
            Ok(rows.into_iter().map(|row| ledger.settle(row)).collect())
        })
    }

    fn listing(&self, device: Option<String>) -> BoxFuture<Result<RemoteListing, String>> {
        let fut = self.inner.listing(device);
        let ledger = Arc::clone(&self.ledger);
        Box::pin(async move {
            let mut listed = fut.await?;
            listed.rows = listed
                .rows
                .into_iter()
                .map(|row| ledger.settle(row))
                .collect();
            Ok(listed)
        })
    }

    fn locate(&self, device: String, engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
        self.inner.locate(device, engine_id)
    }

    fn lookup(&self, device: String, durable_id: String) -> BoxFuture<RemoteLookupOutcome> {
        self.inner.lookup(device, durable_id)
    }

    fn lookup_alone(
        &self,
        device: String,
        durable_id: String,
        timeout_ms: i64,
    ) -> BoxFuture<Option<RemoteLookupOutcome>> {
        self.inner.lookup_alone(device, durable_id, timeout_ms)
    }

    fn can_lookup(&self) -> BoxFuture<bool> {
        self.inner.can_lookup()
    }

    fn can_wake(&self) -> BoxFuture<bool> {
        self.inner.can_wake()
    }

    fn session_command(
        &self,
        device: String,
        session_id: String,
        rpc: Value,
        options: SessionCommandOptions,
    ) -> BoxFuture<RemoteCommandOutcome> {
        self.inner.session_command(device, session_id, rpc, options)
    }

    fn closed(&self, row: &RemoteSessionRow, file: Option<&str>, at: Option<i64>) {
        let updated_ms = row.updated_ms.max(at.unwrap_or(row.updated_ms));
        self.ledger.lock().insert(
            mark_key(row),
            Mark::Closed {
                updated_ms,
                file: file.map(str::to_owned),
            },
        );
        self.inner.closed(row, file, at);
    }

    fn reopened(&self, row: &RemoteSessionRow, key: &str) {
        self.ledger.lock().insert(
            mark_key(row),
            Mark::Reopened {
                updated_ms: row.updated_ms,
                key: key.to_owned(),
            },
        );
        self.inner.reopened(row, key);
    }
}

/// Wraps a [`RemoteThreadSurface`] with a [`ClosedRowsPort`] while sharing its client, aliases and `last_listed`.
pub fn with_closed_rows(remote: Arc<RemoteThreadSurface>) -> Arc<RemoteThreadSurface> {
    Arc::new(RemoteThreadSurface {
        sessions: Arc::new(ClosedRowsPort::new(Arc::clone(&remote.sessions))),
        client: Arc::clone(&remote.client),
        aliases: remote.aliases.clone(),
        last_listed: remote.last_listed.clone(),
    })
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;

    use super::*;
    use crate::thread::remote::ports::{
        ClientDeviceRow, RemoteClient, RemoteFailure, ServiceStatus,
    };

    struct StubClient;

    impl RemoteClient for StubClient {
        fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn service_status(&self) -> BoxFuture<Result<ServiceStatus, RemoteFailure>> {
            Box::pin(async {
                Ok(ServiceStatus {
                    state: "connected".to_owned(),
                    device_id: Some("here".to_owned()),
                })
            })
        }
    }

    struct StubSessions {
        listed: Mutex<RemoteListing>,
    }

    impl RemoteSessionsPort for StubSessions {
        fn sessions(
            &self,
            _device: Option<String>,
        ) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
            let rows = self
                .listed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .rows
                .clone();
            Box::pin(async move { Ok(rows) })
        }

        fn listing(&self, _device: Option<String>) -> BoxFuture<Result<RemoteListing, String>> {
            let listed = self
                .listed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            Box::pin(async move { Ok(listed) })
        }

        fn locate(&self, _device: String, _engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
            Box::pin(async {
                RemoteLocateOutcome::Refused {
                    reason: "unknown_session".to_owned(),
                }
            })
        }
    }

    fn base_row(updated_ms: i64, session_id: &str, state: &str) -> RemoteSessionRow {
        RemoteSessionRow {
            device: "b".to_owned(),
            session_id: session_id.to_owned(),
            durable_id: Some("dur-1".to_owned()),
            title: "beta".to_owned(),
            project: "/work/b".to_owned(),
            path: "/work/b".to_owned(),
            state: state.to_owned(),
            updated_ms,
            ..RemoteSessionRow::default()
        }
    }

    #[test]
    fn closed_stamps_ended_until_newer_snapshot_arrives_across_sessions_and_listing() {
        let sessions = Arc::new(StubSessions {
            listed: Mutex::new(RemoteListing {
                rows: vec![base_row(1000, "rk-1", "idle")],
                incomplete: Vec::new(),
                devices: None,
            }),
        });
        let wrapped = with_closed_rows(Arc::new(RemoteThreadSurface::new(
            sessions.clone(),
            Arc::new(StubClient),
            None,
        )));

        let row = base_row(1000, "rk-1", "idle");
        wrapped
            .sessions
            .closed(&row, Some("/b/dur-1.jsonl"), Some(1500));

        let listed = block_on(wrapped.sessions.listing(Some("b".into()))).expect("listing");
        assert_eq!(listed.rows[0].state, "ended");
        assert_eq!(
            listed.rows[0].session_file.as_deref(),
            Some("/b/dur-1.jsonl")
        );
        let rows = block_on(wrapped.sessions.sessions(None)).expect("sessions");
        assert_eq!(rows[0].state, "ended");

        sessions
            .listed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rows = vec![base_row(1600, "rk-2", "idle")];
        let caught_up = block_on(wrapped.sessions.listing(None)).expect("caught up");
        assert_eq!(caught_up.rows[0].state, "idle");
        assert_eq!(caught_up.rows[0].session_file, None);
    }

    #[test]
    fn reopened_stamps_live_key_until_listing_catches_up() {
        let sessions = Arc::new(StubSessions {
            listed: Mutex::new(RemoteListing {
                rows: vec![base_row(1000, "rk-old", "ended")],
                incomplete: Vec::new(),
                devices: None,
            }),
        });
        let wrapped = with_closed_rows(Arc::new(RemoteThreadSurface::new(
            sessions.clone(),
            Arc::new(StubClient),
            None,
        )));

        let row = base_row(1000, "rk-old", "ended");
        wrapped.sessions.reopened(&row, "rk-new");

        let listed = block_on(wrapped.sessions.listing(None)).expect("listing");
        assert_eq!(listed.rows[0].session_id, "rk-new");
        assert_eq!(listed.rows[0].state, "idle");
        assert_eq!(listed.rows[0].source, Some(RowSource::Live));

        sessions
            .listed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rows = vec![base_row(1000, "rk-new", "running")];
        let caught_up = block_on(wrapped.sessions.sessions(None)).expect("caught up");
        assert_eq!(caught_up[0].state, "running");
    }
}
