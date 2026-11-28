//! One kept connection to the engine host: the carrier's own attachment to a woken session lives on it (A3.1.3).
//!
//! Upstream counterpart: `thread/wake/carrier-connection.ts`. The carrier is synchronous, so the socket half
//! (`connectCarrierHost` over senpi-task's authenticated `rpc-socket-client`) runs on a runtime thread of its own:
//! each [`CarrierHostConnection::request`] dials the host's authenticated socket, asks, and closes - the one-request
//! wire this crate's `HostTransport` uses. Two things a kept socket would give are therefore absent: no frame arrives
//! outside a reply (the carrier's `session_lifecycle` listener hears nothing between sweeps, and its next sweep reads
//! the same state), and a host that dies is noticed at the next request rather than as a `close` event.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

pub type CarrierFrameListener = Arc<dyn Fn(&Value) + Send + Sync>;
pub type Unsubscribe = Box<dyn FnOnce() + Send>;
pub type ConnectionClosed = Box<dyn FnOnce() + Send>;

pub trait CarrierHostConnection: Send + Sync {
    /// Sends one frame and answers the host's reply to it, or why no reply came.
    fn request(&self, frame: Value) -> Result<Value, String>;
    /// Every line that answers no request (`session_lifecycle`, broadcasts).
    fn on_event(&self, listener: CarrierFrameListener) -> Unsubscribe;
    /// Runs once the connection ended; at once when it already has.
    fn on_closed(&self, callback: ConnectionClosed);
    fn close(&self);
}

pub type CarrierConnect =
    Arc<dyn Fn() -> Result<Arc<dyn CarrierHostConnection>, String> + Send + Sync>;

/// The engine host answered no request because it is not there.
pub const HOST_UNAVAILABLE: &str = "host_unavailable";

/// The connection was closed before this frame could be asked.
pub const CONNECTION_CLOSED: &str = "the engine host closed the carrier's connection";

