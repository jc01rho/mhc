//! The lookup gate of one remote sessions port (omo `remote/port/lookup-gate.ts`, #14748).

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use futures::future::{FutureExt, Shared};

use crate::thread::adapter::BoxFuture;
use crate::thread::remote::ports::{LookupFenceRefused, RemoteLookupOutcome};

/// One single-probe locate as sent: its answer (capped by the caller) and when the locate itself settles.
pub struct LookupStart {
    pub answer: BoxFuture<RemoteLookupOutcome>,
    pub settled: BoxFuture<()>,
}

pub type StartLookupFn = Arc<dyn Fn(&str, &str) -> Result<LookupStart, String> + Send + Sync>;
type SharedSettled = Shared<Pin<Box<dyn Future<Output = ()> + Send>>>;

#[derive(Default)]
struct GateState {
    next_id: u64,
    in_flight: BTreeMap<u64, SharedSettled>,
    fence: Option<u64>,
    solo_queue: VecDeque<u64>,
    wakers: Vec<Waker>,
}

impl GateState {
    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn wake_all(&mut self) {
        for waker in self.wakers.drain(..) {
            waker.wake();
        }
    }

    fn register_waker(&mut self, waker: &Waker) {
        if !self.wakers.iter().any(|existing| existing.will_wake(waker)) {
            self.wakers.push(waker.clone());
        }
    }
}

/// Coordinates every locate of one sessions port so a solo lookup never overlaps another locate (#14748).
#[derive(Clone)]
pub struct LookupGate {
    start: StartLookupFn,
    fence_wait: Duration,
    state: Arc<Mutex<GateState>>,
}

fn lock_state(mutex: &Mutex<GateState>) -> MutexGuard<'_, GateState> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn_wake_timer(deadline: Instant, state: Arc<Mutex<GateState>>) {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        lock_state(&state).wake_all();
        return;
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            tokio::time::sleep(remaining).await;
            lock_state(&state).wake_all();
        });
    } else {
        std::thread::spawn(move || {
            std::thread::sleep(remaining);
            lock_state(&state).wake_all();
        });
    }
}

fn poll_active(state: &Mutex<GateState>, cx: &mut Context<'_>) {
    let mut active: Vec<SharedSettled> = lock_state(state).in_flight.values().cloned().collect();
    for fut in &mut active {
        let _ = Pin::new(fut).poll(cx);
    }
}

impl LookupGate {
    pub fn new<F>(start: F, fence_wait: Duration) -> Self
    where
        F: Fn(&str, &str) -> LookupStart + Send + Sync + 'static,
    {
        Self::try_new(
            move |device, durable| Ok(start(device, durable)),
            fence_wait,
        )
    }

    pub fn try_new<F>(start: F, fence_wait: Duration) -> Self
    where
        F: Fn(&str, &str) -> Result<LookupStart, String> + Send + Sync + 'static,
    {
        Self {
            start: Arc::new(start),
            fence_wait,
            state: Arc::new(Mutex::new(GateState::default())),
        }
    }

