use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;
use tokio::task::AbortHandle;

pub trait TimerHandle: Send + Sync {
    fn cancel(&self);
}

pub trait TimerProvider: Send + Sync {
    fn set_timeout(
        &self,
        callback: Box<dyn FnOnce() + Send>,
        delay_ms: u64,
    ) -> Box<dyn TimerHandle>;
}

pub type SharedTimerProvider = Arc<dyn TimerProvider>;

struct DetachedTimerHandle;

impl TimerHandle for DetachedTimerHandle {
    fn cancel(&self) {}
}

struct TokioTimerHandle {
    abort: AbortHandle,
}

impl TimerHandle for TokioTimerHandle {
    fn cancel(&self) {
        self.abort.abort();
    }
}

pub struct RealTimerProvider;

impl TimerProvider for RealTimerProvider {
    fn set_timeout(
        &self,
        callback: Box<dyn FnOnce() + Send>,
        delay_ms: u64,
    ) -> Box<dyn TimerHandle> {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Box::new(DetachedTimerHandle);
        };
        let join = runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            callback();
        });
        Box::new(TokioTimerHandle {
            abort: join.abort_handle(),
        })
    }
}

pub fn real_timer_provider() -> SharedTimerProvider {
    Arc::new(RealTimerProvider)
}

#[derive(Default)]
struct RepeatState {
    stopped: bool,
    handle: Option<Box<dyn TimerHandle>>,
}

pub struct RepeatHandle {
    state: Arc<Mutex<RepeatState>>,
}

impl RepeatHandle {
    pub fn stop(&self) {
        let handle = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.stopped = true;
            state.handle.take()
        };
        if let Some(handle) = handle {
            handle.cancel();
        }
    }
}

fn arm_repeat(
    timers: &SharedTimerProvider,
    interval_ms: u64,
    tick: &Arc<dyn Fn() + Send + Sync>,
    state: &Arc<Mutex<RepeatState>>,
) {
    let next_timers = Arc::clone(timers);
    let next_tick = Arc::clone(tick);
    let next_state = Arc::clone(state);
    let handle = timers.set_timeout(
        Box::new(move || {
            next_tick();
            let still_running = !next_state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .stopped;
            if still_running {
                arm_repeat(&next_timers, interval_ms, &next_tick, &next_state);
            }
        }),
        interval_ms,
    );
    let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.stopped {
        drop(guard);
        handle.cancel();
    } else {
        guard.handle = Some(handle);
    }
}

pub fn repeat_every(
    timers: SharedTimerProvider,
    interval_ms: u64,
    tick: Arc<dyn Fn() + Send + Sync>,
) -> RepeatHandle {
    let state = Arc::new(Mutex::new(RepeatState::default()));
    arm_repeat(&timers, interval_ms, &tick, &state);
    RepeatHandle { state }
}