#[cfg(unix)]
mod host_connection {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, Sender};

    use super::*;

    /// How long the carrier waits for one host reply to a `open_session`/`wake`/`release_session` frame.
    pub(super) const REQUEST_TIMEOUT_MS: u64 = 30_000;

    /// One frame a caller waits on, and the channel its answer goes back through.
    struct Job {
        frame: Value,
        reply: Sender<Result<Value, String>>,
    }

    /// Whether the connection ended, and what runs once it has.
    #[derive(Default)]
    struct Closed {
        ended: bool,
        callbacks: Vec<ConnectionClosed>,
    }

    /// The carrier's connection to one engine host: a runtime thread that dials the host's authenticated socket per
    /// frame, and the listeners the carrier registered on the connection.
    pub(super) struct HostConnection {
        /// `None` once the connection closed: a frame asked after that fails as a closed connection does.
        jobs: Mutex<Option<Sender<Job>>>,
        /// Kept behind an `Arc` so an `Unsubscribe` owns its registry instead of borrowing the connection.
        listeners: Arc<Mutex<Vec<(u64, CarrierFrameListener)>>>,
        next_listener: AtomicU64,
        closed: Mutex<Closed>,
    }

    impl HostConnection {
        pub(super) fn connect(
            socket: String,
            agent_dir: PathBuf,
            timeout_ms: u64,
        ) -> Result<Arc<Self>, String> {
            let (outbox, inbox) = mpsc::channel();
            let serving_socket = socket.clone();
            std::thread::Builder::new()
                .name("carrier-host".to_owned())
                .spawn(move || serve(serving_socket, agent_dir, timeout_ms, inbox))
                .map_err(|error| format!("{HOST_UNAVAILABLE}:{socket}:{error}"))?;
            Ok(Arc::new(Self {
                jobs: Mutex::new(Some(outbox)),
                listeners: Arc::default(),
                next_listener: AtomicU64::new(1),
                closed: Mutex::default(),
            }))
        }

        /// How many listeners the connection holds (a test reads it to see an unsubscribe land).
        #[cfg(test)]
        pub(super) fn listener_count(&self) -> usize {
            self.listeners
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
        }

        fn enqueue(&self, frame: Value) -> Result<Receiver<Result<Value, String>>, String> {
            let (reply, answer) = mpsc::channel();
            let slot = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(jobs) = slot.as_ref() else {
                return Err(CONNECTION_CLOSED.to_owned());
            };
            jobs.send(Job { frame, reply })
                .map_err(|_| CONNECTION_CLOSED.to_owned())?;
            Ok(answer)
        }
    }

    impl CarrierHostConnection for HostConnection {
        fn request(&self, frame: Value) -> Result<Value, String> {
            let answer = self.enqueue(frame)?;
            answer
                .recv()
                .unwrap_or_else(|_| Err(CONNECTION_CLOSED.to_owned()))
        }

        fn on_event(&self, listener: CarrierFrameListener) -> Unsubscribe {
            let id = self.next_listener.fetch_add(1, Ordering::SeqCst);
            self.listeners
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((id, listener));
            let listeners = self.listeners.clone();
            Box::new(move || {
                listeners
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retain(|(entry, _)| *entry != id);
            })
        }

        fn on_closed(&self, callback: ConnectionClosed) {
            let mut state = self.closed.lock().unwrap_or_else(PoisonError::into_inner);
            if state.ended {
                drop(state);
                callback();
                return;
            }
            state.callbacks.push(callback);
        }

        fn close(&self) {
            let jobs = self
                .jobs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            // Dropping the last sender ends the runtime thread; a frame already in flight answers closed.
            drop(jobs);
            let callbacks = {
                let mut state = self.closed.lock().unwrap_or_else(PoisonError::into_inner);
                state.ended = true;
                std::mem::take(&mut state.callbacks)
            };
            for callback in callbacks {
                callback();
            }
        }
    }

    /// The connection's runtime thread: one current-thread runtime, one authenticated dial per frame, until the
    /// connection is closed or dropped (either drops the last sender).
    fn serve(socket: String, agent_dir: PathBuf, timeout_ms: u64, inbox: Receiver<Job>) {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        while let Ok(job) = inbox.recv() {
            let answer = runtime.block_on(ask(&socket, &agent_dir, timeout_ms, job.frame));
            let _ = job.reply.send(answer);
        }
    }

    /// One frame on the host's authenticated socket: `get_protocol_info` proves the host's identity, then the frame
    /// travels the same connection (`HostTransport::open` + `request`).
    async fn ask(
        socket: &str,
        agent_dir: &Path,
        timeout_ms: u64,
        frame: Value,
    ) -> Result<Value, String> {
        let mut connection = maho_rpc::host_probe::authenticated_host_socket(
            socket,
            &serde_json::json!({ "type": "get_protocol_info", "observe": true }),
            timeout_ms,
            Some(agent_dir),
        )
        .await
        .map(|(connection, _)| connection)
        .map_err(|error| failure(socket, error.as_ref()))?;
        connection
            .request(frame)
            .await
            .map_err(|error| failure(socket, &error))
    }

    /// A host that is not there is `host_unavailable:<socket>`, the failure the carrier counts as unreachable; a host
    /// whose identity is not proven keeps its typed code.
    fn failure(socket: &str, error: &(dyn std::error::Error + Send + Sync + 'static)) -> String {
        if let Some(identity) = error.downcast_ref::<maho_rpc::socket_auth::RpcIdentityError>() {
            return format!("{}:{socket}", identity.code);
        }
        match error.downcast_ref::<std::io::Error>() {
            Some(io) => match io.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    format!("{HOST_UNAVAILABLE}:{socket}")
                }
                _ => io.to_string(),
            },
            None => error.to_string(),
        }
    }
}

