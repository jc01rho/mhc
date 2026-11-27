//! One long-lived agentd connection per device: hello/resume, commands answered by id, events resumed by seq.
//!
//! The link owns a runtime thread of its own, so a caller may poll its futures from any executor: mhc's agent loop
//! runs on tokio, while a gateway test may block on a plain one. A command still unanswered when a connection drops is
//! sent again with the same id after the reconnect; the daemon remembers an id's result, so the resend answers from
//! that record instead of running the op twice.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot};

use crate::thread::adapter::BoxFuture;

use super::frame::{
    EventFrame, ServerFrame, ack_frame, command_frame, hello_frame, parse_line, ping_frame,
    pong_frame,
};
use super::peer_config::{AgentdPeer, read_token};

/// How long one command waits for its answer unless the call asks otherwise.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a connection has to answer the hello before it is dropped and retried.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The first wait between reconnect attempts, doubling to MAX_BACKOFF.
const FIRST_BACKOFF: Duration = Duration::from_millis(50);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// How often a connected link pings its daemon and sweeps expired calls.
const TICK: Duration = Duration::from_secs(5);

/// The link stopped because its inbox closed: nothing is left to serve.
const STOPPED: &str = "the agentd link stopped";

/// How a call to a device's agentd ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentdError {
    /// No connection could be made, or the link stopped: the device is not reachable right now.
    Unavailable(String),
    /// The daemon answered ok:false with a typed reason (fork_source_not_found, session_busy, ...).
    Refused {
        reason: String,
        detail: Option<String>,
    },
    /// The daemon does not serve this op (unsupported:<op>).
    Unsupported(String),
    /// The command's expiresAt passed before the daemon could run it.
    Expired,
    /// No answer arrived inside the call's own bound.
    Timeout,
    /// The daemon sent something this client cannot read.
    Protocol(String),
}

impl AgentdError {
    /// The daemon's own refusal text, for a caller that maps it onto a thread tool failure.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Refused { reason, .. } => Some(reason),
            _ => None,
        }
    }

    /// A short line naming what happened, for a transport failure's message.
    pub fn describe(&self) -> String {
        match self {
            Self::Unavailable(reason) => format!("agentd is unreachable: {reason}"),
            Self::Refused { reason, detail } => match detail {
                Some(detail) => format!("agentd refused: {reason} ({detail})"),
                None => format!("agentd refused: {reason}"),
            },
            Self::Unsupported(op) => format!("agentd does not serve {op}"),
            Self::Expired => "the agentd command expired before it ran".to_owned(),
            Self::Timeout => "agentd did not answer in time".to_owned(),
            Self::Protocol(reason) => format!("agentd answered outside the protocol: {reason}"),
        }
    }
}

/// One unanswered command: the caller waiting for it, and when it stops waiting.
struct Waiter {
    sender: Option<oneshot::Sender<Result<Value, AgentdError>>>,
    deadline: Instant,
}

/// What the connection thread and every caller share.
struct Shared {
    next_id: u64,
    waiters: BTreeMap<String, Waiter>,
    /// Commands sent on a connection that dropped before answering them, resent on the next one under the same id.
    resend: Vec<Value>,
    /// The last seq this side kept per stream, sent as resume on every hello.
    cursors: BTreeMap<String, u64>,
    /// Every live subscriber of a stream, keyed by the stream's exact name.
    subscribers: BTreeMap<String, Vec<mpsc::UnboundedSender<EventFrame>>>,
    connected: bool,
    node: Option<String>,
}

impl Shared {
    fn subscribe_names(&self) -> Vec<String> {
        self.subscribers.keys().cloned().collect()
    }

    /// Records how far a stream has been read and returns the whole cursor to acknowledge.
    fn keep(&mut self, stream: &str, seq: u64) -> BTreeMap<String, u64> {
        let cursor = self.cursors.entry(stream.to_owned()).or_insert(0);
        if *cursor < seq {
            *cursor = seq;
        }
        self.cursors.clone()
    }
}

enum Command {
    Send(Value),
}

/// One device's agentd, as this process reaches it.
pub struct AgentdLink {
    peer: AgentdPeer,
    node_name: String,
    shared: Mutex<Shared>,
    outbox: mpsc::UnboundedSender<Command>,
    ping_every: Duration,
    first_backoff: Duration,
}

impl AgentdLink {
    /// Opens the link and starts its runtime thread; the first connection is attempted at once.
    pub fn new(peer: AgentdPeer, node_name: impl Into<String>) -> Arc<Self> {
        Self::with_timing(peer, node_name, TICK, FIRST_BACKOFF)
    }

