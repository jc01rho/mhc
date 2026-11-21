//! Port of senpi `modes/interactive/attach-client.ts`: a presentation-only socket client. It cannot
//! spawn or fall back to an in-process runtime. The socket is read on its own thread; a request
//! blocks its caller until the endpoint answers or the deadline passes, so callers that must not
//! block (the TUI loop) run requests on a short-lived thread of their own.
use maho_rpc::jsonl::{JsonlLineReader, LineRecord, MAX_RPC_LINE_CHARACTERS, serialize_json_line};
use maho_rpc::rpc_request_deadline::request_deadline_ms;
use serde_json::Value;
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
type Stream = std::net::TcpStream;

const ANSWER_DEADLINE_MS: u64 = 30_000;
const DISCONNECTED_PENDING: &str = "Disconnected; pending command outcome is unknown";

/// The endpoint refused an attachment the presentation asked for; `code` is machine-readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachRefusedError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for AttachRefusedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AttachRefusedError {}

/// Why a request did not succeed. `AnswerRefused` is the endpoint's refusal of a dialog answer and
/// carries its error code (senpi `AnswerRefusedError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    AnswerRefused(String),
    Failed(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AnswerRefused(code) => write!(formatter, "Answer refused: {code}"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RequestError {}

type Pending = HashMap<String, mpsc::Sender<Result<Value, RequestError>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Shared {
    pending: Mutex<Pending>,
    socket: Mutex<Option<Stream>>,
    closed: AtomicBool,
    failure: Mutex<Option<String>>,
    on_record: Box<dyn Fn(Value) + Send + Sync>,
    on_disconnect: Box<dyn Fn(Option<String>) + Send + Sync>,
}

impl Shared {
    fn reject_pending(&self) {
        let pending: Vec<_> = lock(&self.pending).drain().collect();
        for (_, sender) in pending {
            let _ = sender.send(Err(RequestError::Failed(DISCONNECTED_PENDING.to_owned())));
        }
    }

    fn fail(&self, reason: &str) {
        lock(&self.failure).get_or_insert_with(|| reason.to_owned());
        if let Some(socket) = lock(&self.socket).as_ref() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }

    /// One endpoint line. A record it cannot read ends the connection; it never throws to the socket.
    fn receive(&self, line: &str) -> bool {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            self.fail("the endpoint sent an unreadable record");
            return false;
        };
        if record["type"] == "response" {
            let Some(id) = record["id"].as_str() else {
                return true;
            };
            let Some(sender) = lock(&self.pending).remove(id) else {
                return true;
            };
            let error = record["error"].as_str().unwrap_or_default().to_owned();
            let outcome = if record["success"] == true {
                Ok(record)
            } else if record["command"] == "extension_ui_response" {
                Err(RequestError::AnswerRefused(error))
            } else {
                Err(RequestError::Failed(error))
            };
            let _ = sender.send(outcome);
            return true;
        }
        (self.on_record)(record);
        true
    }

    fn ended(&self) {
        *lock(&self.socket) = None;
        self.reject_pending();
        if !self.closed.load(Ordering::SeqCst) {
            let failure = lock(&self.failure).clone();
            (self.on_disconnect)(failure);
        }
    }
}

fn read_loop(mut stream: Stream, shared: &Shared) {
    let Ok(mut reader) = JsonlLineReader::new(MAX_RPC_LINE_CHARACTERS) else {
        shared.ended();
        return;
    };
    let mut chunk = [0u8; 8192];
    'read: loop {
        let count = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        for record in reader.push(&chunk[..count]) {
            match record {
                LineRecord::Line(line) => {
                    if !shared.receive(&line) {
                        break 'read;
                    }
                }
                LineRecord::Oversized => {
                    shared.fail("the endpoint sent a record over the line limit");
                    break 'read;
                }
            }
        }
    }
    shared.ended();
}

