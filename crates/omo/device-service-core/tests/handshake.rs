#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use device_service_core::local_api::access::{
    EntryKind, SOCKET_FILE, TOKEN_FILE, UNIX_ENDPOINT, ensure_private_dir, posix_secure_file,
    write_token,
};
use device_service_core::local_api::framing::encode_line;
use device_service_core::local_api::proof::{
    client_proof, mint_nonce, proof_matches, server_proof,
};
use device_service_core::local_api::protocol::{ClientInfo, Welcome, error_response, ok_response};
use device_service_core::local_api::transport::is_service_not_running;
use device_service_core::local_api::{ConnectOptions, LocalApiError, connect_local_api};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

#[derive(Clone, Copy)]
enum Prove {
    ThisBootToken,
    StaleToken,
}

fn private_state_dir() -> (tempfile::TempDir, PathBuf, Vec<u8>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("device-service");
    ensure_private_dir(&state_dir).expect("private dir");
    let token = write_token(&state_dir).expect("token");
    (dir, state_dir, token)
}

/// A minimal owner: proves with `prove`, checks the client proof, answers `status.get`, and returns
/// every line the client sent.
fn fake_owner(state_dir: &Path, token: Vec<u8>, prove: Prove) -> JoinHandle<Vec<Value>> {
    let socket = state_dir.join(SOCKET_FILE);
    let listener = UnixListener::bind(&socket).expect("bind");
    posix_secure_file(&socket, EntryKind::Socket).expect("private socket");
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let mut seen = Vec::new();
        let hello_line = lines.next_line().await.expect("read").expect("hello");
        let hello: Value = serde_json::from_str(&hello_line).expect("json");
        seen.push(hello.clone());
        let proving = match prove {
            Prove::ThisBootToken => token.clone(),
            Prove::StaleToken => vec![0x42; 32],
        };
        let client_nonce = hello["client_nonce"].as_str().expect("client nonce");
        let server_nonce = mint_nonce().expect("nonce");
        let welcome = Welcome {
            v: 1,
            service_version: "0.0.0-test".into(),
            owner: "service".into(),
            server_proof: server_proof(&proving, client_nonce, UNIX_ENDPOINT).expect("proof"),
            server_nonce: server_nonce.clone(),
            capabilities: vec!["sessions".into(), "devices".into()],
            state: json!({ "state": "connected" }),
        };
        let line = encode_line(&welcome.to_message()).expect("line");
        write.write_all(&line).await.expect("write");
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).expect("json");
            seen.push(request.clone());
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
                        "state": "connected",
                        "since_ms": 1,
                        "device": null,
                        "origin_configured": true,
                        "enabled": true,
                    }),
                ),
                _ => error_response(&id, &LocalApiError::new("unknown_type", "unknown request")),
            };
            let line = encode_line(&answer).expect("line");
            if write.write_all(&line).await.is_err() {
                break;
            }
        }
        seen
    })
}

fn options(state_dir: &Path) -> ConnectOptions {
    ConnectOptions::new(state_dir, ClientInfo::cli("0.0.0"))
}

#[tokio::test]
async fn a_proven_owner_is_authenticated_before_any_request() {
    let (_dir, state_dir, token) = private_state_dir();
    let owner = fake_owner(&state_dir, token, Prove::ThisBootToken);
    let client = connect_local_api(options(&state_dir))
        .await
        .expect("connect");
    assert_eq!(client.service_version(), "0.0.0-test");
    assert!(client.has_capability("devices"));
    assert_eq!(client.status().await.expect("status")["state"], "connected");
    client.close().await;
    let seen = owner.await.expect("owner");
    let types: Vec<&str> = seen
        .iter()
        .filter_map(|line| line["type"].as_str())
        .collect();
    assert_eq!(types, vec!["hello", "authenticate", "status.get"]);
    assert_eq!(seen[0]["v"], 1);
    assert_eq!(seen[0]["client"]["name"], "omo");
}

#[tokio::test]
async fn an_owner_proving_a_stale_token_is_refused_and_gets_no_request() {
    let (_dir, state_dir, token) = private_state_dir();
    let owner = fake_owner(&state_dir, token, Prove::StaleToken);
    let error = connect_local_api(options(&state_dir))
        .await
        .err()
        .expect("a stale proof is refused");
    assert_eq!(error.code, "server_proof_invalid");
    let seen = owner.await.expect("owner");
    assert_eq!(seen.len(), 1, "the client sent nothing but its hello");
}

#[tokio::test]
async fn an_insecure_state_dir_is_refused_before_connecting() {
    let (_dir, state_dir, _token) = private_state_dir();
    let missing = connect_local_api(options(&state_dir))
        .await
        .err()
        .expect("no socket");
    assert_eq!(missing.code, "insecure_state_dir");
    assert!(is_service_not_running(&missing));

    let _listener = UnixListener::bind(state_dir.join(SOCKET_FILE)).expect("bind");
    posix_secure_file(&state_dir.join(SOCKET_FILE), EntryKind::Socket).expect("socket");
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let open = connect_local_api(options(&state_dir))
        .await
        .err()
        .expect("open dir");
    assert_eq!(open.code, "insecure_state_dir");
    assert!(!is_service_not_running(&open));

    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let token = state_dir.join(TOKEN_FILE);
    let real = state_dir.join("real-token");
    std::fs::rename(&token, &real).expect("move");
    std::os::unix::fs::symlink(&real, &token).expect("symlink");
    let linked = connect_local_api(options(&state_dir))
        .await
        .err()
        .expect("symlinked token");
    assert_eq!(linked.code, "insecure_state_dir");
}