    fn with_timing(
        peer: AgentdPeer,
        node_name: impl Into<String>,
        ping_every: Duration,
        first_backoff: Duration,
    ) -> Arc<Self> {
        let (outbox, inbox) = mpsc::unbounded_channel();
        let link = Arc::new(Self {
            peer,
            node_name: node_name.into(),
            shared: Mutex::new(Shared {
                next_id: 1,
                waiters: BTreeMap::new(),
                resend: Vec::new(),
                cursors: BTreeMap::new(),
                subscribers: BTreeMap::new(),
                connected: false,
                node: None,
            }),
            outbox,
            ping_every,
            first_backoff,
        });
        let running = Arc::clone(&link);
        let name = format!("agentd-{}", link.peer.device);
        std::thread::Builder::new()
            .name(name)
            .spawn(move || running.serve(inbox))
            .ok();
        link
    }

    pub fn device(&self) -> &str {
        &self.peer.device
    }

    pub fn peer(&self) -> &AgentdPeer {
        &self.peer
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the link holds a connection right now (read from the link; no round trip).
    pub fn connected(&self) -> bool {
        self.lock().connected
    }

    /// The daemon's node name from its welcome, once one arrived.
    pub fn daemon_node(&self) -> Option<String> {
        self.lock().node.clone()
    }

    /// One command, answered by its own id; timeout bounds both the daemon's run (expiresAt) and this wait.
    pub fn call(
        &self,
        op: &str,
        args: Value,
        timeout: Duration,
    ) -> BoxFuture<Result<Value, AgentdError>> {
        let (id, receiver) = {
            let mut shared = self.lock();
            let id = format!("{}-{}", self.node_name, shared.next_id);
            shared.next_id += 1;
            let (sender, receiver) = oneshot::channel();
            shared.waiters.insert(
                id.clone(),
                Waiter {
                    sender: Some(sender),
                    deadline: Instant::now() + timeout,
                },
            );
            (id, receiver)
        };
        let expires_at = now_ms() + i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX);
        let frame = command_frame(&id, op, &args, Some(expires_at));
        if self.outbox.send(Command::Send(frame)).is_err() {
            self.lock().waiters.remove(&id);
            return Box::pin(async {
                Err(AgentdError::Unavailable(
                    "the agentd link is not running".to_owned(),
                ))
            });
        }
        Box::pin(async move {
            match tokio::time::timeout(timeout, receiver).await {
                Ok(result) => result.unwrap_or_else(|_| {
                    Err(AgentdError::Unavailable(
                        "the agentd link closed before it answered".to_owned(),
                    ))
                }),
                // A link that never completes its handshake never ticks the sweep: the call's own bound ends it.
                Err(_) => Err(AgentdError::Timeout),
            }
        })
    }

