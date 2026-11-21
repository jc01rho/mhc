//! Port of `local-api/client.ts`, `client-open.ts` and `client-connection.ts`: the state dir's access
//! check, the local socket and the per-boot token around one connection.
//!
//! The client sends nothing but its `hello` until the service proves the token (`server_proof`), reads
//! the token fresh after the `welcome` (a restarted service has a new one), then authenticates. From
//! there requests are correlated by id and events go to every subscriber. A message the contract does
//! not allow fails every pending request and closes the connection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::access::read_token;
use super::errors::LocalApiError;
use super::framing::{DecodedLine, LineDecoder, encode_line, parse_line};
use super::limits::{LOCAL_INLINE_MAX, line_bytes};
use super::proof::{client_proof, mint_id, mint_nonce, proof_matches, server_proof};
use super::protocol::{ClientInfo, LOCAL_API_VERSION, Welcome, hello_message};
use super::transport::{BoxedReader, BoxedWriter, LocalTransport, connect_local};
use super::vocabulary::is_terminal_command_state;

pub const HANDSHAKE_TIMEOUT_MS: u64 = 10_000;
pub const REQUEST_TIMEOUT_MS: u64 = 30_000;
const READ_BUFFER: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct ConnectOptions {
    pub state_dir: PathBuf,
    pub client: ClientInfo,
    pub handshake_timeout: Duration,
    pub request_timeout: Duration,
}

impl ConnectOptions {
    pub fn new(state_dir: impl Into<PathBuf>, client: ClientInfo) -> Self {
        Self {
            state_dir: state_dir.into(),
            client,
            handshake_timeout: Duration::from_millis(HANDSHAKE_TIMEOUT_MS),
            request_timeout: Duration::from_millis(REQUEST_TIMEOUT_MS),
        }
    }
}

type Settle = oneshot::Sender<Result<Value, LocalApiError>>;

#[derive(Default)]
struct Pending {
    waiting: HashMap<String, Settle>,
    /// Requests answered `request_timeout`: a late answer is dropped, not a violation.
    expired: HashSet<String>,
}

struct Shared {
    writer: tokio::sync::Mutex<Option<BoxedWriter>>,
    pending: Mutex<Pending>,
    subscribers: Mutex<Vec<mpsc::UnboundedSender<Value>>>,
    next_id: AtomicU64,
    ended: AtomicBool,
    oversized: AtomicU64,
    request_timeout: Duration,
}

fn violation(detail: &str) -> LocalApiError {
    LocalApiError::protocol_violation(detail)
}

fn closed() -> LocalApiError {
    LocalApiError::new("closed", "the connection closed")
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn subscribers(&self) -> MutexGuard<'_, Vec<mpsc::UnboundedSender<Value>>> {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn fail_all(&self, error: &LocalApiError) {
        let waiting = {
            let mut pending = self.pending();
            std::mem::take(&mut pending.waiting)
        };
        for (_, settle) in waiting {
            let _ = settle.send(Err(error.clone()));
        }
    }

    async fn write(&self, message: &Value) -> Result<(), LocalApiError> {
        let line = encode_line(message)?;
        let mut writer = self.writer.lock().await;
        let Some(stream) = writer.as_mut() else {
            return Err(LocalApiError::new("closed", "the connection is closed"));
        };
        stream
            .write_all(&line)
            .await
            .map_err(|error| LocalApiError::new("closed", error.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|error| LocalApiError::new("closed", error.to_string()))
    }

    async fn end(&self, error: &LocalApiError) {
        self.ended.store(true, Ordering::SeqCst);
        self.fail_all(error);
        self.subscribers().clear();
        if let Some(mut stream) = self.writer.lock().await.take() {
            let _ = stream.shutdown().await;
        }
    }

    fn on_line(&self, line: &[u8]) -> Result<(), LocalApiError> {
        let message = parse_line(line)?;
        match message.get("type").and_then(Value::as_str) {
            Some("response") => self.settle(&message),
            Some("event") => {
                if message.get("kind").and_then(Value::as_str).is_none() {
                    return Err(violation("an event without a kind"));
                }
                self.subscribers()
                    .retain(|subscriber| subscriber.send(message.clone()).is_ok());
                Ok(())
            }
            _ => Err(violation(
                "a message that is neither a response nor an event",
            )),
        }
    }

    fn settle(&self, message: &Value) -> Result<(), LocalApiError> {
        let Some(id) = message.get("id").and_then(Value::as_str) else {
            return Err(violation("a response without an id"));
        };
        let settle = {
            let mut pending = self.pending();
            if pending.expired.remove(id) {
                return Ok(());
            }
            pending.waiting.remove(id)
        };
        let Some(settle) = settle else {
            return Err(violation("a response to no pending request"));
        };
        match response_result(message) {
            Ok(result) => {
                let _ = settle.send(result);
                Ok(())
            }
            Err(error) => {
                let _ = settle.send(Err(error.clone()));
                Err(error)
            }
        }
    }
}

