//! One kept connection to the engine host: the carrier's own attachment to a woken session lives on it (A3.1.3).
//!
//! Upstream counterpart: `thread/wake/carrier-connection.ts`. The socket half (`connectCarrierHost` over senpi-task's
//! authenticated `rpc-socket-client`) has no Rust counterpart in this crate yet; `HostConnection` binds one over the
//! existing authenticated host transport (`maho_rpc::socket_client::RpcSocket`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

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

#[cfg(unix)]
mod host_connection {
    use super::*;
    use maho_rpc::socket_client::RpcSocket;
    use tokio::runtime::Handle;

    /// How long the carrier waits for one host reply to a `open_session`/`wake`/`release_session` frame.
    const REQUEST_TIMEOUT_MS: u64 = 30_000;

    /// One authenticated connection per request: the carrier holds no long-lived socket; each `request` dials,
    /// authenticates, asks and closes, mirroring the `RpcTransport` one-request wire but kept as one connection per
    /// call because the carrier never leaves a session's attachment open.
    pub(super) struct HostConnection {
        socket: String,
        timeout_ms: u64,
        listener: Mutex<Option<CarrierFrameListener>>,
        closed: Mutex<Option<ConnectionClosed>>,
    }

    impl HostConnection {
        pub(super) fn connect(socket: String, timeout_ms: u64) -> Result<Arc<Self>, String> {
            Ok(Arc::new(Self {
                socket,
                timeout_ms,
                listener: Mutex::default(),
                closed: Mutex::default(),
            }))
        }

        fn runtime() -> Handle {
            Handle::try_current().unwrap_or_else(|_| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime")
            })
        }

        async fn authenticated(socket: &str, timeout_ms: u64) -> Result<RpcSocket, String> {
            RpcSocket::connect(socket, std::time::Duration::from_millis(timeout_ms))
                .await
                .map_err(|error| format!("{error}:{socket}"))
        }
    }

    impl CarrierHostConnection for HostConnection {
        fn request(&self, frame: Value) -> Result<Value, String> {
            let rt = Self::runtime();
            rt.block_on(async move {
                let socket = Self::authenticated(&self.socket, self.timeout_ms).await?;
                let id = frame
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let mut frame = frame;
                frame["id"] = json!(id);
                socket
                    .request(frame)
                    .await
                    .map_err(|error| error.to_string())
            })
        }

        fn on_event(&self, listener: CarrierFrameListener) -> Unsubscribe {
            let mut slot = self.listener.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.is_some() {
                return Box::new(|| {});
            }
            *slot = Some(listener);
            Box::new(move || {
                let mut slot = self.listener.lock().unwrap_or_else(PoisonError::into_inner);
                *slot = None;
            })
        }

        fn on_closed(&self, callback: ConnectionClosed) {
            let mut slot = self.closed.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.is_some() {
                callback();
                return;
            }
            *slot = Some(callback);
        }

        fn close(&self) {}
    }
}

/// `resolveThreadSocket` on this host: `--socket`, then `RPC_SOCKET`, then `<agentDir>/rpc/rpc.sock`.
pub fn resolve_host_socket(explicit: Option<&str>, agent_dir: &std::path::Path) -> String {
    explicit.map(str::to_owned).unwrap_or_else(|| {
        maho_core::config::env_value("RPC_SOCKET", &maho_core::config::current_env())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                agent_dir
                    .join("rpc/rpc.sock")
                    .to_string_lossy()
                    .into_owned()
            })
    })
}

/// The carrier's socket for an agent directory: the operator host's socket the thread surface opens on.
pub fn carrier_socket(agent_dir: &std::path::Path) -> String {
    resolve_host_socket(None, agent_dir)
}

/// The host's idle-exit read: `task.host_idle_exit_ms` is not exposed on the bridge yet, so the carrier's ensure
/// runs with `--policy never` and no idle-exit override (owner ruling 2026-11-14).
pub fn host_idle_exit_ms(_env: &std::collections::BTreeMap<String, String>) -> Option<u64> {
    None
}

/// The carrier's engine-host connector for an agent directory: one authenticated `HostConnection` per open, the
/// socket the carrier opens sessions on.
pub fn carrier_connect(socket: String, timeout_ms: u64) -> CarrierConnect {
    Arc::new(move || -> Result<Arc<dyn CarrierHostConnection>, String> {
        #[cfg(unix)]
        return host_connection::HostConnection::connect(socket.clone(), timeout_ms);
        #[cfg(not(unix))]
        let _ = (socket, timeout_ms);
        Err("host_unavailable:socket".to_owned())
    })
}

/// A frame's `id`: absent frames get one so every request is correlated.
pub fn frame_id(frame: &Value) -> String {
    frame
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

#[cfg(test)]
mod tests {
    use super::host_connection::HostConnection;
    use super::*;

    #[test]
    fn carrier_socket_derives_the_agent_dirs_rpc_socket() {
        let dir = std::env::temp_dir().join("carrier-connection-test");
        assert_eq!(
            carrier_socket(&dir),
            format!("{}/rpc/rpc.sock", dir.display())
        );
    }

    #[test]
    fn host_connection_connect_keeps_the_socket() {
        let connection = HostConnection::connect("/tmp/does-not-exist.sock", 30_000).unwrap();
        assert_eq!(connection.socket, "/tmp/does-not-exist.sock");
    }

    #[test]
    fn on_event_subscribes_and_unsubscribes() {
        let connection = HostConnection::connect("/tmp/s.sock", 30_000).unwrap();
        let unsub = connection.on_event(Arc::new(|_| {}));
        // No frames arrive, so nothing fires.
        drop(unsub);
    }

    #[test]
    fn on_closed_fires_immediately_when_not_connected() {
        let connection = HostConnection::connect("/tmp/s.sock", 30_000).unwrap();
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let clone = called.clone();
        connection.on_closed(Box::new(move || {
            clone.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
