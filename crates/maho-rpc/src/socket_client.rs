//! omo `runners/rpc-host/socket-client.ts`: correlated JSONL conversations on a local socket.
//!
//! [`RpcSocket`] is the v5.1.47 single-deadline conversation that `host_probe::authenticated_host_socket` hands to
//! the persistent mux. [`with_rpc_socket`] is v5.1.83's `withRpcSocket`: its deadline is the host's time, not the
//! caller's (omo#14444), measured by the caller-stall meter, and a read the host dropped during a caller stall is
//! dialed once more within the same budget (omo#14624). No Rust caller passes a read command yet (the thread
//! surface's `RpcTransport` has no socket implementation), so `tests/socket_client_*.rs` pin the rules.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::read_rpc_diagnostic::{
    READ_TRACE_ENV, ReadRpcDiagnostic, ReadRpcPhase, is_read_command, read_rpc_diagnostic,
};

#[cfg(unix)]
pub struct RpcSocket {
    reader: tokio::io::BufReader<tokio::net::UnixStream>,
    deadline: tokio::time::Instant,
}
#[cfg(unix)]
impl RpcSocket {
    pub async fn connect(endpoint: &str, timeout: std::time::Duration) -> std::io::Result<Self> {
        let deadline = tokio::time::Instant::now() + timeout;
        let stream =
            tokio::time::timeout_at(deadline, tokio::net::UnixStream::connect(endpoint)).await??;
        Ok(Self {
            reader: tokio::io::BufReader::new(stream),
            deadline,
        })
    }
    /// Transfer an authenticated, pre-session connection to the persistent mux.
    pub fn into_stream(self) -> std::io::Result<tokio::net::UnixStream> {
        if !self.reader.buffer().is_empty() {
            return Err(std::io::Error::other(
                "unexpected buffered host data at authentication handoff",
            ));
        }
        Ok(self.reader.into_inner())
    }
    pub async fn request(
        &mut self,
        mut command: serde_json::Value,
    ) -> std::io::Result<serde_json::Value> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let id = command
            .get("id")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(uuid::Uuid::new_v4().to_string()));
        command["id"] = id.clone();
        let deadline = self.deadline;
        tokio::time::timeout_at(deadline, async {
            let mut text = serde_json::to_vec(&command)?;
            text.push(b'\n');
            self.reader.get_mut().write_all(&text).await?;
            loop {
                let mut bytes = Vec::new();
                if self.reader.read_until(b'\n', &mut bytes).await? == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "host closed before response",
                    ));
                }
                let Ok(frame) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                    continue;
                };
                if frame.get("id") == Some(&id) {
                    return Ok(frame);
                }
            }
        })
        .await?
    }
}

/// How often the caller-stall meter looks at its clock.
pub const STALL_PROBE_MS: f64 = 50.0;
/// How late a look may be before the gap counts as a stall.
pub const STALL_TOLERANCE_MS: f64 = 50.0;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A connection a conversation reads and writes: a Unix socket in production, an in-memory double in tests.
pub trait RpcStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> RpcStream for T {}

pub type ConnectFuture = BoxFuture<std::io::Result<Box<dyn RpcStream>>>;
/// Opens a connection to an endpoint (upstream `options.connect`).
pub type RpcConnector = Arc<dyn Fn(&str) -> ConnectFuture + Send + Sync>;

/// Milliseconds on a monotonic clock (`performance.now()`).
pub trait MonotonicClock: Send + Sync {
    fn now_ms(&self) -> f64;
}

/// The one-shot timers a conversation arms (`setTimeout`).
pub trait ConversationTimers: Send + Sync {
    fn sleep(&self, delay_ms: f64) -> BoxFuture<()>;
}

pub type ProbeTick = Arc<dyn Fn() + Send + Sync>;

/// The stall meter's periodic look (`setInterval`): `tick` runs every `period_ms` until the guard drops.
pub trait StallProbe: Send + Sync {
    fn start(&self, period_ms: f64, tick: ProbeTick) -> ProbeGuard;
}

type StopProbe = Box<dyn FnOnce() + Send>;