    fn track_locked(
        state_arc: &Arc<Mutex<GateState>>,
        guard: &mut GateState,
        settled: BoxFuture<()>,
        release_solo: Option<u64>,
    ) -> SharedSettled {
        let id = guard.alloc_id();
        let state_for_done = Arc::clone(state_arc);
        let tracked = async move {
            settled.await;
            let mut st = lock_state(&state_for_done);
            st.in_flight.remove(&id);
            if let Some(turn_id) = release_solo {
                if st.fence == Some(turn_id) {
                    st.fence = None;
                }
                st.solo_queue.retain(|&item| item != turn_id);
            }
            st.wake_all();
        }
        .boxed()
        .shared();
        guard.in_flight.insert(id, tracked.clone());
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(tracked.clone());
        }
        tracked
    }

    fn wrap_answer<T: Send + 'static>(
        mut answer: BoxFuture<T>,
        mut settled: SharedSettled,
    ) -> BoxFuture<T> {
        Box::pin(futures::future::poll_fn(move |cx| {
            let _ = Pin::new(&mut settled).poll(cx);
            Pin::new(&mut answer).poll(cx)
        }))
    }

    async fn admitted<T, D>(&self, dispatch: D) -> Option<T>
    where
        D: FnOnce(&Arc<Mutex<GateState>>, &mut GateState) -> T + Send,
    {
        let deadline = Instant::now() + self.fence_wait;
        let mut timer_armed = false;
        let mut dispatch_slot = Some(dispatch);
        futures::future::poll_fn(|cx| {
            poll_active(&self.state, cx);
            let mut st = lock_state(&self.state);
            if st.fence.is_none() {
                let run = dispatch_slot.take().expect("admitted polled once");
                return Poll::Ready(Some(run(&self.state, &mut st)));
            }
            if Instant::now() >= deadline {
                return Poll::Ready(None);
            }
            st.register_waker(cx.waker());
            drop(st);
            if !timer_armed {
                timer_armed = true;
                spawn_wake_timer(deadline, Arc::clone(&self.state));
            }
            Poll::Pending
        })
        .await
    }

    pub async fn admit<T, F, Fut>(&self, dispatch: F) -> Result<T, LookupFenceRefused>
    where
        T: Send + 'static,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = T> + Send + 'static,
    {
        let mut dispatch_opt = Some(dispatch);
        let started = self
            .admitted(|state_arc, guard| {
                let make = dispatch_opt.take().expect("dispatch once");
                let (tx, rx) = futures::channel::oneshot::channel();
                let fut = make();
                let settled: BoxFuture<()> = Box::pin(async move {
                    let _ = tx.send(fut.await);
                });
                let shared = Self::track_locked(state_arc, guard, settled, None);
                Self::wrap_answer(Box::pin(async move { rx.await.ok() }), shared)
            })
            .await;
        match started {
            Some(work) => work.await.ok_or(LookupFenceRefused),
            None => Err(LookupFenceRefused),
        }
    }

    pub async fn lookup(&self, device: &str, durable_id: &str) -> RemoteLookupOutcome {
        let start = Arc::clone(&self.start);
        let (dev, dur) = (device.to_owned(), durable_id.to_owned());
        let started = self
            .admitted(move |state_arc, guard| {
                start(&dev, &dur).map(|LookupStart { answer, settled }| {
                    let shared = Self::track_locked(state_arc, guard, settled, None);
                    Self::wrap_answer(answer, shared)
                })
            })
            .await;
        match started {
            None => RemoteLookupOutcome::Silent {
                reason: "lookup_timeout".to_owned(),
            },
            Some(Ok(answer)) => answer.await,
            Some(Err(reason)) => RemoteLookupOutcome::Silent { reason },
        }
    }

    pub async fn lookup_alone(
        &self,
        device: &str,
        durable_id: &str,
        wait_ms: i64,
    ) -> Option<RemoteLookupOutcome> {
        self.try_lookup_alone(device, durable_id, wait_ms)
            .await
            .ok()
            .flatten()
    }

    pub async fn try_lookup_alone(
        &self,
        device: &str,
        durable_id: &str,
        wait_ms: i64,
    ) -> Result<Option<RemoteLookupOutcome>, String> {
        let deadline =
            Instant::now() + Duration::from_millis(u64::try_from(wait_ms.max(0)).unwrap_or(0));
        let turn_id = {
            let mut st = lock_state(&self.state);
            let id = st.alloc_id();
            st.solo_queue.push_back(id);
            id
        };
        let mut timer_armed = false;
        let start = Arc::clone(&self.start);
        let (dev, dur, state_arc) = (
            device.to_owned(),
            durable_id.to_owned(),
            Arc::clone(&self.state),
        );
        let outcome = futures::future::poll_fn(|cx| {
            poll_active(&state_arc, cx);
            let mut st = lock_state(&state_arc);
            if st.solo_queue.front() == Some(&turn_id) {
                st.fence = Some(turn_id);
                if st.in_flight.is_empty() {
                    return match start(&dev, &dur) {
                        Ok(LookupStart { answer, settled }) => {
                            let shared =
                                Self::track_locked(&state_arc, &mut st, settled, Some(turn_id));
                            Poll::Ready(Ok(Some(Self::wrap_answer(answer, shared))))
                        }
                        Err(err) => {
                            st.fence = None;
                            st.solo_queue.pop_front();
                            st.wake_all();
                            Poll::Ready(Err(err))
                        }
                    };
                }
            }
            if Instant::now() >= deadline {
                if st.fence == Some(turn_id) {
                    st.fence = None;
                }
                st.solo_queue.retain(|&id| id != turn_id);
                st.wake_all();
                return Poll::Ready(Ok(None));
            }
            st.register_waker(cx.waker());
            drop(st);
            if !timer_armed {
                timer_armed = true;
                spawn_wake_timer(deadline, Arc::clone(&state_arc));
            }
            Poll::Pending
        })
        .await?;
        Ok(match outcome {
            Some(answer) => Some(answer.await),
            None => None,
        })
    }
}

#[cfg(test)]
#[path = "lookup_gate_tests.rs"]
mod tests;
