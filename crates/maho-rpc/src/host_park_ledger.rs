use futures::future::{BoxFuture, FutureExt, Shared};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle};

pub const MAX_PARKS: usize = 4;
pub const PARK_TTL_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkOutcome {
    Parked,
    AlreadyParked,
    Full,
}

impl ParkOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parked => "parked",
            Self::AlreadyParked => "already_parked",
            Self::Full => "full",
        }
    }
}

pub type ParkClose<T> = Arc<dyn Fn(T) -> BoxFuture<'static, ()> + Send + Sync>;

struct ParkSlot<T: Clone + Send + Sync + 'static> {
    id: u64,
    ready: Shared<BoxFuture<'static, Result<(), String>>>,
    opened: Option<T>,
    expiry: Option<JoinHandle<()>>,
    claiming: Option<Arc<Notify>>,
    closing: Option<Shared<BoxFuture<'static, ()>>>,
}

struct LedgerState<T: Clone + Send + Sync + 'static> {
    next_id: u64,
    parks: BTreeMap<String, ParkSlot<T>>,
    replacements: BTreeMap<String, JoinHandle<()>>,
    stopped: bool,
}

pub struct HostParkLedger<T: Clone + Send + Sync + 'static = String> {
    state: Arc<Mutex<LedgerState<T>>>,
    close: ParkClose<T>,
    ttl_ms: u64,
}

impl<T: Clone + Send + Sync + 'static> Clone for HostParkLedger<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            close: self.close.clone(),
            ttl_ms: self.ttl_ms,
        }
    }
}