    /// Subscribes this connection to a stream from from_seq (the stream's own numbering); dropping the receiver
    /// releases the subscription, so the next hello no longer asks for that stream.
    pub fn follow(&self, stream: &str, from_seq: u64) -> mpsc::UnboundedReceiver<EventFrame> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut shared = self.lock();
        shared
            .subscribers
            .entry(stream.to_owned())
            .or_default()
            .push(sender);
        let cursor = shared.cursors.entry(stream.to_owned()).or_insert(0);
        if *cursor < from_seq {
            *cursor = from_seq;
        }
        receiver
    }

    /// The last seq kept for a stream: what a resuming connection asks the daemon to continue from.
    pub fn cursor_of(&self, stream: &str) -> Option<u64> {
        self.lock().cursors.get(stream).copied()
    }

    /// The number of live subscribers of a stream.
    pub fn subscriber_count(&self, stream: &str) -> usize {
        self.lock()
            .subscribers
            .get(stream)
            .map(Vec::len)
            .unwrap_or_default()
    }

    /// Forgets a stream: no later hello asks for it, and its cursor leaves with it (a closed thread is not resumed).
    pub fn release_stream(&self, stream: &str) {
        let mut shared = self.lock();
        shared.subscribers.remove(stream);
        shared.cursors.remove(stream);
    }

    /// The runtime thread: connect, serve, reconnect with the same resume cursors, until the inbox closes.
    fn serve(self: Arc<Self>, mut inbox: mpsc::UnboundedReceiver<Command>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            self.fail_all("the agentd link has no runtime");
            return;
        };
        runtime.block_on(self.clone().run(&mut inbox));
    }

    async fn run(self: Arc<Self>, inbox: &mut mpsc::UnboundedReceiver<Command>) {
        let mut backoff = self.first_backoff;
        loop {
            let reason = self.serve_connection(inbox).await;
            self.lock().connected = false;
            if reason == STOPPED {
                self.fail_all(STOPPED);
                return;
            }
            // A command sent on the lost connection is resent under its id, so its waiter keeps waiting.
            self.keep_for_resend();
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Keeps only the commands whose caller still waits, for the next connection.
    fn keep_for_resend(&self) {
        let mut shared = self.lock();
        let sent = std::mem::take(&mut shared.resend);
        let mut keep = Vec::new();
        for frame in sent {
            let id = frame.get("id").and_then(Value::as_str).unwrap_or_default();
            if shared.waiters.contains_key(id) {
                keep.push(frame);
            }
        }
        shared.resend = keep;
    }

    fn fail_all(&self, reason: &str) {
        let mut shared = self.lock();
        for (_, waiter) in std::mem::take(&mut shared.waiters) {
            if let Some(sender) = waiter.sender {
                let _ = sender.send(Err(AgentdError::Unavailable(reason.to_owned())));
            }
        }
    }

    /// Answers every call past its deadline, so a daemon that never answers cannot hold a caller forever.
    fn sweep_expired(&self) {
        let now = Instant::now();
        let mut shared = self.lock();
        let expired: Vec<String> = shared
            .waiters
            .iter()
            .filter(|(_, waiter)| waiter.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(waiter) = shared.waiters.remove(&id)
                && let Some(sender) = waiter.sender
            {
                let _ = sender.send(Err(AgentdError::Timeout));
            }
        }
        let sent = std::mem::take(&mut shared.resend);
        let mut keep = Vec::new();
        for frame in sent {
            let id = frame.get("id").and_then(Value::as_str).unwrap_or_default();
            if shared.waiters.contains_key(id) {
                keep.push(frame);
            }
        }
        shared.resend = keep;
    }

    async fn serve_connection(
        self: &Arc<Self>,
        inbox: &mut mpsc::UnboundedReceiver<Command>,
    ) -> String {
        let token = match read_token(&self.peer) {
            Ok(token) => token,
            Err(reason) => return reason,
        };
        let address = self.peer.socket_address();
        let stream =
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(&address)).await {
                Err(_) => return format!("{address}: no connection in time"),
                Ok(Err(error)) => return format!("{address}: {error}"),
                Ok(Ok(stream)) => stream,
            };
        let (read_half, mut write_half) = stream.into_split();
        let (resume, subscribe) = {
            let shared = self.lock();
            (shared.cursors.clone(), shared.subscribe_names())
        };
        let hello = hello_frame(
            &self.node_name,
            &token,
            &resume,
            (!subscribe.is_empty()).then_some(subscribe.as_slice()),
        );
        if let Err(reason) = write_frame(&mut write_half, &hello).await {
            return reason;
        }

        let mut reader = BufReader::new(read_half);
        let mut line: Vec<u8> = Vec::new();
        match read_welcome(&mut reader, &mut line).await {
            Ok(node) => {
                let mut shared = self.lock();
                shared.connected = true;
                shared.node = Some(node);
            }
            Err(reason) => return reason,
        }

        // Everything sent on the lost connection goes out again, in the order it was sent.
        let resend = std::mem::take(&mut self.lock().resend);
        for frame in resend {
            if let Err(reason) = write_frame(&mut write_half, &frame).await {
                return reason;
            }
            self.lock().resend.push(frame);
        }

        let mut ticker = tokio::time::interval(self.ping_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut ping = 0i64;

        loop {
            tokio::select! {
                biased;
                incoming = inbox.recv() => {
                    let Some(Command::Send(frame)) = incoming else {
                        return STOPPED.to_owned();
                    };
                    if let Err(reason) = write_frame(&mut write_half, &frame).await {
                        return reason;
                    }
                    self.lock().resend.push(frame);
                }
                _ = ticker.tick() => {
                    self.sweep_expired();
                    ping += 1;
                    if let Err(reason) = write_frame(&mut write_half, &ping_frame(ping, now_ms())).await {
                        return reason;
                    }
                }
                read = reader.read_until(b'\n', &mut line) => {
                    match read {
                        Err(error) => return error.to_string(),
                        Ok(0) => return "the daemon closed the connection".to_owned(),
                        Ok(_) => {}
                    }
                    let text = String::from_utf8_lossy(&line).into_owned();
                    line.clear();
                    let frame = match parse_line(&text) {
                        Ok(frame) => frame,
                        Err(error) => return error.to_string(),
                    };
                    if let Some(reason) = self.handle_frame(frame, &mut write_half).await {
                        return reason;
                    }
                }
            }
        }
    }

    /// One server frame. Some(reason) ends the connection.
    async fn handle_frame(
        self: &Arc<Self>,
        frame: ServerFrame,
        write_half: &mut OwnedWriteHalf,
    ) -> Option<String> {
        match frame {
            ServerFrame::Result(result) => {
                let waiter = self.lock().waiters.remove(&result.id);
                let waiter = waiter?;
                let outcome = if result.ok {
                    Ok(result.data.unwrap_or(Value::Null))
                } else {
                    Err(classify_error(result.error.as_deref().unwrap_or_default()))
                };
                if let Some(sender) = waiter.sender {
                    let _ = sender.send(outcome);
                }
                None
            }
            ServerFrame::Event(event) => {
                let (cursor, receivers) = {
                    let mut shared = self.lock();
                    let cursor = shared.keep(&event.stream, event.last_seq());
                    let receivers = shared
                        .subscribers
                        .get(&event.stream)
                        .cloned()
                        .unwrap_or_default();
                    (cursor, receivers)
                };
                for receiver in &receivers {
                    let _ = receiver.send(event.clone());
                }
                if !receivers.is_empty() {
                    let mut shared = self.lock();
                    if let Some(subscribers) = shared.subscribers.get_mut(&event.stream) {
                        subscribers.retain(|sender| !sender.is_closed());
                        if subscribers.is_empty() {
                            shared.subscribers.remove(&event.stream);
                        }
                    }
                }
                write_frame(write_half, &ack_frame(&cursor)).await.err()
            }
            ServerFrame::Stream { stream, from } => {
                let mut shared = self.lock();
                let cursor = shared.cursors.entry(stream).or_insert(0);
                if *cursor >= from {
                    *cursor = from.saturating_sub(1);
                }
                None
            }
            ServerFrame::Ping { n, at } => write_frame(write_half, &pong_frame(n, at)).await.err(),
            ServerFrame::Pong { .. } => None,
            ServerFrame::Refused { reason } => Some(reason),
            ServerFrame::Welcome(_) => None,
        }
    }
}

/// The daemon's welcome, or the reason this connection is not usable.
async fn read_welcome(
    reader: &mut BufReader<OwnedReadHalf>,
    line: &mut Vec<u8>,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        match tokio::time::timeout_at(deadline, reader.read_until(b'\n', line)).await {
            Err(_) => return Err("the daemon sent no welcome in time".to_owned()),
            Ok(Err(error)) => return Err(error.to_string()),
            Ok(Ok(0)) => return Err("the daemon closed before its welcome".to_owned()),
            Ok(Ok(_)) => {}
        }
        let text = String::from_utf8_lossy(line).into_owned();
        line.clear();
        match parse_line(&text) {
            Ok(ServerFrame::Welcome(welcome)) => return Ok(welcome.node),
            Ok(ServerFrame::Refused { reason }) => return Err(reason),
            Ok(_) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// The daemon's own error text as the failure it names.
pub fn classify_error(error: &str) -> AgentdError {
    if let Some(op) = error.strip_prefix("unsupported:") {
        return AgentdError::Unsupported(op.trim().to_owned());
    }
    if let Some(op) = error.strip_prefix("unknown op:") {
        return AgentdError::Unsupported(op.trim().to_owned());
    }
    if error == "expired" || error.starts_with("expired:") {
        return AgentdError::Expired;
    }
    AgentdError::Refused {
        reason: error.to_owned(),
        detail: None,
    }
}

async fn write_frame(write_half: &mut OwnedWriteHalf, frame: &Value) -> Result<(), String> {
    let mut line = serde_json::to_string(frame).map_err(|error| error.to_string())?;
    line.push('\n');
    write_half
        .write_all(line.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    write_half.flush().await.map_err(|error| error.to_string())
}

/// Epoch milliseconds, as expiresAt and ping.at carry them.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_error_becomes_the_failure_it_names() {
        assert_eq!(
            classify_error("unsupported:mhc.session.read"),
            AgentdError::Unsupported("mhc.session.read".to_owned())
        );
        assert_eq!(
            classify_error("unknown op: mhc.delivery.push"),
            AgentdError::Unsupported("mhc.delivery.push".to_owned())
        );
        assert_eq!(classify_error("expired"), AgentdError::Expired);
        assert_eq!(
            classify_error("fork_source_not_found"),
            AgentdError::Refused {
                reason: "fork_source_not_found".to_owned(),
                detail: None,
            }
        );
    }

    #[test]
    fn an_unsupported_answer_is_not_a_refusal_a_caller_may_retry_as_data() {
        let error = classify_error("unsupported:mhc.session.follow");
        assert_eq!(error.reason(), None);
        assert!(error.describe().contains("mhc.session.follow"));
        assert!(
            classify_error("session_busy")
                .describe()
                .contains("session_busy")
        );
    }
}
