//! Translation of `controlled-clock-test-support.ts`, plus the `deferred` and `within` helpers the
//! manager and directory-scan tests share.

use lsp_core::lsp::manager::ManagerNow;
use lsp_core::lsp::timer_provider::SharedTimerProvider;
use lsp_core::lsp::timer_provider::TimerHandle;
use lsp_core::lsp::timer_provider::TimerProvider;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::watch;

/// Circuit breaker only: fails with `message` when the awaited signal never arrives.
pub async fn within<T>(future: impl Future<Output = T>, message: &str) -> T {
    match tokio::time::timeout(Duration::from_secs(5), future).await {
        Ok(value) => value,
        Err(_) => panic!("{message}"),
    }
}

pub async fn after_yields(turns: usize) {
    for _ in 0..turns {
        tokio::task::yield_now().await;
    }
}

#[derive(Clone)]
pub struct Deferred {
    resolved: Arc<watch::Sender<bool>>,
}

impl Default for Deferred {
    fn default() -> Self {
        Self::new()
    }
}

impl Deferred {
    pub fn new() -> Self {
        Self {
            resolved: Arc::new(watch::channel(false).0),
        }
    }

    pub fn resolve(&self) {
        self.resolved.send_replace(true);
    }

    pub async fn wait(&self) {
        let mut receiver = self.resolved.subscribe();
        let _ = receiver.wait_for(|resolved| *resolved).await;
    }
}

struct ControlledTimer {
    at: u64,
    callback: Box<dyn FnOnce() + Send>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct ClockState {
    now: u64,
    timers: Vec<ControlledTimer>,
    scheduled: usize,
}

struct ControlledTimerHandle {
    cancelled: Arc<AtomicBool>,
}

impl TimerHandle for ControlledTimerHandle {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// Deterministic timer provider: timers fire only through `advance_by`, and `wait_for_timer(delay)`
/// resolves once a live timer with exactly that delay is scheduled, so a test orders itself behind
/// the timer it intends to fire.
pub struct ControlledClock {
    state: Mutex<ClockState>,
    changes: watch::Sender<u64>,
}

impl ControlledClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ClockState::default()),
            changes: watch::channel(0).0,
        })
    }

    fn lock(&self) -> MutexGuard<'_, ClockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn now(&self) -> u64 {
        self.lock().now
    }

    pub fn timers(self: &Arc<Self>) -> SharedTimerProvider {
        Arc::clone(self) as SharedTimerProvider
    }

    pub fn manager_now(self: &Arc<Self>) -> ManagerNow {
        let clock = Arc::clone(self);
        Arc::new(move || clock.now() as f64)
    }

    async fn wait_until(&self, ready: impl Fn(&ClockState) -> bool, message: &str) {
        let mut changes = self.changes.subscribe();
        let wait = async {
            loop {
                let done = {
                    let state = self.lock();
                    ready(&state)
                };
                if done || changes.changed().await.is_err() {
                    return;
                }
            }
        };
        within(wait, message).await;
    }

    pub async fn wait_for_timer(&self, delay_ms: u64) {
        self.wait_until(
            |state| {
                state.timers.iter().any(|timer| {
                    !timer.cancelled.load(Ordering::SeqCst) && timer.at == state.now + delay_ms
                })
            },
            "the awaited timer was never scheduled",
        )
        .await;
    }

    /// Resolves once this clock has been asked to schedule `count` timers in total.
    pub async fn wait_for_scheduled(&self, count: usize) {
        self.wait_until(
            |state| state.scheduled >= count,
            "the awaited number of timers was never scheduled",
        )
        .await;
    }

    pub fn advance_by(&self, delay_ms: u64) {
        self.lock().now += delay_ms;
        loop {
            let due = {
                let mut state = self.lock();
                let now = state.now;
                match state.timers.iter().position(|timer| timer.at <= now) {
                    Some(index) => state.timers.remove(index),
                    None => break,
                }
            };
            if !due.cancelled.load(Ordering::SeqCst) {
                (due.callback)();
            }
        }
    }
}

impl TimerProvider for ControlledClock {
    fn set_timeout(
        &self,
        callback: Box<dyn FnOnce() + Send>,
        delay_ms: u64,
    ) -> Box<dyn TimerHandle> {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.lock();
            let at = state.now + delay_ms;
            state.timers.push(ControlledTimer {
                at,
                callback,
                cancelled: Arc::clone(&cancelled),
            });
            state.scheduled += 1;
        }
        self.changes.send_modify(|version| *version += 1);
        Box::new(ControlledTimerHandle { cancelled })
    }
}
