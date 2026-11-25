//! A hand-driven world for `with_rpc_socket` (omo `socket-client*.test.ts`): a clock the test moves and whose stall
//! probe it runs, timers it fires, and a fake host behind every dial. Only the conversation's own timers and probe
//! run on it (omo#14809); the bound in `settle` stays on tokio's real clock.

use std::future::Future;
use std::io::ErrorKind;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use maho_rpc::socket_client::{
    BoxFuture, ConnectFuture, ConversationTimers, MonotonicClock, ProbeGuard, ProbeTick,
    RpcConnector, RpcRequest, RpcSocketError, RpcSocketOptions, RpcSocketRuntime, RpcStream,
    StallProbe,
};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Awaits `future` within a bound, so a signal the conversation never sends fails the test instead of hanging it.
pub async fn settle<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("the conversation settles within the bound")
}

pub async fn drive<T, H>(call: impl Future<Output = T>, host: impl Future<Output = H>) -> (T, H) {
    settle(async { tokio::join!(call, host) }).await
}

pub async fn read_messages(mut request: RpcRequest) -> Result<Value, RpcSocketError> {
    request.send(json!({"type": "get_messages"}), None).await
}

type Probes = Arc<Mutex<Vec<(u64, ProbeTick)>>>;

/// The caller's clock and the stall meter's probe (omo `controlledTime`, `manualClock`).
pub struct ManualClock {
    now: Mutex<f64>,
    probes: Probes,
    next_probe: AtomicU64,
}

impl ManualClock {
    pub fn at(now: f64) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(now),
            probes: Probes::default(),
            next_probe: AtomicU64::new(0),
        })
    }

    /// The caller cannot run: the clock moves and no probe looks.
    pub fn stall(&self, ms: u64) {
        *lock(&self.now) += ms as f64;
    }

    /// The caller is responsive: every probe period the clock moves 50 ms and the probe looks.
    pub fn responsive(&self, ms: u64) {
        for _ in 0..ms.div_ceil(50) {
            *lock(&self.now) += 50.0;
            self.overdue_probe();
        }
    }

    /// A probe callback that was already due runs now; the clock does not move.
    pub fn overdue_probe(&self) {
        let ticks: Vec<ProbeTick> = lock(&self.probes)
            .iter()
            .map(|(_, tick)| Arc::clone(tick))
            .collect();
        for tick in ticks {
            tick();
        }
    }

    pub fn probes(&self) -> usize {
        lock(&self.probes).len()
    }
}

impl MonotonicClock for ManualClock {
    fn now_ms(&self) -> f64 {
        *lock(&self.now)
    }
}

impl StallProbe for ManualClock {
    fn start(&self, _period_ms: f64, tick: ProbeTick) -> ProbeGuard {
        let id = self.next_probe.fetch_add(1, Ordering::SeqCst);
        lock(&self.probes).push((id, tick));
        let probes = Arc::clone(&self.probes);
        ProbeGuard::new(move || lock(&probes).retain(|(probe, _)| *probe != id))
    }
}

/// Timers a conversation arms; each fires only when the test says so (omo `manualClock`'s `armed`, `fireLast`).
#[derive(Default)]
pub struct ManualTimers {
    armed: Mutex<Vec<u64>>,
    fires: Mutex<Vec<oneshot::Sender<()>>>,
}

impl ManualTimers {
    /// The delay of every timer armed so far, in milliseconds, in order.
    pub fn armed(&self) -> Vec<u64> {
        lock(&self.armed).clone()
    }

    /// Fires the timer armed last, as the event loop would once its delay passed.
    pub fn fire_last(&self) {
        if let Some(fire) = lock(&self.fires).pop() {
            let _ = fire.send(());
        }
    }
}

impl ConversationTimers for ManualTimers {
    fn sleep(&self, delay_ms: f64) -> BoxFuture<()> {
        let (fire, fired) = oneshot::channel();
        lock(&self.armed).push(delay_ms.round() as u64);
        lock(&self.fires).push(fire);
        Box::pin(async move {
            if fired.await.is_err() {
                std::future::pending::<()>().await;
            }
        })
    }
}

enum HostEvent {
    Data(Vec<u8>),
    Drop(ErrorKind, String),
    Close,
}

struct FakeSocket {
    events: mpsc::UnboundedReceiver<HostEvent>,
    writes: mpsc::UnboundedSender<Vec<u8>>,
    delivered: mpsc::UnboundedSender<usize>,
    unread: Vec<u8>,
    closed: bool,
}