/// Stops a running stall probe when dropped.
pub struct ProbeGuard(Option<StopProbe>);

impl ProbeGuard {
    pub fn new(stop: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(stop)))
    }
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(stop) = self.0.take() {
            stop();
        }
    }
}

pub type DiagnosticLog = Arc<dyn Fn(&str) + Send + Sync>;

/// The clock, timers, stall probe and trace switch one call runs on. A test hands in a clock it moves by hand, so
/// a deadline or a stall is exact instead of timed.
#[derive(Clone)]
pub struct RpcSocketRuntime {
    pub clock: Arc<dyn MonotonicClock>,
    pub timers: Arc<dyn ConversationTimers>,
    pub probe: Arc<dyn StallProbe>,
    /// The `OMO_THREAD_READ_TRACE` value this call reads.
    pub read_trace: Option<String>,
    pub log: DiagnosticLog,
}

impl RpcSocketRuntime {
    /// The process's monotonic clock, tokio timers, a probe task on the current runtime and
    /// `OMO_THREAD_READ_TRACE` from the environment; a traced failure goes to stderr.
    pub fn system() -> Self {
        Self {
            clock: Arc::new(SystemClock(std::time::Instant::now())),
            timers: Arc::new(TokioTimers),
            probe: Arc::new(TokioStallProbe),
            read_trace: std::env::var(READ_TRACE_ENV).ok(),
            log: Arc::new(|line: &str| eprintln!("{line}")),
        }
    }
}

struct SystemClock(std::time::Instant);

impl MonotonicClock for SystemClock {
    fn now_ms(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1000.0
    }
}

struct TokioTimers;

impl ConversationTimers for TokioTimers {
    fn sleep(&self, delay_ms: f64) -> BoxFuture<()> {
        let delay = std::time::Duration::from_millis(delay_ms.max(0.0).ceil() as u64);
        Box::pin(tokio::time::sleep(delay))
    }
}

struct TokioStallProbe;

impl StallProbe for TokioStallProbe {
    fn start(&self, period_ms: f64, tick: ProbeTick) -> ProbeGuard {
        let period = std::time::Duration::from_millis(period_ms as u64);
        let task = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            // Node runs a late setInterval callback once; a burst of catch-up looks would read as the caller
            // running on time again.
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                tick();
            }
        });
        ProbeGuard::new(move || task.abort())
    }
}

/// Why a conversation failed. Every variant but [`RpcSocketError::Operation`] is a transport failure the socket saw.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcSocketError {
    #[error("thread RPC request timed out")]
    TimedOut,
    #[error("thread RPC connection closed before the response arrived")]
    Closed,
    #[error("{message}")]
    Io {
        kind: std::io::ErrorKind,
        message: String,
    },
    /// The operation's own failure: never traced and never redialed.
    #[error("{0}")]
    Operation(String),
}

impl RpcSocketError {
    fn is_transport(&self) -> bool {
        !matches!(self, Self::Operation(_))
    }

    /// The errno name Node reports in `error.code` for a socket error.
    pub fn errno(&self) -> Option<&'static str> {
        match self {
            Self::Io { kind, .. } => errno_name(*kind),
            Self::TimedOut | Self::Closed | Self::Operation(_) => None,
        }
    }
}

impl From<&std::io::Error> for RpcSocketError {
    fn from(error: &std::io::Error) -> Self {
        Self::Io {
            kind: error.kind(),
            message: error.to_string(),
        }
    }
}

fn errno_name(kind: std::io::ErrorKind) -> Option<&'static str> {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::BrokenPipe => Some("EPIPE"),
        ErrorKind::ConnectionReset => Some("ECONNRESET"),
        ErrorKind::ConnectionRefused => Some("ECONNREFUSED"),
        ErrorKind::NotFound => Some("ENOENT"),
        ErrorKind::TimedOut => Some("ETIMEDOUT"),
        ErrorKind::PermissionDenied => Some("EACCES"),
        _ => None,
    }
}

