//! Ported from upstream `thread/live-surface.ts` `dialFor` on a `tui` endpoint: a terminal control
//! socket authenticates with the 32 bytes in `<socket>.secret`, written before the first request.

#[cfg(unix)]
mod unix {
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use maho_omo_gateway::thread::attachment_fence::RpcTransport;
    use maho_omo_gateway::thread::endpoint_registry::control_socket_secret_path;
    use maho_omo_gateway::thread::native::{NativeThreadHost, ThreadHostWire};
    use maho_omo_remote::thread_host::native_thread_host;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn a_tui_endpoint_sends_the_32_secret_bytes_before_the_first_request() {
        let dir = tempdir().unwrap();
        let socket = dir.path().join("t-0123456789abcdef.sock");
        let secret_path = PathBuf::from(control_socket_secret_path(socket.to_str().unwrap()));
        std::fs::write(&secret_path, [7u8; 32]).unwrap();

        let bytes = Arc::new(Mutex::new(Vec::new()));
        let received = bytes.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            ready_tx.send(()).unwrap();
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut secret = [0u8; 32];
            std::io::Read::read_exact(&mut reader, &mut secret).unwrap();
            received.lock().unwrap().extend_from_slice(&secret);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            received.lock().unwrap().extend_from_slice(line.as_bytes());
        });
        ready_rx.recv().unwrap();

        let session: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync> =
            Arc::new(|| None);
        let host: NativeThreadHost = native_thread_host(session, dir.path());
        let wire: ThreadHostWire = host.wire.expect("wire");
        let outcome = wire
            .transport
            .request(
                socket.to_str().unwrap(),
                json!({ "type": "list_sessions" }),
                None,
            )
            .await;
        // The fake endpoint does not answer; the handshake bytes must already have been read.
        assert!(outcome.is_err());

        let received = received.lock().unwrap();
        // 32 secret bytes first; only a JSON line after them, never an rpc-host auth challenge.
        assert_eq!(received[..32], [7u8; 32]);
        assert!(received[32..].starts_with(b"{"));
        drop(received);
        let _ = server.join();
    }
}

use maho_omo_remote::thread_host::native_thread_host;