impl AsyncRead for FakeSocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let socket = self.get_mut();
        if socket.unread.is_empty() && !socket.closed {
            match socket.events.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(HostEvent::Data(bytes))) => socket.unread = bytes,
                Poll::Ready(Some(HostEvent::Drop(kind, message))) => {
                    return Poll::Ready(Err(std::io::Error::new(kind, message)));
                }
                Poll::Ready(Some(HostEvent::Close) | None) => socket.closed = true,
            }
        }
        let size = socket.unread.len().min(buf.remaining());
        buf.put_slice(&socket.unread[..size]);
        socket.unread.drain(..size);
        if size > 0 {
            let _ = socket.delivered.send(size);
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for FakeSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let _ = self.writes.send(buf.to_vec());
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct FakeHost {
    gate: Option<oneshot::Sender<std::io::Result<()>>>,
    events: mpsc::UnboundedSender<HostEvent>,
    writes: mpsc::UnboundedReceiver<Vec<u8>>,
    delivered: mpsc::UnboundedReceiver<usize>,
}

impl FakeHost {
    pub fn accept(&mut self) {
        if let Some(gate) = self.gate.take() {
            let _ = gate.send(Ok(()));
        }
    }

    pub fn refuse(&mut self, kind: ErrorKind, message: &str) {
        if let Some(gate) = self.gate.take() {
            let _ = gate.send(Err(std::io::Error::new(kind, message.to_owned())));
        }
    }

    pub async fn next_write(&mut self) -> Vec<u8> {
        settle(self.writes.recv()).await.expect("the caller writes")
    }

    /// The next JSONL frame the caller wrote; a raw write (a secret) is skipped.
    pub async fn next_frame(&mut self) -> Value {
        loop {
            if let Ok(frame) = serde_json::from_slice::<Value>(&self.next_write().await) {
                return frame;
            }
        }
    }

    pub fn reply(&self, frame: &Value) {
        let response = json!({"id": frame["id"], "type": "response", "success": true});
        self.data(&format!("{response}\n"));
    }

    pub fn data(&self, text: &str) {
        let _ = self.events.send(HostEvent::Data(text.as_bytes().to_vec()));
    }

    /// Waits until the caller has read what the host sent so far.
    pub async fn delivered(&mut self) {
        settle(self.delivered.recv())
            .await
            .expect("the caller reads");
    }

    /// The host drops the connection: the caller's read fails with `kind`.
    pub fn drop_connection(&self, kind: ErrorKind, message: &str) {
        let _ = self.events.send(HostEvent::Drop(kind, message.to_owned()));
    }

    pub fn close(&self) {
        let _ = self.events.send(HostEvent::Close);
    }
}

pub struct Dials(mpsc::UnboundedReceiver<FakeHost>);

impl Dials {
    pub async fn next_dial(&mut self) -> FakeHost {
        settle(self.0.recv()).await.expect("the caller dials")
    }
}

pub struct World {
    pub clock: Arc<ManualClock>,
    pub timers: Arc<ManualTimers>,
    lines: Arc<Mutex<Vec<String>>>,
    dialed: Arc<AtomicUsize>,
    gated: bool,
    dial: mpsc::UnboundedSender<FakeHost>,
}

impl World {
    /// Every dial connects at once.
    pub fn open() -> (Self, Dials) {
        Self::build(false)
    }

    /// Every dial waits for its host to accept or refuse.
    pub fn gated() -> (Self, Dials) {
        Self::build(true)
    }

    fn build(gated: bool) -> (Self, Dials) {
        let (dial, dials) = mpsc::unbounded_channel();
        let world = Self {
            clock: ManualClock::at(1_000.0),
            timers: Arc::default(),
            lines: Arc::default(),
            dialed: Arc::default(),
            gated,
            dial,
        };
        (world, Dials(dials))
    }

    pub fn dialed(&self) -> usize {
        self.dialed.load(Ordering::SeqCst)
    }

    pub fn lines(&self) -> Vec<String> {
        lock(&self.lines).clone()
    }

    pub fn runtime(&self, read_trace: Option<&str>) -> RpcSocketRuntime {
        let lines = Arc::clone(&self.lines);
        RpcSocketRuntime {
            clock: self.clock.clone(),
            timers: self.timers.clone(),
            probe: self.clock.clone(),
            read_trace: read_trace.map(str::to_owned),
            log: Arc::new(move |line: &str| lock(&lines).push(line.to_owned())),
        }
    }

    pub fn connector(&self) -> RpcConnector {
        let dial = self.dial.clone();
        let dialed = Arc::clone(&self.dialed);
        let gated = self.gated;
        Arc::new(move |_endpoint: &str| -> ConnectFuture {
            dialed.fetch_add(1, Ordering::SeqCst);
            let (events_tx, events) = mpsc::unbounded_channel();
            let (writes, writes_rx) = mpsc::unbounded_channel();
            let (delivered, delivered_rx) = mpsc::unbounded_channel();
            let (gate, opened) = oneshot::channel();
            let socket = FakeSocket {
                events,
                writes,
                delivered,
                unread: Vec::new(),
                closed: false,
            };
            let _ = dial.send(FakeHost {
                gate: gated.then_some(gate),
                events: events_tx,
                writes: writes_rx,
                delivered: delivered_rx,
            });
            Box::pin(async move {
                if gated {
                    match opened.await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => return Err(error),
                        Err(_) => std::future::pending::<()>().await,
                    }
                }
                let socket: Box<dyn RpcStream> = Box::new(socket);
                Ok(socket)
            })
        })
    }

    pub fn options(&self, timeout_ms: u64, read_command: Option<&str>) -> RpcSocketOptions {
        RpcSocketOptions {
            timeout_ms,
            read_command: read_command.map(str::to_owned),
            connect: Some(self.connector()),
            runtime: Some(self.runtime(None)),
            ..RpcSocketOptions::default()
        }
    }
}