fn response_result(message: &Value) -> Result<Result<Value, LocalApiError>, LocalApiError> {
    match message.get("ok").and_then(Value::as_bool) {
        Some(true) => match message.get("data") {
            Some(data @ Value::Object(_)) => Ok(Ok(data.clone())),
            _ => Err(violation("a response without its data")),
        },
        Some(false) => message
            .get("error")
            .and_then(LocalApiError::from_error_body)
            .map(Err)
            .ok_or_else(|| violation("a refusal without its error")),
        None => Err(violation("a response without ok")),
    }
}

async fn read_loop(
    shared: Arc<Shared>,
    mut reader: BoxedReader,
    mut decoder: LineDecoder,
    early: Vec<DecodedLine>,
) {
    let handle = |decoded: DecodedLine| -> Result<(), LocalApiError> {
        match decoded {
            DecodedLine::Oversized(_) => {
                shared.oversized.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            DecodedLine::Line(line) => shared.on_line(&line),
        }
    };
    for decoded in early {
        if let Err(error) = handle(decoded) {
            shared.end(&error).await;
            return;
        }
    }
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        for decoded in decoder.push(&buffer[..read]) {
            if let Err(error) = handle(decoded) {
                shared.end(&error).await;
                return;
            }
        }
    }
    shared.end(&closed()).await;
}

/// One authenticated connection to the device service.
pub struct LocalApiClient {
    shared: Arc<Shared>,
    welcome: Welcome,
    endpoint: String,
    reader: JoinHandle<()>,
}

impl Drop for LocalApiClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Checks the state dir (POSIX), connects to its endpoint and runs the handshake.
pub async fn connect_local_api(options: ConnectOptions) -> Result<LocalApiClient, LocalApiError> {
    #[cfg(unix)]
    super::access::check_posix_access(&options.state_dir)?;
    let transport = connect_local(&options.state_dir).await?;
    let state_dir = options.state_dir.clone();
    open_local_api_client(
        transport,
        &options.client,
        move || read_token(&state_dir),
        options.handshake_timeout,
        options.request_timeout,
    )
    .await
}

