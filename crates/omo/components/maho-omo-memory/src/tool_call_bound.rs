use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use maho_ext_api::AbortSignal;
use memory_core::tools::memory::MemoryWriteStage;

/// Whole-call budget for one memory write (omo #14543). Every git step already has its own
/// deadline (120 s for commit), so this bounds hangs below git such as wedged filesystem calls.
pub const MEMORY_CALL_TIMEOUT_MS: u64 = 180_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCallStage {
    Prepare,
    Lock,
    Check,
    Apply,
    Commit,
    Report,
}

impl From<MemoryWriteStage> for MemoryCallStage {
    fn from(stage: MemoryWriteStage) -> Self {
        match stage {
            MemoryWriteStage::Lock => Self::Lock,
            MemoryWriteStage::Check => Self::Check,
            MemoryWriteStage::Apply => Self::Apply,
            MemoryWriteStage::Commit => Self::Commit,
        }
    }
}

impl MemoryCallStage {
    pub const fn description(self) -> &'static str {
        match self {
            Self::Prepare => "preparing the memory repository",
            Self::Lock => "waiting for the memory writer lock",
            Self::Check => "checking the memory repository state",
            Self::Apply => "editing the memory file",
            Self::Commit => "committing to git",
            Self::Report => "reporting the write",
        }
    }
}

/// Turns `true` before the caller sees a timeout or cancellation so a late-finishing background
/// write skips announcements and write-notice decoration.
#[derive(Clone, Default)]
pub struct MemoryCallState {
    stopped: Arc<AtomicBool>,
}

impl MemoryCallState {
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub(crate) fn mark_stopped(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

const LATE_WRITE_NOTE: &str = "If the write finishes later it still commits, so check the memory repository's git log before retrying.";

pub enum BoundedCall<T, E> {
    Settled(T),
    Failed(E),
    Stopped(String),
}

async fn wait_cancelled(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

/// Starts `start` unless `signal` is already aborted, then races completion against `signal`
/// and `timeout_ms`. Stopped work keeps running detached; its late result is dropped.
pub async fn bound_memory_call<T, E, Fut>(
    start: impl FnOnce(MemoryCallState) -> Fut,
    command: &str,
    stage: impl Fn() -> MemoryCallStage,
    timeout_ms: u64,
    signal: Option<&AbortSignal>,
) -> BoundedCall<T, E>
where
    T: Send + 'static,
    E: Send + 'static,
    Fut: Future<Output = Result<T, E>> + Send + 'static,
{
    if signal.is_some_and(AbortSignal::is_aborted) {
        return BoundedCall::Stopped(cancelled_message(command, stage()));
    }
    let state = MemoryCallState::default();
    let mut work = tokio::spawn(start(state.clone()));
    tokio::select! {
        biased;
        () = wait_cancelled(signal) => {
            state.mark_stopped();
            BoundedCall::Stopped(cancelled_message(command, stage()))
        }
        outcome = &mut work => {
            if signal.is_some_and(AbortSignal::is_aborted) {
                state.mark_stopped();
                return BoundedCall::Stopped(cancelled_message(command, stage()));
            }
            match outcome {
                Ok(Ok(value)) => BoundedCall::Settled(value),
                Ok(Err(error)) => BoundedCall::Failed(error),
                Err(join_error) => BoundedCall::Stopped(format!(
                    "memory: {command} failed while {}: {join_error}",
                    stage().description()
                )),
            }
        }
        () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
            state.mark_stopped();
            BoundedCall::Stopped(timeout_message(command, timeout_ms, stage()))
        }
    }
}

fn cancelled_message(command: &str, stage: MemoryCallStage) -> String {
    format!(
        "memory: {command} was cancelled while {}. {LATE_WRITE_NOTE}",
        stage.description()
    )
}

fn timeout_message(command: &str, timeout_ms: u64, stage: MemoryCallStage) -> String {
    format!(
        "memory: {command} did not finish within {} (stopped while {}). {LATE_WRITE_NOTE}",
        format_seconds(timeout_ms),
        stage.description()
    )
}

fn format_seconds(ms: u64) -> String {
    let tenths = ms.saturating_add(50) / 100;
    if tenths.is_multiple_of(10) {
        format!("{}s", tenths / 10)
    } else {
        format!("{}.{}s", tenths / 10, tenths % 10)
    }
}
