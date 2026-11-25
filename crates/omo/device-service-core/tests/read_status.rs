use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use device_service_core::hand_device_keys::{BoxFuture, StatusClient};
use device_service_core::local_api::LocalApiError;
use device_service_core::{
    CancelController, CancelSignal, DeviceServiceStatus, device_service_endpoint_path,
    read_device_service_status_with,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

fn seed_endpoint(state_dir: &Path) {
    let path = device_service_endpoint_path(state_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, "{}").expect("seed endpoint");
}

struct DummyClient {
    closed_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl StatusClient for DummyClient {
    fn status(&self) -> BoxFuture<'_, Result<Value, LocalApiError>> {
        Box::pin(async {
            Ok(json!({
                "state": "connected",
                "since_ms": 1,
                "device": null,
                "origin_configured": true,
                "enabled": true,
            }))
        })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        let tx = self.closed_tx.lock().expect("lock").take();
        Box::pin(async move {
            if let Some(tx) = tx {
                let _ = tx.send(());
            }
        })
    }
}

#[tokio::test]
async fn state_dir_with_no_endpoint_file_answers_not_running_without_connecting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let connects = Arc::new(AtomicUsize::new(0));
    let connects_ref = Arc::clone(&connects);
    let outcome = read_device_service_status_with(dir.path(), None, move || async move {
        connects_ref.fetch_add(1, Ordering::SeqCst);
        Err::<DummyClient, _>(LocalApiError::new("internal", "must not connect"))
    })
    .await
    .expect("read status");
    assert_eq!(outcome, DeviceServiceStatus::NotRunning);
    assert_eq!(connects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pre_aborted_signal_answers_no_answer_without_connecting() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_endpoint(dir.path());
    let connects = Arc::new(AtomicUsize::new(0));
    let connects_ref = Arc::clone(&connects);
    let outcome = read_device_service_status_with(
        dir.path(),
        Some(CancelSignal::aborted()),
        move || async move {
            connects_ref.fetch_add(1, Ordering::SeqCst);
            Err::<DummyClient, _>(LocalApiError::new("internal", "must not connect"))
        },
    )
    .await
    .expect("read status");
    assert_eq!(outcome, DeviceServiceStatus::NoAnswer);
    assert_eq!(connects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn connection_opening_after_abort_answers_no_answer_and_closes_late_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_endpoint(dir.path());
    let controller = CancelController::new();
    let signal = controller.signal();
    let (late_tx, late_rx) = oneshot::channel::<DummyClient>();
    let (closed_tx, closed_rx) = oneshot::channel::<()>();
    let connect_entered = Arc::new(Notify::new());
    let connect_entered_ref = Arc::clone(&connect_entered);
    let state_dir = dir.path().to_path_buf();

    let handle = tokio::spawn(async move {
        read_device_service_status_with(&state_dir, Some(signal), move || async move {
            connect_entered_ref.notify_one();
            late_rx
                .await
                .map_err(|_| LocalApiError::new("closed", "late sender dropped"))
        })
        .await
    });

    connect_entered.notified().await;
    controller.cancel();
    let answer = handle.await.expect("join").expect("read status");
    assert_eq!(answer, DeviceServiceStatus::NoAnswer);

    if late_tx
        .send(DummyClient {
            closed_tx: Mutex::new(Some(closed_tx)),
        })
        .is_err()
    {
        panic!("send late client");
    }
    tokio::time::timeout(Duration::from_secs(2), closed_rx)
        .await
        .expect("close timeout")
        .expect("closed signal");
}

#[tokio::test]
async fn connect_timeout_and_dead_endpoint_map_cleanly_while_insecure_propagates() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_endpoint(dir.path());

    let timed_out = read_device_service_status_with(dir.path(), None, || async {
        Err::<DummyClient, _>(LocalApiError::new("handshake_timeout", "slow"))
    })
    .await
    .expect("timeout");
    assert_eq!(timed_out, DeviceServiceStatus::NoAnswer);

    let dead = read_device_service_status_with(dir.path(), None, || async {
        Err::<DummyClient, _>(LocalApiError::new("service_not_running", "refused"))
    })
    .await
    .expect("dead");
    assert_eq!(dead, DeviceServiceStatus::NotRunning);

    let refused = read_device_service_status_with(dir.path(), None, || async {
        Err::<DummyClient, _>(LocalApiError::new("server_proof_invalid", "bad proof"))
    })
    .await;
    assert_eq!(refused.expect_err("refused").code, "server_proof_invalid");
}

#[cfg(unix)]
mod unix_tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use device_service_core::local_api::LocalApiError;
    use device_service_core::local_api::access::{
        EntryKind, SOCKET_FILE, UNIX_ENDPOINT, ensure_private_dir, posix_secure_file, write_token,
    };
    use device_service_core::local_api::framing::encode_line;
    use device_service_core::local_api::proof::{
        client_proof, mint_nonce, proof_matches, server_proof,
    };
    use device_service_core::local_api::protocol::{Status, Welcome, error_response, ok_response};
    use device_service_core::{DeviceServiceStatus, read_device_service_status};
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::task::JoinHandle;

    fn private_state_dir() -> (tempfile::TempDir, PathBuf, Vec<u8>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("device-service");
        ensure_private_dir(&state_dir).expect("private dir");
        let token = write_token(&state_dir).expect("token");
        (dir, state_dir, token)
    }

    fn fake_owner(state_dir: &Path, token: Vec<u8>) -> JoinHandle<()> {
        let socket = state_dir.join(SOCKET_FILE);
        let listener = UnixListener::bind(&socket).expect("bind");
        posix_secure_file(&socket, EntryKind::Socket).expect("private socket");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let hello_line = lines.next_line().await.expect("read").expect("hello");
            let hello: Value = serde_json::from_str(&hello_line).expect("json");
            let client_nonce = hello["client_nonce"].as_str().expect("client nonce");
            let server_nonce = mint_nonce().expect("nonce");
            let welcome = Welcome {
                v: 1,
                service_version: "0.1.27".into(),
                owner: "service".into(),
                server_proof: server_proof(&token, client_nonce, UNIX_ENDPOINT).expect("proof"),
                server_nonce: server_nonce.clone(),
                capabilities: vec!["sessions".into()],
                state: json!({ "state": "connected" }),
            };
            let line = encode_line(&welcome.to_message()).expect("line");
            write.write_all(&line).await.expect("write");
            while let Ok(Some(line)) = lines.next_line().await {
                let request: Value = serde_json::from_str(&line).expect("json");
                let id = request["id"].as_str().unwrap_or_default().to_owned();
                let answer = match request["type"].as_str() {
                    Some("authenticate") => {
                        let expected =
                            client_proof(&token, &server_nonce, UNIX_ENDPOINT).expect("proof");
                        let offered = request["client_proof"].as_str().unwrap_or_default();
                        if proof_matches(&expected, offered) {
                            ok_response(&id, json!({}))
                        } else {
                            error_response(
                                &id,
                                &LocalApiError::new("client_proof_invalid", "wrong proof"),
                            )
                        }
                    }
                    Some("status.get") => ok_response(
                        &id,
                        json!({
                            "state": "signed_out",
                            "since_ms": 42,
                            "device": null,
                            "origin_configured": false,
                            "enabled": true,
                        }),
                    ),
                    _ => error_response(&id, &LocalApiError::new("unknown_type", "unknown")),
                };
                let line = encode_line(&answer).expect("line");
                if write.write_all(&line).await.is_err() {
                    break;
                }
            }
        })
    }

    #[tokio::test]
    async fn live_service_returns_status_and_dead_socket_returns_not_running() {
        let (_dir, state_dir, token) = private_state_dir();
        let owner = fake_owner(&state_dir, token);
        let outcome = read_device_service_status(&state_dir, "0.0.0", None)
            .await
            .expect("live status");
        assert_eq!(
            outcome,
            DeviceServiceStatus::Status(Status {
                state: "signed_out".into(),
                since_ms: 42,
                detail: None,
                device: None,
                origin_configured: false,
                enabled: true,
            })
        );
        owner.await.expect("owner");

        let stale_listener = UnixListener::bind(state_dir.join("dead.sock")).expect("bind");
        drop(stale_listener);
        std::fs::rename(state_dir.join("dead.sock"), state_dir.join(SOCKET_FILE)).expect("rename");
        posix_secure_file(&state_dir.join(SOCKET_FILE), EntryKind::Socket).expect("secure");
        let dead = read_device_service_status(&state_dir, "0.0.0", None)
            .await
            .expect("dead socket");
        assert_eq!(dead, DeviceServiceStatus::NotRunning);

        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let insecure = read_device_service_status(&state_dir, "0.0.0", None)
            .await
            .expect_err("insecure dir");
        assert_eq!(insecure.code, "insecure_state_dir");
    }
}