/// The socket errors that mean the host dropped the connection (omo#14624 review M2). Anything else (a permission
/// error, a refused connect) is no evidence that the host cut the handshake, so it is never redialed.
fn is_host_drop(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
    )
}

pub type FrameListener = Arc<dyn Fn(Value) + Send + Sync>;
pub type ClosedListener = Arc<dyn Fn(&RpcSocketError) + Send + Sync>;

/// A subscription's receivers (upstream `options.listen`): every frame that is not the pending reply, and once, the
/// failure that ended the connection.
#[derive(Clone)]
pub struct RpcListen {
    pub frame: FrameListener,
    pub closed: ClosedListener,
}

/// One call's options (upstream `withRpcSocket` options).
#[derive(Clone, Default)]
pub struct RpcSocketOptions {
    /// The whole call's budget: connect, handshake and every request, a redial included.
    pub timeout_ms: u64,
    /// The command type a read call sends (`list_sessions`, `get_messages`, ...): it names the trace line and makes
    /// a dropped read redialable.
    pub read_command: Option<String>,
    /// Written before the first request.
    pub secret: Option<Vec<u8>>,
    /// Defaults to a Unix socket connect.
    pub connect: Option<RpcConnector>,
    pub listen: Option<RpcListen>,
    /// Defaults to [`RpcSocketRuntime::system`].
    pub runtime: Option<RpcSocketRuntime>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct MeterState {
    stalled: f64,
    last: f64,
    stall_count: u32,
    recovered: bool,
    last_probe: f64,
    probe: Option<ProbeGuard>,
}

impl MeterState {
    /// Records the gap since the last look: a look later than its period plus the tolerance adds the excess.
    fn look(&mut self, now: f64, probe_tick: bool) -> f64 {
        let excess = (now - self.last - STALL_PROBE_MS - STALL_TOLERANCE_MS).max(0.0);
        if excess > 0.0 {
            self.stalled += excess;
            self.stall_count += 1;
            self.recovered = false;
        }
        if probe_tick {
            // Only a probe on time proves the caller runs again, and on time is measured from the previous probe:
            // an overdue probe right after another clock read (a request arming its timer) is not one
            // (omo#14624 review M5).
            if excess <= 0.0 && now - self.last_probe <= STALL_PROBE_MS + STALL_TOLERANCE_MS {
                self.recovered = true;
            }
            self.last_probe = now;
        }
        self.last = now;
        self.stalled
    }
}

/// The milliseconds this process could not run while a conversation waited on the clock (upstream
/// `callerStallMeter`); a conversation's deadline moves by exactly that much.
struct CallerStallMeter {
    clock: Arc<dyn MonotonicClock>,
    scheduler: Arc<dyn StallProbe>,
    state: Arc<Mutex<MeterState>>,
}

impl CallerStallMeter {
    fn new(clock: Arc<dyn MonotonicClock>, scheduler: Arc<dyn StallProbe>) -> Self {
        let now = clock.now_ms();
        let meter = Self {
            clock,
            scheduler,
            state: Arc::new(Mutex::new(MeterState {
                stalled: 0.0,
                last: now,
                stall_count: 0,
                recovered: true,
                last_probe: now,
                probe: None,
            })),
        };
        meter.resume();
        meter
    }

    /// The stall so far, counting the gap up to now while the probe runs.
    fn total(&self) -> f64 {
        let now = self.clock.now_ms();
        let mut state = lock(&self.state);
        if state.probe.is_none() {
            return state.stalled;
        }
        state.look(now, false)
    }

    /// How many looks found a stall so far, counting the gap up to now.
    fn stalls(&self) -> u32 {
        let now = self.clock.now_ms();
        let mut state = lock(&self.state);
        if state.probe.is_some() {
            state.look(now, false);
        }
        state.stall_count
    }

    /// Whether an on-time probe ran after the last stall: the caller has been running again since.
    fn recovered(&self) -> bool {
        lock(&self.state).recovered
    }

    /// Stops looking while nothing waits on the clock (a subscription between requests); that gap is never counted.
    fn pause(&self) {
        let now = self.clock.now_ms();
        let mut state = lock(&self.state);
        if state.probe.is_none() {
            return;
        }
        state.look(now, false);
        state.probe = None;
    }