/// The transport-neutral handshake over an open transport.
pub async fn open_local_api_client(
    transport: LocalTransport,
    client: &ClientInfo,
    read_token: impl FnOnce() -> Result<Vec<u8>, LocalApiError>,
    handshake_timeout: Duration,
    request_timeout: Duration,
) -> Result<LocalApiClient, LocalApiError> {
    let LocalTransport {
        endpoint,
        mut reader,
        mut writer,
    } = transport;
    let client_nonce = mint_nonce()?;
    let hello = encode_line(&hello_message(client, &client_nonce))?;
    let handshake = first_message(&mut reader, &mut writer, &hello);
    let (first, early, decoder) = match tokio::time::timeout(handshake_timeout, handshake).await {
        Ok(answer) => answer?,
        Err(_) => {
            return Err(LocalApiError::new(
                "handshake_timeout",
                "the device service did not complete the handshake in time",
            ));
        }
    };
    match first.get("type").and_then(Value::as_str) {
        Some("welcome") => {}
        Some("refused") => {
            let reason = first
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("malformed");
            return Err(LocalApiError::new("refused", reason));
        }
        _ => {
            return Err(violation(
                "the first message is neither a welcome nor a refusal",
            ));
        }
    }
    let welcome = Welcome::parse(&first)?;
    if welcome.v != LOCAL_API_VERSION {
        return Err(LocalApiError::new("refused", "version"));
    }
    let token = read_token()?;
    let expected = server_proof(&token, &client_nonce, &endpoint)?;
    if !proof_matches(&expected, &welcome.server_proof) {
        let _ = writer.shutdown().await;
        return Err(LocalApiError::new(
            "server_proof_invalid",
            "the device service did not prove this boot's token",
        ));
    }
    let proof = client_proof(&token, &welcome.server_nonce, &endpoint)?;
    let shared = Arc::new(Shared {
        writer: tokio::sync::Mutex::new(Some(writer)),
        pending: Mutex::new(Pending::default()),
        subscribers: Mutex::new(Vec::new()),
        next_id: AtomicU64::new(1),
        ended: AtomicBool::new(false),
        oversized: AtomicU64::new(0),
        request_timeout,
    });
    let reader = tokio::spawn(read_loop(Arc::clone(&shared), reader, decoder, early));
    let client = LocalApiClient {
        shared,
        welcome,
        endpoint,
        reader,
    };
    client
        .request("authenticate", json!({ "client_proof": proof }))
        .await?;
    Ok(client)
}

/// Sends the `hello` and reads the first message back, with the lines that arrived after it and the
/// decoder holding any partial line.
async fn first_message(
    reader: &mut BoxedReader,
    writer: &mut BoxedWriter,
    hello: &[u8],
) -> Result<(Value, Vec<DecodedLine>, LineDecoder), LocalApiError> {
    let io = |error: std::io::Error| LocalApiError::new("closed", error.to_string());
    writer.write_all(hello).await.map_err(io)?;
    writer.flush().await.map_err(io)?;
    let mut decoder = LineDecoder::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let read = reader.read(&mut buffer).await.map_err(io)?;
        if read == 0 {
            return Err(LocalApiError::new(
                "closed",
                "the device service closed the connection during the handshake",
            ));
        }
        let mut decoded = decoder.push(&buffer[..read]);
        let Some(index) = decoded
            .iter()
            .position(|line| matches!(line, DecodedLine::Line(_)))
        else {
            continue;
        };
        let early = decoded.split_off(index + 1);
        if let Some(DecodedLine::Line(line)) = decoded.pop() {
            return Ok((parse_line(&line)?, early, decoder));
        }
    }
}

/// One `sessions.list` query's last complete revision.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListingResult {
    pub revision: u64,
    pub rows: Vec<Value>,
    /// Devices whose rows may be missing from `rows`.
    pub incomplete: Vec<Value>,
    /// The round's device states, device to state.
    pub devices: Value,
    /// The round reached `done` within the request bound.
    pub complete: bool,
}

/// How a `session.command` ended, from its last `session.command_state`.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandOutcome {
    pub command_id: String,
    pub state: String,
    pub reason: Option<String>,
    pub retryable: bool,
    pub receipt: Option<Value>,
}

impl CommandOutcome {
    pub fn from_event(event: &Value) -> Self {
        Self {
            command_id: event
                .get("command_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            state: event
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            reason: event
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
            retryable: event
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            receipt: event
                .get("receipt")
                .cloned()
                .filter(|receipt| !receipt.is_null()),
        }
    }

    pub fn uncertain(command_id: impl Into<String>, reason: &str) -> Self {
        Self {
            command_id: command_id.into(),
            state: "uncertain".into(),
            reason: Some(reason.into()),
            retryable: false,
            receipt: None,
        }
    }

    pub fn applied(&self) -> bool {
        self.state == "engine_applied"
    }
}

/// `control.session_wake.*`: read, or set to on/off/`null` (the build default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeSetting {
    Read,
    Set(Option<bool>),
}