/// `resolveThreadSocket` on this host: `--socket`, then the brand's `RPC_SOCKET`, then `<agentDir>/rpc/rpc.sock`.
pub fn resolve_host_socket(explicit: Option<&str>, agent_dir: &std::path::Path) -> String {
    explicit.map(str::to_owned).unwrap_or_else(|| {
        maho_core::brand::env_value("RPC_SOCKET", &maho_core::config::current_env())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                agent_dir
                    .join("rpc/rpc.sock")
                    .to_string_lossy()
                    .into_owned()
            })
    })
}

/// The carrier's socket for an agent directory: the operator host's socket the thread surface opens on. The same read
/// `maho_omo_remote::sync_status::operator_socket` makes; that crate depends on this one, so the two lines stay apart.
pub fn carrier_socket(agent_dir: &std::path::Path) -> String {
    resolve_host_socket(None, agent_dir)
}

/// The carrier's engine-host connector for an agent directory: one authenticated connection per request, on the
/// socket the carrier opens sessions on.
#[cfg(unix)]
pub fn carrier_connect(socket: String, agent_dir: std::path::PathBuf) -> CarrierConnect {
    Arc::new(move || -> Result<Arc<dyn CarrierHostConnection>, String> {
        let connection: Arc<dyn CarrierHostConnection> = host_connection::HostConnection::connect(
            socket.clone(),
            agent_dir.clone(),
            host_connection::REQUEST_TIMEOUT_MS,
        )?;
        Ok(connection)
    })
}