    fn resume(&self) {
        let now = self.clock.now_ms();
        let mut state = lock(&self.state);
        if state.probe.is_some() {
            return;
        }
        state.last = now;
        state.last_probe = now;
        let probed = Arc::clone(&self.state);
        let clock = Arc::clone(&self.clock);
        let tick: ProbeTick = Arc::new(move || {
            let now = clock.now_ms();
            let mut state = lock(&probed);
            if state.probe.is_some() {
                state.look(now, true);
            }
        });
        state.probe = Some(self.scheduler.start(STALL_PROBE_MS, tick));
    }
}

/// Stops the call's stall probe however the call ends, a dropped future included.
struct PauseMeter(Arc<CallerStallMeter>);

impl Drop for PauseMeter {
    fn drop(&mut self) {
        self.0.pause();
    }
}

#[derive(Default)]
struct ConversationState {
    pending: Option<PendingReply>,
    failure: Option<RpcSocketError>,
    answered: bool,
    wrote: bool,
    lost_during_stall: bool,
}

struct PendingReply {
    id: Value,
    reply: tokio::sync::oneshot::Sender<Result<Value, RpcSocketError>>,
}

struct Conversation {
    state: Mutex<ConversationState>,
    meter: Arc<CallerStallMeter>,
    clock: Arc<dyn MonotonicClock>,
    timers: Arc<dyn ConversationTimers>,
    listen: Option<RpcListen>,
    diagnostic: Option<Arc<ReadRpcDiagnostic>>,
    read_command: Option<String>,
    stalls_at_dial: u32,
    deadline: f64,
}

#[derive(Clone, Copy)]
struct Step {
    armed_at: f64,
    stalled_at_arm: f64,
    timeout_ms: f64,
}

/// One step's wait (connect, or one request), with its first timer already armed.
struct StepDeadline {
    conversation: Arc<Conversation>,
    step: Step,
    first: BoxFuture<()>,
}

impl StepDeadline {
    /// Completes once the step is due on the host's clock; a timer that fires before that because a stall moved the
    /// due time waits again.
    async fn expire(self) {
        let Self {
            conversation,
            step,
            first,
        } = self;
        first.await;
        while conversation.due(step) > conversation.clock.now_ms() {
            let delay = (conversation.due(step) - conversation.clock.now_ms()).max(0.0);
            conversation.timers.sleep(delay).await;
        }
    }
}

struct AttemptFailure {
    error: RpcSocketError,
    transport: bool,
    lost_during_stall: bool,
}

impl Conversation {
    fn state(&self) -> MutexGuard<'_, ConversationState> {
        lock(&self.state)
    }

    fn failure(&self) -> Option<RpcSocketError> {
        self.state().failure.clone()
    }

    fn at(&self, phase: ReadRpcPhase) {
        if let Some(diagnostic) = &self.diagnostic {
            diagnostic.at(phase);
        }
    }

    /// The call's deadline, or the step's own budget when that is shorter, both moved by every stall since.
    fn due(&self, step: Step) -> f64 {
        let stalled = self.meter.total();
        (self.deadline + stalled)
            .min(step.armed_at + step.timeout_ms + stalled - step.stalled_at_arm)
    }

    fn arm(self: &Arc<Self>, timeout_ms: f64) -> StepDeadline {
        self.meter.resume();
        let armed_at = self.clock.now_ms();
        let stalled_at_arm = self.meter.total();
        let step = Step {
            armed_at,
            stalled_at_arm,
            timeout_ms,
        };
        let delay = (self.due(step) - self.clock.now_ms()).max(0.0);
        StepDeadline {
            conversation: Arc::clone(self),
            step,
            first: self.timers.sleep(delay),
        }
    }

