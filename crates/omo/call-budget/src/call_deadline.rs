//! `components/call-budget/call-deadline.ts` (omo#14130): one thread tool call may send several
//! commands to another device (a send's state probe, its admission and the state read after it; a
//! read's pages). Each sizes its wait to what is left of the call's budget, so the whole call
//! settles, typed, inside the 60 s a host-tool bridge (the eval kernel's) gives a tool call before
//! dropping it untyped.
//!
//! Node's `AsyncLocalStorage` is a tokio task-local here: the deadline is visible to everything
//! polled inside [`with_call_deadline`], and `None` inside [`outside_call_deadline`].

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TOOL_CALL_BUDGET_MS: i64 = 50_000;

/// What a wait inside a call leaves for the read after it. A read's own bound is 30 s
/// (`READ_WAIT_MS`), longer than this reserve, so a slow paged read after a long wait can still end
/// as a typed `request_timeout`; a full 30 s reserve would cap every wait at 20 s instead.
pub const READ_RESERVE_MS: i64 = 15_000;

tokio::task_local! {
    static CALL_DEADLINE: Option<i64>;
}

/// Wall-clock epoch ms, the clock a call's deadline is stated in.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn current_deadline() -> Option<i64> {
    CALL_DEADLINE.try_with(|deadline| *deadline).ok().flatten()
}

/// Runs `call` with every remote command inside it bounded by `deadline_ms` (epoch ms).
pub async fn with_call_deadline<F: Future>(deadline_ms: i64, call: F) -> F::Output {
    CALL_DEADLINE.scope(Some(deadline_ms), call).await
}

/// The wait a remote command may take: `bound`, cut to what is left of the enclosing call's
/// deadline (none: `bound`). A deadline already past yields a negative wait, as upstream.
pub fn command_wait_ms(bound: i64, now: i64) -> i64 {
    match current_deadline() {
        Some(deadline) => bound.min(deadline - now),
        None => bound,
    }
}

/// A wait inside a call: `requested`, cut so that `reserve` of the call's budget is left for what
/// follows, never below zero (none: as asked).
pub fn wait_within_call_budget(requested: i64, reserve: i64, now: i64) -> i64 {
    match current_deadline() {
        Some(deadline) => requested.min(deadline - now - reserve).max(0),
        None => requested,
    }
}

/// Runs `work` outside any call's budget: a background task a call queued (a held re-send)
/// outlives that call.
pub fn outside_call_deadline<R>(work: impl FnOnce() -> R) -> R {
    CALL_DEADLINE.sync_scope(None, work)
}

/// [`outside_call_deadline`] for work that is polled as a future.
pub async fn outside_call_deadline_async<F: Future>(work: F) -> F::Output {
    CALL_DEADLINE.scope(None, work).await
}

/// `work`, or `None` once the enclosing call's budget runs out first (no enclosing call: `work` as
/// it is). Work cut off this way keeps running in the background, still inside the call's
/// deadline, as a JavaScript promise does; a panic inside it reaches the caller.
pub async fn within_call_budget<T, F>(work: F, now: i64) -> Option<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let Some(deadline) = current_deadline() else {
        return Some(work.await);
    };
    let remaining = Duration::from_millis(u64::try_from(deadline - now).unwrap_or(0));
    let running = tokio::spawn(CALL_DEADLINE.scope(Some(deadline), work));
    match tokio::time::timeout(remaining, running).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Ok(Err(_)) | Err(_) => None,
    }
}
