use std::cmp::Ordering;

use device_service_core::release::{compare_versions, is_semver};

#[test]
fn semver_parsing_and_comparison_match_upstream() {
    assert!(is_semver("0.1.11"));
    assert!(is_semver("0.0.0-test+build.1"));
    assert!(!is_semver("0.1"));
    assert!(!is_semver("0.1.11-"));
    assert!(!is_semver("0.1.11+"));

    assert_eq!(
        compare_versions("0.1.11", "0.1.11").expect("eq"),
        Ordering::Equal
    );
    assert_eq!(
        compare_versions("0.1.11", "0.1.10").expect("gt"),
        Ordering::Greater
    );
    assert_eq!(
        compare_versions("0.1.10", "0.1.11").expect("lt"),
        Ordering::Less
    );
    assert_eq!(
        compare_versions("0.1.11-rc.1", "0.1.11").expect("pre lt release"),
        Ordering::Less
    );
    assert_eq!(
        compare_versions("0.1.11-alpha.2", "0.1.11-alpha.10").expect("numeric pre"),
        Ordering::Less
    );
    assert_eq!(
        compare_versions("0.1.11-1", "0.1.11-alpha").expect("numeric lt alpha"),
        Ordering::Less
    );
    assert_eq!(
        compare_versions("0.1.11+build.1", "0.1.11+build.2").expect("ignore build"),
        Ordering::Equal
    );
    assert!(compare_versions("not-semver", "0.1.11").is_err());
}

#[cfg(unix)]
mod unix_tests {
    use std::path::{Path, PathBuf};

    use device_service_core::EngineEndpoints;
    use device_service_core::local_api::access::{
        EntryKind, SOCKET_FILE, UNIX_ENDPOINT, ensure_private_dir, posix_secure_file, write_token,
    };
    use device_service_core::local_api::framing::encode_line;
    use device_service_core::local_api::proof::{
        client_proof, mint_nonce, proof_matches, server_proof,
    };
    use device_service_core::local_api::protocol::{
        ClientInfo, Welcome, error_response, ok_response,
    };
    use device_service_core::local_api::{ConnectOptions, LocalApiError, connect_local_api};
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

    fn fake_owner(
        state_dir: &Path,
        token: Vec<u8>,
        service_version: &'static str,
        endpoints_reply: Value,
    ) -> JoinHandle<Vec<Value>> {
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
            let client_nonce = hello["client_nonce"].as_str().expect("client nonce");
            let server_nonce = mint_nonce().expect("nonce");
            let welcome = Welcome {
                v: 1,
                service_version: service_version.into(),
                owner: "service".into(),
                server_proof: server_proof(&token, client_nonce, UNIX_ENDPOINT).expect("proof"),
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
                    Some("session.endpoints") => ok_response(&id, endpoints_reply.clone()),
                    _ => error_response(&id, &LocalApiError::new("unknown_type", "unknown")),
                };
                let line = encode_line(&answer).expect("line");
                if write.write_all(&line).await.is_err() {
                    break;
                }
            }
            seen
        })
    }

    #[tokio::test]
    async fn engine_endpoints_asks_primary_on_0_1_11_plus_and_preserves_primary_key_when_last() {
        let (_dir, state_dir, token) = private_state_dir();
        let owner = fake_owner(
            &state_dir,
            token,
            "0.1.11",
            json!({
                "endpoints": ["OTHER_DIR", "SHARD", "PRIMARY"],
                "primary": "PRIMARY",
            }),
        );
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");
        let endpoints = client.engine_endpoints("dev-1").await.expect("endpoints");
        assert_eq!(
            endpoints,
            EngineEndpoints {
                keys: vec!["OTHER_DIR".into(), "SHARD".into(), "PRIMARY".into()],
                primary: Some("PRIMARY".into()),
            }
        );
        client.close().await;
        let seen = owner.await.expect("owner");
        let req = seen
            .iter()
            .find(|msg| msg["type"] == "session.endpoints")
            .expect("session.endpoints request");
        assert_eq!(req["device"], "dev-1");
        assert_eq!(req["primary"], true);
    }

    #[tokio::test]
    async fn engine_endpoints_omits_primary_flag_before_0_1_11() {
        let (_dir, state_dir, token) = private_state_dir();
        let owner = fake_owner(
            &state_dir,
            token,
            "0.1.10",
            json!({
                "endpoints": ["ONLY"],
            }),
        );
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");
        let endpoints = client.engine_endpoints("dev-1").await.expect("endpoints");
        assert_eq!(
            endpoints,
            EngineEndpoints {
                keys: vec!["ONLY".into()],
                primary: None,
            }
        );
        client.close().await;
        let seen = owner.await.expect("owner");
        let req = seen
            .iter()
            .find(|msg| msg["type"] == "session.endpoints")
            .expect("session.endpoints request");
        assert_eq!(req["device"], "dev-1");
        assert!(req.get("primary").is_none());
    }

    #[tokio::test]
    async fn engine_endpoints_handles_absent_primary_on_0_1_11_plus() {
        let (_dir, state_dir, token) = private_state_dir();
        let owner = fake_owner(
            &state_dir,
            token,
            "0.1.27",
            json!({
                "endpoints": ["A", "B"],
            }),
        );
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");
        let endpoints = client.engine_endpoints("dev-1").await.expect("endpoints");
        assert_eq!(
            endpoints,
            EngineEndpoints {
                keys: vec!["A".into(), "B".into()],
                primary: None,
            }
        );
        client.close().await;
        let _ = owner.await.expect("owner");
    }
}