    /// Records the connection's first failure. A host drop before any response byte is the stall's only if a stall
    /// began after the dial and the caller had written nothing yet or had not run on time again since
    /// (omo#14624 review M5); a deadline is never the stall's.
    fn fail(&self, error: RpcSocketError, dropped_by_host: bool) {
        let pending = {
            let mut state = self.state();
            if state.failure.is_some() {
                return;
            }
            if dropped_by_host && !state.answered {
                let stalled = self.meter.stalls() > self.stalls_at_dial;
                state.lost_during_stall = stalled && !(state.wrote && self.meter.recovered());
            }
            state.failure = Some(error.clone());
            state.pending.take()
        };
        if let Some(listen) = &self.listen {
            (listen.closed)(&error);
        }
        if let Some(pending) = pending {
            let _ = pending.reply.send(Err(error));
        }
    }

    fn attempt_failure(&self, error: RpcSocketError) -> AttemptFailure {
        let transport = error.is_transport();
        let lost_during_stall = transport && self.state().lost_during_stall;
        AttemptFailure {
            error,
            transport,
            lost_during_stall,
        }
    }

    fn dispatch(&self, line: &[u8]) {
        let Ok(text) = std::str::from_utf8(line) else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        // A line that is not JSON (stray host output) is never the reply; it is skipped like a broadcast.
        let Ok(frame) = serde_json::from_str::<Value>(text) else {
            return;
        };
        if !(frame.is_object() || frame.is_array()) {
            return;
        }
        let waiter = {
            let mut state = self.state();
            let answers = state
                .pending
                .as_ref()
                .is_some_and(|pending| frame.get("id") == Some(&pending.id));
            if answers { state.pending.take() } else { None }
        };
        match waiter {
            Some(waiter) => {
                // A subscription between requests waits only for frames, so nothing needs the clock
                // (omo#14624 review L2).
                if self.listen.is_some() {
                    self.meter.pause();
                }
                let _ = waiter.reply.send(Ok(frame));
            }
            None => {
                if let Some(listen) = &self.listen {
                    (listen.frame)(frame);
                }
            }
        }
    }
}

async fn pump_frames(
    conversation: Arc<Conversation>,
    mut reader: tokio::io::ReadHalf<Box<dyn RpcStream>>,
) -> std::convert::Infallible {
    use tokio::io::AsyncReadExt;
    let mut buffer = Vec::new();
    let mut chunk = vec![0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await;
        if conversation.failure().is_some() {
            break;
        }
        match read {
            Ok(0) => {
                conversation.fail(RpcSocketError::Closed, true);
                break;
            }
            Ok(size) => {
                conversation.state().answered = true;
                conversation.at(ReadRpcPhase::ResponseBytes);
                buffer.extend_from_slice(&chunk[..size]);
                while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                    conversation.at(ReadRpcPhase::ResponseFrame);
                    let line: Vec<u8> = buffer.drain(..=newline).collect();
                    conversation.dispatch(&line[..newline]);
                }
            }
            Err(error) => {
                conversation.fail(RpcSocketError::from(&error), is_host_drop(error.kind()));
                break;
            }
        }
    }
    std::future::pending().await
}

/// Sends requests on one connection (upstream `RpcRequest`); the operation receives one per attempt.
pub struct RpcRequest {
    conversation: Arc<Conversation>,
    writer: tokio::io::WriteHalf<Box<dyn RpcStream>>,
    timeout_ms: u64,
}

