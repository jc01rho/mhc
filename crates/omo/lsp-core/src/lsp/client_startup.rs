//! Port of TS `client-startup.ts`: the lifecycle of a starting client, owned by the LspManager.
//!
//! The manager owns every startup and every shutdown; a caller only waits. A startup begun by
//! `get_client` is held by its waiters, counted in `pending_waiters` from the moment they join until
//! each one either becomes a reference or leaves. A waiter whose signal aborts (a caller
//! cancellation or a directory scan's start deadline) detaches: it stops counting and rejects at
//! once, and never awaits a shutdown. When the last waiter of a waiter-held startup leaves while it
//! holds no reference, the manager drops the startup from its pool (so `client_count()` no longer
//! counts it) and stops it as a tracked tombstone, which the next `get_client` for that key and
//! `stop_all() await, so no process outlives the manager. A start that fails is dropped and stopped
//! the same way, once. A warmup startup has no waiter to lose and runs to its end.
//!
//! States of one key: ABSENT; STARTING (N counted waiters); RESIDENT (R references, plus the counted
//! waiters of a start that just finished); STOPPING (not pooled, stop tracked in `pending_stops`).
//!
//! - `get_client` resident hit: R+1 synchronously, no await.
//! - `get_client` cold start or joining a start: counted (N+1) synchronously, then awaits the start.
//!   When it resumes it stops counting and takes its reference (R+1) in one synchronous step, after
//!   checking it still holds the pooled instance for its key and that the client is alive. A client
//!   that is no longer pooled (stopped by the init-timeout reaper, `invalidate_client` or `stop_all`
//!   while it waited) is never handed out: the caller acquires again through the normal path, which
//!   awaits that stop and starts a fresh client, or rejects once the manager is disposed.
//! - Waiter abort or deadline: N-1, rejects; the last waiter leaving stops the startup as above.
//! - `release_client`: R-1 on the instance the caller held; a stale instance gives back nothing.
//! - Eviction at the resident cap and the idle reaper take only a RESIDENT client with R = 0 and
//!   N = 0.
//! - The init-timeout reaper stops a startup older than the init timeout whatever its waiters, and
//!   `stop_all` stops everything; neither hands the stopped client to a caller.
//!
//! The pool transitions themselves live in [`crate::lsp::manager::LspManager`], because the pool and
//! the stop tombstones are there; this module holds the per-key record and the two waits.

use crate::abort::AbortSignal;
use crate::lsp::errors::LspError;
use crate::lsp::manager::SharedClient;
use std::future::Future;
use tokio::sync::watch;

/// TS `InitOutcome`: `None` while the start is in flight.
pub type InitOutcome = Option<Result<(), LspError>>;

/// TS `ManagedClient`: one pooled (or starting) client and its accounting.
pub struct ManagedClient {
    pub id: u64,
    pub key: String,
    pub root: String,
    pub server_id: String,
    pub client: SharedClient,
    /// False for a warmup startup, which no waiter holds.
    pub held_by_waiters: bool,
    pub ref_count: usize,
    pub pending_waiters: usize,
    pub last_used_at: f64,
    pub init: Option<watch::Receiver<InitOutcome>>,
    pub is_initializing: bool,
    pub initializing_since: Option<f64>,
}

impl ManagedClient {
    /// TS `startupExpired`: a start older than the init timeout is stopped by the reaper.
    pub fn startup_expired(&self, now: f64, init_timeout_ms: f64) -> bool {
        self.is_initializing
            && self
                .initializing_since
                .is_some_and(|since| now - since > init_timeout_ms)
    }
}

/// TS `awaitWithSignal`: the wait rejects with [`LspError::Aborted`] as soon as `signal` aborts.
pub async fn await_with_signal<F, T>(fut: F, signal: Option<&AbortSignal>) -> Result<T, LspError>
where
    F: Future<Output = T>,
{
    match signal {
        None => Ok(fut.await),
        Some(sig) => {
            if sig.aborted() {
                return Err(LspError::Aborted);
            }
            tokio::select! {
                res = fut => Ok(res),
                () = sig.cancelled() => Err(LspError::Aborted),
            }
        }
    }
}

/// Waits for one startup's outcome, detaching on an aborted signal.
pub async fn await_init(
    mut receiver: watch::Receiver<InitOutcome>,
    signal: Option<&AbortSignal>,
) -> Result<(), LspError> {
    let wait = async {
        match receiver.wait_for(Option::is_some).await {
            Ok(outcome) => outcome.clone().unwrap_or(Ok(())),
            Err(_) => Err(LspError::other("LSP client initialization was abandoned")),
        }
    };
    match signal {
        None => wait.await,
        Some(signal) => {
            if signal.aborted() {
                return Err(LspError::Aborted);
            }
            tokio::select! {
                outcome = wait => outcome,
                () = signal.cancelled() => Err(LspError::Aborted),
            }
        }
    }
}