/// Senpi `connectRpcClientSocket`: a terminal control socket takes the secret handshake, every
/// other endpoint takes the authenticated public challenge first.
#[cfg(unix)]
fn open_stream(path: &str, key_root: Option<&Path>) -> Result<Stream, String> {
    use maho_rpc::socket_transport::{
        SOCKET_HANDSHAKE_TIMEOUT_MS, read_socket_secret, socket_secret_path,
    };
    if maho_rpc::host_endpoints::is_tui_control_socket(path) {
        let secret = read_socket_secret(Path::new(&socket_secret_path(path)?))
            .map_err(|error| error.to_string())?;
        let mut stream = Stream::connect(path).map_err(|error| error.to_string())?;
        stream
            .write_all(&secret)
            .map_err(|error| error.to_string())?;
        return Ok(stream);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let (socket, _) = maho_rpc::host_probe::authenticated_host_socket(
            path,
            &serde_json::json!({"type":"get_protocol_info","id":"rpc-auth-protocol","observe":true}),
            SOCKET_HANDSHAKE_TIMEOUT_MS,
            key_root,
        )
        .await
        .map_err(|error| error.to_string())?;
        let stream = socket
            .into_stream()
            .map_err(|error| error.to_string())?
            .into_std()
            .map_err(|error| error.to_string())?;
        stream
            .set_nonblocking(false)
            .map_err(|error| error.to_string())?;
        Ok(stream)
    })
}

#[cfg(not(unix))]
fn open_stream(_path: &str, _key_root: Option<&Path>) -> Result<Stream, String> {
    Err("--attach needs Unix domain sockets, which this platform does not provide".to_owned())
}

/// A presentation-only socket client (senpi `AttachClient`).
pub struct AttachClient {
    shared: Arc<Shared>,
    answer_deadline_ms: u64,
}