impl<T: Clone + Send + Sync + 'static> HostParkLedger<T> {
    pub fn new(close: impl Fn(T) -> BoxFuture<'static, ()> + Send + Sync + 'static) -> Self {
        Self::with_ttl(close, PARK_TTL_MS)
    }

    pub fn with_ttl(
        close: impl Fn(T) -> BoxFuture<'static, ()> + Send + Sync + 'static,
        ttl_ms: u64,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(LedgerState {
                next_id: 0,
                parks: BTreeMap::new(),
                replacements: BTreeMap::new(),
                stopped: false,
            })),
            close: Arc::new(close),
            ttl_ms,
        }
    }

    pub fn size(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .parks
            .len()
    }

    pub async fn park<F>(&self, key: &str, build: F) -> Result<ParkOutcome, String>
    where
        F: FnOnce() -> BoxFuture<'static, Result<T, String>>,
    {
        enum Action {
            WaitExisting(Shared<BoxFuture<'static, Result<(), String>>>),
            Full,
            Build {
                slot_id: u64,
                ready_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
            },
        }
        let action = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(existing) = state.parks.get(key) {
                if existing.claiming.is_none() && existing.closing.is_none() {
                    Action::WaitExisting(existing.ready.clone())
                } else {
                    Action::Full
                }
            } else {
                if let Some(handle) = state.replacements.remove(key) {
                    handle.abort();
                }
                if state.stopped {
                    return Err("host_draining".into());
                }
                if state.parks.len() + state.replacements.len() >= MAX_PARKS {
                    Action::Full
                } else {
                    state.next_id += 1;
                    let slot_id = state.next_id;
                    let (ready_tx, ready_rx) =
                        tokio::sync::oneshot::channel::<Result<(), String>>();
                    let ready = async move {
                        ready_rx
                            .await
                            .unwrap_or_else(|_| Err("park_cancelled".into()))
                    }
                    .boxed()
                    .shared();
                    state.parks.insert(
                        key.to_owned(),
                        ParkSlot {
                            id: slot_id,
                            ready,
                            opened: None,
                            expiry: None,
                            claiming: None,
                            closing: None,
                        },
                    );
                    Action::Build { slot_id, ready_tx }
                }
            }
        };

        match action {
            Action::WaitExisting(ready) => {
                ready.await?;
                Ok(ParkOutcome::AlreadyParked)
            }
            Action::Full => Ok(ParkOutcome::Full),
            Action::Build { slot_id, ready_tx } => match build().await {
                Ok(opened) => {
                    let should_retire = {
                        let mut state = self
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let stopped = state.stopped;
                        if let Some(slot) = state.parks.get_mut(key).filter(|s| s.id == slot_id) {
                            slot.opened = Some(opened);
                            if !stopped {
                                let ledger = self.clone();
                                let key_owned = key.to_owned();
                                let ttl = self.ttl_ms;
                                slot.expiry = Some(tokio::spawn(async move {
                                    tokio::time::sleep(Duration::from_millis(ttl)).await;
                                    ledger.retire(&key_owned, slot_id).await;
                                }));
                            }
                        }
                        stopped
                    };
                    let _ = ready_tx.send(Ok(()));
                    if should_retire {
                        self.retire(key, slot_id).await;
                    }
                    Ok(ParkOutcome::Parked)
                }
                Err(error) => {
                    {
                        let mut state = self
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if state.parks.get(key).is_some_and(|s| s.id == slot_id) {
                            state.parks.remove(key);
                        }
                    }
                    let _ = ready_tx.send(Err(error.clone()));
                    Err(error)
                }
            },
        }
    }

    pub fn take(&self, key: &str) -> Option<T> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stopped {
            return None;
        }
        let slot = state.parks.get_mut(key)?;
        if slot.opened.is_none() || slot.claiming.is_some() || slot.closing.is_some() {
            return None;
        }
        if let Some(expiry) = slot.expiry.take() {
            expiry.abort();
        }
        slot.claiming = Some(Arc::new(Notify::new()));
        slot.opened.clone()
    }

    pub fn finish(&self, key: &str) {
        let notify = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .parks
                .remove(key)
                .and_then(|mut slot| slot.claiming.take())
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }

    pub fn replace<F>(&self, key: &str, build: F)
    where
        F: FnOnce() -> BoxFuture<'static, Result<T, String>> + Send + 'static,
    {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stopped || state.replacements.contains_key(key) {
            return;
        }
        let ledger = self.clone();
        let key_owned = key.to_owned();
        let handle = tokio::spawn(async move {
            tokio::task::yield_now().await;
            {
                let mut state = ledger
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.replacements.remove(&key_owned);
            }
            let _ = ledger.park(&key_owned, build).await;
        });
        state.replacements.insert(key.to_owned(), handle);
    }

    pub async fn stop(&self) {
        type StoppingParkEntry = (
            String,
            u64,
            Shared<BoxFuture<'static, Result<(), String>>>,
            Option<Arc<Notify>>,
        );
        let entries: Vec<StoppingParkEntry> = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.stopped = true;
            for (_, handle) in std::mem::take(&mut state.replacements) {
                handle.abort();
            }
            state
                .parks
                .iter()
                .map(|(k, slot)| {
                    (
                        k.clone(),
                        slot.id,
                        slot.ready.clone(),
                        slot.claiming.clone(),
                    )
                })
                .collect()
        };
        for (key, slot_id, ready, claiming) in entries {
            let _ = ready.await;
            let current_claim = {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .parks
                    .get(&key)
                    .filter(|s| s.id == slot_id)
                    .and_then(|s| s.claiming.clone())
                    .or(claiming)
            };
            if let Some(notify) = current_claim {
                notify.notified().await;
            } else {
                self.retire(&key, slot_id).await;
            }
        }
    }

    async fn retire(&self, key: &str, slot_id: u64) {
        let closing = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(slot) = state.parks.get_mut(key).filter(|s| s.id == slot_id) else {
                return;
            };
            if let Some(expiry) = slot.expiry.take() {
                expiry.abort();
            }
            if let Some(closing) = &slot.closing {
                closing.clone()
            } else {
                let opened = slot.opened.clone();
                let close = self.close.clone();
                let ledger_state = self.state.clone();
                let key_owned = key.to_owned();
                let fut = async move {
                    if let Some(opened) = opened {
                        close(opened).await;
                    }
                    let mut state = ledger_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.parks.get(&key_owned).is_some_and(|s| s.id == slot_id) {
                        state.parks.remove(&key_owned);
                    }
                }
                .boxed()
                .shared();
                slot.closing = Some(fut.clone());
                fut
            }
        };
        closing.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn coalesces_builds_enforces_cap_takes_once_and_stops_cleanly() {
        let closed = Arc::new(AtomicUsize::new(0));
        let closed_for_ledger = closed.clone();
        let ledger = HostParkLedger::new(move |_id: String| {
            let closed = closed_for_ledger.clone();
            async move {
                closed.fetch_add(1, Ordering::SeqCst);
            }
            .boxed()
        });

        let first = ledger
            .park("k1", || async { Ok("s1".to_owned()) }.boxed())
            .await
            .unwrap();
        assert_eq!(first, ParkOutcome::Parked);
        let repeat = ledger
            .park("k1", || async { Ok("s2".to_owned()) }.boxed())
            .await
            .unwrap();
        assert_eq!(repeat, ParkOutcome::AlreadyParked);

        for idx in 2..=4 {
            let outcome = ledger
                .park(&format!("k{idx}"), move || {
                    async move { Ok(format!("s{idx}")) }.boxed()
                })
                .await
                .unwrap();
            assert_eq!(outcome, ParkOutcome::Parked);
        }
        let fifth = ledger
            .park("k5", || async { Ok("s5".to_owned()) }.boxed())
            .await
            .unwrap();
        assert_eq!(fifth, ParkOutcome::Full);

        assert_eq!(ledger.take("k1").as_deref(), Some("s1"));
        assert_eq!(ledger.take("k1"), None);
        assert_eq!(
            ledger
                .park("k1", || async { Ok("while-claimed".to_owned()) }.boxed())
                .await
                .unwrap(),
            ParkOutcome::Full
        );
        ledger.finish("k1");

        ledger.stop().await;
        assert_eq!(closed.load(Ordering::SeqCst), 3);
        assert_eq!(ledger.size(), 0);
    }
}