#[cfg(not(unix))]
pub fn carrier_connect(socket: String, _agent_dir: std::path::PathBuf) -> CarrierConnect {
    Arc::new(move || -> Result<Arc<dyn CarrierHostConnection>, String> {
        Err(format!("{HOST_UNAVAILABLE}:{socket}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carrier_socket_reads_the_brands_socket_then_the_agent_dir() {
        let dir = std::env::temp_dir().join("carrier-connection-test");
        match maho_core::brand::env_value("RPC_SOCKET", &maho_core::config::current_env())
            .filter(|value| !value.is_empty())
        {
            Some(socket) => assert_eq!(carrier_socket(&dir), socket),
            None => assert_eq!(
                carrier_socket(&dir),
                format!("{}/rpc/rpc.sock", dir.display())
            ),
        }
    }

    #[test]
    fn an_explicit_socket_wins_over_the_agent_dir() {
        let dir = std::env::temp_dir().join("carrier-connection-explicit");
        assert_eq!(
            resolve_host_socket(Some("/tmp/explicit.sock"), &dir),
            "/tmp/explicit.sock"
        );
    }
}

#[cfg(all(test, unix))]
mod socket_tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::*;

    /// A host on a real socket: it answers the identity probe, then one reply per frame, echoing each frame's `id`.
    fn serve(path: &Path, frames: Vec<Value>) -> std::thread::JoinHandle<Vec<Value>> {
        let listener = UnixListener::bind(path).expect("the fake host binds its socket");
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the carrier dials the fake host");
            let mut writer = stream.try_clone().expect("the fake host clones its stream");
            let mut reader = BufReader::new(stream);
            let mut seen = Vec::new();
            answer(
                &mut writer,
                &mut reader,
                json!({ "success": true, "data": {} }),
                &mut seen,
            );
            for frame in frames {
                answer(&mut writer, &mut reader, frame, &mut seen);
            }
            seen
        })
    }

    fn answer(
        writer: &mut impl Write,
        reader: &mut impl BufRead,
        reply: Value,
        seen: &mut Vec<Value>,
    ) {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the carrier sends a frame");
        let frame: Value = serde_json::from_str(line.trim()).expect("the frame is json");
        let mut reply = reply;
        reply["id"] = frame.get("id").cloned().unwrap_or(Value::Null);
        writeln!(writer, "{reply}").expect("the fake host answers");
        writer.flush().expect("the fake host flushes");
        seen.push(frame);
    }

    fn socket_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the test makes its socket directory");
        dir
    }

    #[test]
    fn request_answers_the_hosts_reply_to_its_own_frame() {
        let dir = socket_dir("carrier-request");
        let path = dir.join("rpc.sock");
        let _ = std::fs::remove_file(&path);
        let server = serve(
            &path,
            vec![json!({ "success": true, "data": { "sessionId": "s-1", "attached": true } })],
        );
        let connect = carrier_connect(path.to_string_lossy().into_owned(), dir.clone());
        let connection = connect().expect("the carrier connects");
        let reply = connection
            .request(json!({ "type": "open_session", "sessionPath": "/p", "cwd": "/c" }))
            .expect("the host answers");
        assert_eq!(reply.get("success"), Some(&Value::Bool(true)));
        assert_eq!(
            reply.get("data").and_then(|data| data.get("sessionId")),
            Some(&Value::String("s-1".to_owned()))
        );
        let seen = server.join().expect("the fake host finishes");
        // The identity probe comes first; the carrier's own frame follows it on the same connection.
        assert_eq!(
            seen.first().and_then(|frame| frame.get("type")),
            Some(&Value::String("get_protocol_info".to_owned()))
        );
        assert_eq!(
            seen.get(1).and_then(|frame| frame.get("type")),
            Some(&Value::String("open_session".to_owned()))
        );
        connection.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_host_that_is_not_there_is_host_unavailable() {
        let dir = socket_dir("carrier-unavailable");
        let path = dir.join("missing.sock");
        let _ = std::fs::remove_file(&path);
        let connect = carrier_connect(path.to_string_lossy().into_owned(), dir.clone());
        let connection = connect().expect("the connection object opens without a host");
        let error = connection
            .request(json!({ "type": "wake" }))
            .expect_err("no host answers");
        assert!(
            error.starts_with(HOST_UNAVAILABLE),
            "the failure names the host: {error}"
        );
        connection.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_closed_connection_answers_no_frame() {
        let dir = socket_dir("carrier-closed");
        let path = dir.join("rpc.sock");
        let _ = std::fs::remove_file(&path);
        let connect = carrier_connect(path.to_string_lossy().into_owned(), dir.clone());
        let connection = connect().expect("the carrier connects");
        connection.close();
        assert_eq!(
            connection.request(json!({ "type": "wake" })),
            Err(CONNECTION_CLOSED.to_owned())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn on_closed_runs_when_the_connection_closes() {
        let dir = socket_dir("carrier-on-closed");
        let path = dir.join("rpc.sock");
        let _ = std::fs::remove_file(&path);
        let connect = carrier_connect(path.to_string_lossy().into_owned(), dir.clone());
        let connection = connect().expect("the carrier connects");
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        connection.on_closed(Box::new(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        // A registered callback waits for the close; only then does the connection count as ended.
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        connection.close();
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn on_closed_after_a_close_runs_at_once() {
        let dir = socket_dir("carrier-on-closed-late");
        let path = dir.join("rpc.sock");
        let _ = std::fs::remove_file(&path);
        let connect = carrier_connect(path.to_string_lossy().into_owned(), dir.clone());
        let connection = connect().expect("the carrier connects");
        connection.close();
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        connection.on_closed(Box::new(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unsubscribe_leaves_the_connections_listeners_empty() {
        let dir = socket_dir("carrier-unsubscribe");
        let path = dir.join("rpc.sock");
        let _ = std::fs::remove_file(&path);
        let connection = super::host_connection::HostConnection::connect(
            path.to_string_lossy().into_owned(),
            dir.clone(),
            5_000,
        )
        .expect("the connection opens");
        let unsub = connection.on_event(std::sync::Arc::new(|_| {}));
        assert_eq!(connection.listener_count(), 1);
        unsub();
        assert_eq!(connection.listener_count(), 0);
        connection.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
