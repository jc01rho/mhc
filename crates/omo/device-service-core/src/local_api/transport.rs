//! Port of `local-api/transport.ts`: a Unix socket `<state dir>/service.sock` on POSIX; on Windows a
//! loopback TCP listener whose port is recorded in `<state dir>/endpoint.json`.

use std::path::Path;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

use super::access::tcp_endpoint;
use super::errors::LocalApiError;

pub const ENDPOINT_FILE: &str = "endpoint.json";
/// The client error for an endpoint that is not there or does not accept: the service is not running.
pub const SERVICE_NOT_RUNNING: &str = "service_not_running";

pub type BoxedReader = Box<dyn AsyncRead + Unpin + Send>;
pub type BoxedWriter = Box<dyn AsyncWrite + Unpin + Send>;

/// One connection's byte halves and the endpoint the proofs are bound to.
pub struct LocalTransport {
    pub endpoint: String,
    pub reader: BoxedReader,
    pub writer: BoxedWriter,
}

impl LocalTransport {
    pub fn new<S>(endpoint: impl Into<String>, stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            endpoint: endpoint.into(),
            reader: Box::new(reader),
            writer: Box::new(writer),
        }
    }
}

pub fn read_endpoint_port(state_dir: &Path) -> Result<u16, LocalApiError> {
    let no_port = || LocalApiError::new("insecure_state_dir", "endpoint.json holds no port");
    let text = std::fs::read_to_string(state_dir.join(ENDPOINT_FILE)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LocalApiError::new(
                SERVICE_NOT_RUNNING,
                "the device service has no endpoint file",
            )
        } else {
            LocalApiError::new("read_failed", error.to_string())
        }
    })?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| no_port())?;
    parsed
        .get("port")
        .and_then(Value::as_u64)
        .filter(|port| (1..=65_535).contains(port))
        .and_then(|port| u16::try_from(port).ok())
        .ok_or_else(no_port)
}

fn not_running(error: &std::io::Error) -> LocalApiError {
    LocalApiError::new(
        SERVICE_NOT_RUNNING,
        format!("the device service did not accept a connection: {error}"),
    )
}

/// Opens the platform endpoint in `state_dir`.
pub async fn connect_local(state_dir: &Path) -> Result<LocalTransport, LocalApiError> {
    #[cfg(unix)]
    {
        let stream = tokio::net::UnixStream::connect(state_dir.join(super::access::SOCKET_FILE))
            .await
            .map_err(|error| not_running(&error))?;
        Ok(LocalTransport::new(super::access::UNIX_ENDPOINT, stream))
    }
    #[cfg(not(unix))]
    {
        let port = read_endpoint_port(state_dir)?;
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|error| not_running(&error))?;
        Ok(LocalTransport::new(tcp_endpoint(port), stream))
    }
}

/// Whether `error` means the device service is not there (no endpoint, refused connection).
pub fn is_service_not_running(error: &LocalApiError) -> bool {
    error.is(SERVICE_NOT_RUNNING)
        || (error.is("insecure_state_dir") && error.reason.ends_with("is missing"))
}

/// The endpoint string a server listening on `port` proves (Windows), kept beside the client's.
pub fn loopback_endpoint(port: u16) -> String {
    tcp_endpoint(port)
}