impl RpcRequest {
    /// Writes `command` with an `id` (its own, or a fresh UUID) and waits for the frame that carries it.
    /// `timeout_ms` can only shorten what is left of the call's budget.
    pub async fn send(
        &mut self,
        mut command: Value,
        timeout_ms: Option<u64>,
    ) -> Result<Value, RpcSocketError> {
        use tokio::io::AsyncWriteExt;
        let conversation = Arc::clone(&self.conversation);
        let id = command
            .get("id")
            .filter(|id| !id.is_null())
            .cloned()
            .unwrap_or_else(|| Value::String(uuid::Uuid::new_v4().to_string()));
        if let Some(fields) = command.as_object_mut() {
            fields.insert("id".to_owned(), id.clone());
        }
        let mut line = serde_json::to_vec(&command)
            .map_err(|error| RpcSocketError::Operation(error.to_string()))?;
        line.push(b'\n');
        let (reply, replied) = tokio::sync::oneshot::channel();
        {
            let mut state = conversation.state();
            if let Some(failure) = &state.failure {
                return Err(failure.clone());
            }
            state.pending = Some(PendingReply { id, reply });
        }
        let deadline = conversation.arm(timeout_ms.unwrap_or(self.timeout_ms) as f64);
        let request = command
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| conversation.read_command.as_deref() == Some(kind));
        conversation.at(if request {
            ReadRpcPhase::Request
        } else {
            ReadRpcPhase::Handshake
        });
        conversation.state().wrote = true;
        if let Err(error) = self.writer.write_all(&line).await {
            conversation.fail(RpcSocketError::from(&error), is_host_drop(error.kind()));
        }
        tokio::select! {
            biased;
            answer = replied => match answer {
                Ok(result) => result,
                Err(_dropped) => Err(conversation.failure().unwrap_or(RpcSocketError::Closed)),
            },
            () = deadline.expire() => {
                conversation.fail(RpcSocketError::TimedOut, false);
                Err(conversation.failure().unwrap_or(RpcSocketError::TimedOut))
            }
        }
    }
}

struct Call<'a> {
    endpoint: &'a str,
    options: &'a RpcSocketOptions,
    runtime: RpcSocketRuntime,
    connect: RpcConnector,
    meter: Arc<CallerStallMeter>,
    diagnostic: Option<Arc<ReadRpcDiagnostic>>,
    deadline: f64,
}

/// A correlated JSONL conversation (upstream `withRpcSocket`): broadcasts never settle a request and the call owns
/// the socket. `listen` receives every other frame and, once, the failure that ended the connection.
///
/// The deadline is the host's time, not the caller's (omo#14444): while this process cannot run, no host can be
/// heard, so that time is never charged to the host. A read (`read_command` names a read and nothing listens) whose
/// connection the host dropped (ECONNRESET, EPIPE or a close) before any response byte, after a stall that began
/// after the dial, while the caller had written nothing yet or had not run on time again, is dialed once more with
/// what is left of the one budget (omo#14624 review M1). Nothing else is retried.
pub async fn with_rpc_socket<T, Op, Fut>(
    endpoint: &str,
    options: RpcSocketOptions,
    mut operation: Op,
) -> Result<T, RpcSocketError>
where
    Op: FnMut(RpcRequest) -> Fut,
    Fut: Future<Output = Result<T, RpcSocketError>>,
{
    let runtime = options
        .runtime
        .clone()
        .unwrap_or_else(RpcSocketRuntime::system);
    let diagnostic = read_rpc_diagnostic(
        runtime.read_trace.as_deref(),
        options.read_command.as_deref(),
        runtime.clock.now_ms(),
    )
    .map(Arc::new);
    let redial = options.listen.is_none() && is_read_command(options.read_command.as_deref());
    let meter = Arc::new(CallerStallMeter::new(
        Arc::clone(&runtime.clock),
        Arc::clone(&runtime.probe),
    ));
    let _pause = PauseMeter(Arc::clone(&meter));
    let deadline = runtime.clock.now_ms() + options.timeout_ms as f64;
    let connect = options.connect.clone().unwrap_or_else(default_connector);
    let call = Call {
        endpoint,
        options: &options,
        runtime,
        connect,
        meter,
        diagnostic,
        deadline,
    };
    let mut attempt = 1_u32;
    loop {
        let failure = match converse(&call, &mut operation).await {
            Ok(value) => return Ok(value),
            Err(failure) => failure,
        };
        // A reset handled before an already-due timeout must not open a second socket once the budget is gone.
        let budget_left = call.runtime.clock.now_ms() < call.deadline + call.meter.total();
        if redial && attempt == 1 && failure.lost_during_stall && budget_left {
            if let Some(diagnostic) = &call.diagnostic {
                diagnostic.redialed();
            }
            attempt += 1;
            continue;
        }
        if failure.transport
            && let Some(diagnostic) = &call.diagnostic
        {
            let line = diagnostic.failed(
                failure.error.errno(),
                matches!(failure.error, RpcSocketError::TimedOut),
                call.meter.total(),
                call.runtime.clock.now_ms(),
            );
            (call.runtime.log)(&line);
        }
        return Err(failure.error);
    }
}