impl LocalApiClient {
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    pub fn service_version(&self) -> &str {
        &self.welcome.service_version
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn has_capability(&self, capability: &str) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|known| known == capability)
    }

    pub fn oversized_lines(&self) -> u64 {
        self.shared.oversized.load(Ordering::SeqCst)
    }

    /// Every event from now on, until the connection ends.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<Value> {
        let (sender, receiver) = mpsc::unbounded_channel();
        if !self.shared.ended.load(Ordering::SeqCst) {
            self.shared.subscribers().push(sender);
        }
        receiver
    }

    /// One request; its answer's `data`, or the owner's error.
    pub async fn request(&self, kind: &str, params: Value) -> Result<Value, LocalApiError> {
        if self.shared.ended.load(Ordering::SeqCst) {
            return Err(LocalApiError::new("closed", "the connection is closed"));
        }
        let id = self
            .shared
            .next_id
            .fetch_add(1, Ordering::SeqCst)
            .to_string();
        let mut message = match params {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            _ => {
                return Err(LocalApiError::new(
                    "invalid_request",
                    "request parameters must be an object",
                ));
            }
        };
        message.insert("id".into(), Value::String(id.clone()));
        message.insert("type".into(), Value::String(kind.to_owned()));
        let message = Value::Object(message);
        if line_bytes(&message) > LOCAL_INLINE_MAX {
            return Err(violation("a request line over LOCAL_INLINE_MAX"));
        }
        let (settle, settled) = oneshot::channel();
        self.shared.pending().waiting.insert(id.clone(), settle);
        if let Err(error) = self.shared.write(&message).await {
            self.shared.pending().waiting.remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(self.shared.request_timeout, settled).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(closed()),
            Err(_) => {
                let mut pending = self.shared.pending();
                if pending.waiting.remove(&id).is_some() {
                    pending.expired.insert(id);
                }
                Err(LocalApiError::new(
                    "request_timeout",
                    format!("the owner did not answer {kind} in time"),
                ))
            }
        }
    }

    pub async fn status(&self) -> Result<Value, LocalApiError> {
        self.request("status.get", json!({})).await
    }

    pub async fn devices(&self) -> Result<Vec<Value>, LocalApiError> {
        let data = self.request("devices.list", json!({})).await?;
        Ok(data
            .get("devices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// This device as the service holds it, `None` when it holds none.
    pub async fn device_info(&self) -> Result<Option<Value>, LocalApiError> {
        let data = self.request("device.info", json!({})).await?;
        Ok(data
            .get("device")
            .cloned()
            .filter(|device| !device.is_null()))
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<bool, LocalApiError> {
        let data = self
            .request("service.set_enabled", json!({ "enabled": enabled }))
            .await?;
        Ok(data
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(enabled))
    }

    /// `control.attach.get`, or `control.attach.set` with `attached`.
    pub async fn control_attach(&self, attached: Option<bool>) -> Result<Value, LocalApiError> {
        match attached {
            None => self.request("control.attach.get", json!({})).await,
            Some(attached) => {
                self.request("control.attach.set", json!({ "attached": attached }))
                    .await
            }
        }
    }

    pub async fn session_wake(&self, setting: WakeSetting) -> Result<Value, LocalApiError> {
        match setting {
            WakeSetting::Read => self.request("control.session_wake.get", json!({})).await,
            WakeSetting::Set(enabled) => {
                self.request("control.session_wake.set", json!({ "enabled": enabled }))
                    .await
            }
        }
    }

    pub async fn register_endpoint(&self, agent_dir: &Path) -> Result<(), LocalApiError> {
        self.request(
            "endpoints.register",
            json!({ "agent_dir": agent_dir.to_string_lossy() }),
        )
        .await
        .map(|_| ())
    }

    pub async fn forget_device(&self) -> Result<(), LocalApiError> {
        self.request("device.forget", json!({})).await.map(|_| ())
    }

    /// One `sessions.list` query, followed through its round until `done` or the request bound; the
    /// last complete revision wins (a client replaces its whole view at each `listing_end`).
    pub async fn sessions_list(
        &self,
        filter: Option<Value>,
        rows: Option<u64>,
    ) -> Result<ListingResult, LocalApiError> {
        let mut events = self.subscribe();
        let mut params = Map::new();
        if let Some(filter) = filter {
            params.insert("filter".into(), filter);
        }
        if let Some(rows) = rows {
            params.insert("rows".into(), rows.into());
        }
        let data = self.request("sessions.list", Value::Object(params)).await?;
        let query_id = data
            .get("query_id")
            .and_then(Value::as_str)
            .ok_or_else(|| violation("sessions.list answered without a query_id"))?
            .to_owned();
        let deadline = tokio::time::Instant::now() + self.shared.request_timeout;
        let mut result = ListingResult::default();
        let mut building: Option<(u64, Vec<Value>)> = None;
        loop {
            let event = match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => return Err(closed()),
                Err(_) => return Ok(result),
            };
            if event.get("query_id").and_then(Value::as_str) != Some(query_id.as_str()) {
                continue;
            }
            match event.get("kind").and_then(Value::as_str) {
                Some("sessions.listing_begin") => {
                    let revision = event.get("revision").and_then(Value::as_u64).unwrap_or(0);
                    building = Some((revision, Vec::new()));
                }
                Some("sessions.listing_rows") => {
                    if let (Some((_, rows)), Some(page)) = (
                        building.as_mut(),
                        event.get("rows").and_then(Value::as_array),
                    ) {
                        rows.extend(page.iter().cloned());
                    }
                }
                Some("sessions.listing_end") => {
                    if let Some((revision, rows)) = building.take() {
                        result.revision = revision;
                        result.rows = rows;
                        result.incomplete = event
                            .get("incomplete")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                    }
                }
                Some("sessions.round") => {
                    result.devices = event.get("devices").cloned().unwrap_or(Value::Null);
                    if event.get("state").and_then(Value::as_str) == Some("done") {
                        result.complete = true;
                        return Ok(result);
                    }
                }
                _ => {}
            }
        }
    }

    /// `session.attach`: a handle owned by this connection.
    pub async fn attach(&self, device: &str, session_id: &str) -> Result<String, LocalApiError> {
        let data = self
            .request(
                "session.attach",
                json!({ "device": device, "session_id": session_id }),
            )
            .await?;
        data.get("attach")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| violation("session.attach answered without a handle"))
    }

    pub async fn detach(&self, attach: &str) -> Result<(), LocalApiError> {
        self.request("session.detach", json!({ "attach": attach }))
            .await
            .map(|_| ())
    }

    /// `session.command` with one engine RPC record, followed until its terminal
    /// `session.command_state`. A `command_id` is always sent (one is minted when none is given), and
    /// a timeout carries it, so a retry reuses it instead of dispatching twice.
    pub async fn command(
        &self,
        attach: &str,
        rpc: Value,
        command_id: Option<String>,
    ) -> Result<CommandOutcome, LocalApiError> {
        let command_id = match command_id {
            Some(command_id) => command_id,
            None => mint_id()?,
        };
        let mut events = self.subscribe();
        let data = match self
            .request(
                "session.command",
                json!({ "attach": attach, "command_id": command_id, "rpc": rpc }),
            )
            .await
        {
            Ok(data) => data,
            Err(mut error) => {
                if error.is("request_timeout") {
                    error.command_id = Some(command_id);
                }
                return Err(error);
            }
        };
        let command_id = data
            .get("command_id")
            .and_then(Value::as_str)
            .map_or(command_id, str::to_owned);
        let deadline = tokio::time::Instant::now() + self.shared.request_timeout;
        loop {
            let event = match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => return Ok(CommandOutcome::uncertain(command_id, "request_gone")),
                Err(_) => return Ok(CommandOutcome::uncertain(command_id, "no_receipt")),
            };
            if event.get("kind").and_then(Value::as_str) != Some("session.command_state")
                || event.get("command_id").and_then(Value::as_str) != Some(command_id.as_str())
            {
                continue;
            }
            let state = event
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if is_terminal_command_state(state) {
                return Ok(CommandOutcome::from_event(&event));
            }
        }
    }

    /// Ends the connection; the service detaches everything this connection attached.
    pub async fn close(&self) {
        if let Some(mut stream) = self.shared.writer.lock().await.take() {
            let _ = stream.shutdown().await;
        }
    }
}