impl AttachClient {
    /// Connects and starts reading. `on_record` receives every non-response line, `on_disconnect`
    /// fires once when the connection ends without `close()` (with the failure that ended it).
    pub fn connect(
        path: &str,
        key_root: Option<&Path>,
        on_record: Box<dyn Fn(Value) + Send + Sync>,
        on_disconnect: Box<dyn Fn(Option<String>) + Send + Sync>,
        answer_deadline_ms: Option<u64>,
    ) -> Result<Self, String> {
        let stream = open_stream(path, key_root)?;
        let reader = stream.try_clone().map_err(|error| error.to_string())?;
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            socket: Mutex::new(Some(stream)),
            closed: AtomicBool::new(false),
            failure: Mutex::new(None),
            on_record,
            on_disconnect,
        });
        let reading = shared.clone();
        std::thread::Builder::new()
            .name("attach-reader".to_owned())
            .spawn(move || read_loop(reader, &reading))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            shared,
            answer_deadline_ms: answer_deadline_ms.unwrap_or(ANSWER_DEADLINE_MS),
        })
    }

    pub fn request(&self, mut command: Value) -> Result<Value, RequestError> {
        let id = uuid::Uuid::new_v4().to_string();
        command["id"] = Value::String(id.clone());
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let deadline = request_deadline_ms(&command);
        self.track(
            &id,
            deadline,
            &format!("No acknowledgement for {kind}; its outcome is unknown"),
            &command,
        )
    }

    /// Sends a dialog answer and returns once the endpoint accepts it; a refusal carries its code.
    pub fn answer(&self, session_id: &str, response: &Value) -> Result<(), RequestError> {
        let mut record = response.clone();
        record["sessionId"] = Value::String(session_id.to_owned());
        record["answerId"] = Value::String(uuid::Uuid::new_v4().to_string());
        let id = record["id"].as_str().unwrap_or_default().to_owned();
        self.track(
            &id,
            self.answer_deadline_ms,
            "No acknowledgement for the answer",
            &record,
        )
        .map(|_| ())
    }

    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some(socket) = lock(&self.shared.socket).take() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        self.shared.reject_pending();
    }

    fn track(
        &self,
        id: &str,
        deadline_ms: u64,
        unanswered: &str,
        record: &Value,
    ) -> Result<Value, RequestError> {
        let (sender, receiver) = mpsc::channel();
        lock(&self.shared.pending).insert(id.to_owned(), sender);
        let written = match (
            serialize_json_line(record),
            lock(&self.shared.socket).as_mut(),
        ) {
            (Ok(line), Some(socket)) => socket.write_all(line.as_bytes()).is_ok(),
            _ => false,
        };
        if !written {
            lock(&self.shared.pending).remove(id);
            return Err(RequestError::Failed("Disconnected".to_owned()));
        }
        match receiver.recv_timeout(Duration::from_millis(deadline_ms)) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.shared.pending).remove(id);
                Err(RequestError::Failed(unanswered.to_owned()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(RequestError::Failed(DISCONNECTED_PENDING.to_owned()))
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::attach_test_endpoint::{AnswerBehavior, Endpoint, EndpointOptions};
    use serde_json::json;

    const WAIT: Duration = Duration::from_secs(5);

    fn client_for(
        endpoint: &Endpoint,
    ) -> (
        AttachClient,
        mpsc::Receiver<Value>,
        mpsc::Receiver<Option<String>>,
    ) {
        let (records, received) = mpsc::channel();
        let (disconnects, disconnected) = mpsc::channel();
        let records = Mutex::new(records);
        let disconnects = Mutex::new(disconnects);
        let client = AttachClient::connect(
            &endpoint.path,
            None,
            Box::new(move |record| {
                let _ = lock(&records).send(record);
            }),
            Box::new(move |reason| {
                let _ = lock(&disconnects).send(reason);
            }),
            Some(200),
        )
        .unwrap();
        (client, received, disconnected)
    }

    #[test]
    fn a_request_resolves_with_its_response_and_pushed_records_arrive_separately() {
        let endpoint = Endpoint::start(EndpointOptions::default());
        let (client, records, _) = client_for(&endpoint);
        let listing = client
            .request(json!({"type": "list_sessions", "observe": true}))
            .unwrap();
        assert_eq!(
            listing["data"]["sessions"][0]["durableSessionId"],
            "durable"
        );
        endpoint.send(&json!({"type": "agent_start"}));
        let record = records.recv_timeout(WAIT).unwrap();
        assert_eq!(record["type"], "agent_start");
        assert_eq!(record["sessionId"], "live");
        client.close();
    }

    #[test]
    fn a_refused_answer_carries_the_endpoint_error_code() {
        let endpoint = Endpoint::start(EndpointOptions {
            answer: AnswerBehavior::Refuse("question_already_resolved".to_owned()),
            ..Default::default()
        });
        let (client, _, _) = client_for(&endpoint);
        let error = client
            .answer(
                "live",
                &json!({"type": "extension_ui_response", "id": "a1", "value": "x"}),
            )
            .unwrap_err();
        assert_eq!(
            error,
            RequestError::AnswerRefused("question_already_resolved".to_owned())
        );
        let sent = endpoint.command("extension_ui_response");
        assert_eq!(sent["sessionId"], "live");
        assert!(sent["answerId"].as_str().is_some_and(|id| !id.is_empty()));
        client.close();
    }

    #[test]
    fn an_unanswered_answer_fails_at_its_deadline_and_leaves_nothing_pending() {
        let endpoint = Endpoint::start(EndpointOptions {
            answer: AnswerBehavior::Silent,
            ..Default::default()
        });
        let (client, _, _) = client_for(&endpoint);
        let outcome = client.answer(
            "live",
            &json!({"type": "extension_ui_response", "id": "held"}),
        );
        assert_eq!(
            outcome,
            Err(RequestError::Failed(
                "No acknowledgement for the answer".to_owned()
            ))
        );
        assert!(lock(&client.shared.pending).is_empty());
        client.close();
    }

    #[test]
    fn an_unreadable_record_ends_the_connection_with_the_reason() {
        let endpoint = Endpoint::start(EndpointOptions::default());
        let (client, _, disconnected) = client_for(&endpoint);
        client.request(json!({"type": "list_sessions"})).unwrap();
        endpoint.send_raw("not json");
        let reason = disconnected.recv_timeout(WAIT).unwrap();
        assert_eq!(
            reason.as_deref(),
            Some("the endpoint sent an unreadable record")
        );
        let after = client.request(json!({"type": "get_state"}));
        assert!(after.is_err());
    }

    #[test]
    fn a_closed_client_rejects_new_requests_and_never_reports_a_disconnect() {
        let endpoint = Endpoint::start(EndpointOptions::default());
        let (client, _, disconnected) = client_for(&endpoint);
        client.close();
        assert_eq!(
            client.request(json!({"type": "get_state"})),
            Err(RequestError::Failed("Disconnected".to_owned()))
        );
        drop(client);
        assert!(disconnected.recv().is_err());
    }
}