async fn converse<T, Op, Fut>(call: &Call<'_>, operation: &mut Op) -> Result<T, AttemptFailure>
where
    Op: FnMut(RpcRequest) -> Fut,
    Fut: Future<Output = Result<T, RpcSocketError>>,
{
    use tokio::io::AsyncWriteExt;
    let conversation = Arc::new(Conversation {
        state: Mutex::default(),
        meter: Arc::clone(&call.meter),
        clock: Arc::clone(&call.runtime.clock),
        timers: Arc::clone(&call.runtime.timers),
        listen: call.options.listen.clone(),
        diagnostic: call.diagnostic.clone(),
        read_command: call.options.read_command.clone(),
        stalls_at_dial: call.meter.stalls(),
        deadline: call.deadline,
    });
    let connecting = (call.connect)(call.endpoint);
    let deadline = conversation.arm(call.options.timeout_ms as f64);
    let mut stream = tokio::select! {
        biased;
        connected = connecting => match connected {
            Ok(stream) => stream,
            Err(error) => {
                let failure = RpcSocketError::from(&error);
                conversation.fail(failure.clone(), is_host_drop(error.kind()));
                return Err(conversation.attempt_failure(failure));
            }
        },
        () = deadline.expire() => {
            conversation.fail(RpcSocketError::TimedOut, false);
            return Err(conversation.attempt_failure(RpcSocketError::TimedOut));
        }
    };
    conversation.at(if call.options.secret.is_some() {
        ReadRpcPhase::Handshake
    } else {
        ReadRpcPhase::Connected
    });
    if let Some(secret) = &call.options.secret {
        conversation.state().wrote = true;
        if let Err(error) = stream.write_all(secret).await {
            conversation.fail(RpcSocketError::from(&error), is_host_drop(error.kind()));
        }
    }
    let (reader, writer) = tokio::io::split(stream);
    let request = RpcRequest {
        conversation: Arc::clone(&conversation),
        writer,
        timeout_ms: call.options.timeout_ms,
    };
    let outcome = tokio::select! {
        biased;
        outcome = operation(request) => outcome,
        never = pump_frames(Arc::clone(&conversation), reader) => match never {},
    };
    outcome.map_err(|error| conversation.attempt_failure(error))
}

fn default_connector() -> RpcConnector {
    Arc::new(connect_local_socket)
}

#[cfg(unix)]
fn connect_local_socket(endpoint: &str) -> ConnectFuture {
    let endpoint = endpoint.to_owned();
    Box::pin(async move {
        let stream = tokio::net::UnixStream::connect(endpoint).await?;
        let stream: Box<dyn RpcStream> = Box::new(stream);
        Ok::<_, std::io::Error>(stream)
    })
}

#[cfg(not(unix))]
fn connect_local_socket(_endpoint: &str) -> ConnectFuture {
    Box::pin(async {
        Err::<Box<dyn RpcStream>, _>(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a local RPC socket needs a Unix socket on this platform",
        ))
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn split_utf8_and_stray_output_do_not_corrupt_correlated_reply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let serve = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let command: serde_json::Value = serde_json::from_str(&line).unwrap();
            let response =
                serde_json::json!({"id":command["id"],"success":true,"data":{"text":"日本語"}})
                    .to_string()
                    + "\n";
            stream
                .get_mut()
                .write_all(b"stray output\n{\"id\":\"other\"}\n")
                .await
                .unwrap();
            for byte in response.as_bytes() {
                stream.get_mut().write_all(&[*byte]).await.unwrap();
            }
        });
        let mut socket =
            RpcSocket::connect(path.to_str().unwrap(), std::time::Duration::from_secs(5))
                .await
                .unwrap();
        let response = socket
            .request(serde_json::json!({"type":"get_state"}))
            .await
            .unwrap();
        assert_eq!(response["data"]["text"], "日本語");
        serve.await.unwrap();
    }
}
