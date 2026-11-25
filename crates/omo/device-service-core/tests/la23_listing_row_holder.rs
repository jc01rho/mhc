use device_service_core::local_api::access::{SecureOrigin, make_dir};
use device_service_core::local_api::protocol::{
    ListingRow, ListingWait, ListingWaitKind, Status, validate_listing_row,
    validate_sessions_list_rows,
};
use device_service_core::local_api::vocabulary::COMMAND_REFUSAL_REASONS;
use serde_json::{Value, json};

fn base_row() -> Value {
    json!({
        "device": "dev-peer-1",
        "session_id": "c2Vzc2lvbi1rZXktMDAwMQ",
        "title": "Test session",
        "project": "omo",
        "path": "/home/user/omo",
        "state": "running",
        "updated_ms": 1_790_000_000_000_i64,
        "source": "live",
        "as_of": 1_790_000_100_000_i64,
    })
}

fn valid_waiting() -> Value {
    json!({
        "kind": "retry",
        "attempt": 1,
        "maxAttempts": 3,
        "delayMs": 0,
        "until": "2026-11-21T01:30:00.000Z",
    })
}

#[test]
fn row_with_waiting_and_128_char_engine_version_validates_and_round_trips() {
    let mut row = base_row();
    row["waiting"] = valid_waiting();
    row["engine_version"] = json!("v".repeat(128));
    let parsed = validate_listing_row(&row).expect("valid row");
    assert_eq!(
        parsed.waiting,
        Some(ListingWait {
            kind: ListingWaitKind::Retry,
            attempt: 1,
            max_attempts: 3,
            delay_ms: 0,
            until: "2026-11-21T01:30:00.000Z".into(),
        })
    );
    assert_eq!(
        parsed.engine_version.as_deref(),
        Some("v".repeat(128).as_str())
    );

    let round_tripped = serde_json::to_value(&parsed).expect("serialize");
    let reparsed = validate_listing_row(&round_tripped).expect("revalidate");
    assert_eq!(reparsed, parsed);
    assert_eq!(ListingRow::parse(&round_tripped), Some(parsed));
}

#[test]
fn out_of_bounds_engine_version_is_refused() {
    for bad in [json!(""), json!("v".repeat(129)), json!(7), Value::Null] {
        let mut row = base_row();
        row["engine_version"] = bad;
        assert!(validate_listing_row(&row).is_err());
    }
}

#[test]
fn malformed_waiting_is_refused() {
    let malformed = vec![
        Value::Null,
        json!("rate_limit"),
        json!({ "kind": "backoff", "attempt": 1, "maxAttempts": 3, "delayMs": 0, "until": "2026-11-21T01:30:00.000Z" }),
        json!({ "kind": "retry", "attempt": 4, "maxAttempts": 3, "delayMs": 0, "until": "2026-11-21T01:30:00.000Z" }),
        json!({ "kind": "retry", "attempt": 0, "maxAttempts": 3, "delayMs": 0, "until": "2026-11-21T01:30:00.000Z" }),
        json!({ "kind": "retry", "attempt": 0, "maxAttempts": 0, "delayMs": 0, "until": "2026-11-21T01:30:00.000Z" }),
        json!({ "kind": "retry", "attempt": 1, "maxAttempts": 3, "delayMs": -1, "until": "2026-11-21T01:30:00.000Z" }),
        json!({ "kind": "retry", "attempt": 1, "maxAttempts": 3, "delayMs": 0, "until": "2026-11-21T01:30:00Z" }),
        json!({ "kind": "retry", "attempt": 1, "maxAttempts": 3, "delayMs": 0, "until": "soon" }),
        json!({ "kind": "retry", "attempt": 1, "maxAttempts": 3, "delayMs": 0 }),
    ];
    for waiting in malformed {
        let mut row = base_row();
        row["waiting"] = waiting.clone();
        assert!(
            validate_listing_row(&row).is_err(),
            "expected malformed waiting to fail: {waiting}"
        );
    }
}

#[test]
fn sessions_list_accepts_rows_2_to_7_and_refuses_others() {
    for rows in 2..=7 {
        assert!(validate_sessions_list_rows(rows));
    }
    for rows in [0, 1, 8, 99] {
        assert!(!validate_sessions_list_rows(rows));
    }
    assert!(COMMAND_REFUSAL_REASONS.contains(&"unbound"));
}

#[test]
fn status_policy_blocker_deserializes_and_make_dir_reports_origin() {
    let status: Status = serde_json::from_value(json!({
        "state": "connected",
        "since_ms": 1,
        "device": { "id": "dev-1", "name": "Host" },
        "origin_configured": true,
        "enabled": true,
        "policy": {
            "state": "pending",
            "generation": 2,
            "this_device": "acked",
            "homes_pending": 1,
            "unacked": [{ "device": "dev-2", "label": "Laptop" }],
            "waiting": "reported",
            "blocker": {
                "device": "dev-2",
                "label": "Laptop",
                "reason": "not_acked",
                "action": "Open omo on Laptop",
            }
        }
    }))
    .expect("status with policy");
    let policy = status.policy.expect("policy");
    assert_eq!(policy.waiting.as_deref(), Some("reported"));
    assert_eq!(
        policy.blocker.as_ref().map(|b| b.reason.as_str()),
        Some("not_acked")
    );

    let root = tempfile::tempdir().expect("tempdir");
    let nested = root.path().join("a").join("state");
    assert_eq!(make_dir(&nested).expect("first"), SecureOrigin::Created);
    assert_eq!(make_dir(&nested).expect("second"), SecureOrigin::Existing);
}

#[cfg(unix)]
mod unix_tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use device_service_core::local_api::InsecureStateCause;
    use device_service_core::local_api::access::{
        EntryKind, SOCKET_FILE, TOKEN_FILE, UNIX_ENDPOINT, check_posix_access, ensure_private_dir,
        posix_secure_file, write_token,
    };
    use device_service_core::local_api::framing::encode_line;
    use device_service_core::local_api::proof::{
        client_proof, mint_id, mint_nonce, proof_matches, server_proof,
    };
    use device_service_core::local_api::protocol::{
        ClientInfo, ListingRow, Welcome, error_response, event_message, ok_response,
    };
    use device_service_core::local_api::{ConnectOptions, LocalApiError, connect_local_api};
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::task::JoinHandle;

    use super::{base_row, valid_waiting};

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
                service_version: "0.1.46".into(),
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
                let mut extra_events = Vec::new();
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
                    Some("sessions.list") => {
                        let rows = request.get("rows").and_then(Value::as_u64).unwrap_or(1);
                        let mut row = base_row();
                        if rows >= 6 {
                            row["waiting"] = valid_waiting();
                        }
                        if rows >= 7 {
                            row["engine_version"] = json!("v".repeat(128));
                        }
                        let query_id = mint_id().expect("query id");
                        extra_events.push(event_message(
                            "sessions.listing_begin",
                            json!({ "query_id": query_id, "revision": 1 }),
                        ));
                        extra_events.push(event_message(
                            "sessions.listing_rows",
                            json!({ "query_id": query_id, "revision": 1, "rows": [row] }),
                        ));
                        extra_events.push(event_message(
                            "sessions.listing_end",
                            json!({ "query_id": query_id, "revision": 1, "incomplete": [] }),
                        ));
                        extra_events.push(event_message(
                            "sessions.round",
                            json!({ "query_id": query_id, "state": "done", "devices": {} }),
                        ));
                        ok_response(&id, json!({ "query_id": query_id }))
                    }
                    _ => error_response(&id, &LocalApiError::new("unknown_type", "unknown")),
                };
                let line = encode_line(&answer).expect("line");
                if write.write_all(&line).await.is_err() {
                    break;
                }
                for event in extra_events {
                    let line = encode_line(&event).expect("event line");
                    if write.write_all(&line).await.is_err() {
                        break;
                    }
                }
            }
        })
    }

    #[tokio::test]
    async fn client_listing_at_rows_7_gets_both_fields_and_at_rows_6_wait_only() {
        let (_dir, state_dir, token) = private_state_dir();
        let owner = fake_owner(&state_dir, token);
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");

        let seventh = client.sessions_list(None, Some(7)).await.expect("rows 7");
        let row7 = ListingRow::parse(&seventh.rows[0]).expect("parse 7");
        assert!(row7.waiting.is_some());
        assert_eq!(
            row7.engine_version.as_deref(),
            Some("v".repeat(128).as_str())
        );

        let sixth = client.sessions_list(None, Some(6)).await.expect("rows 6");
        let row6 = ListingRow::parse(&sixth.rows[0]).expect("parse 6");
        assert!(row6.waiting.is_some());
        assert!(row6.engine_version.is_none());

        client.close().await;
        owner.await.expect("owner");
    }

    #[test]
    fn posix_access_populates_typed_insecure_causes() {
        let (_dir, state_dir, _token) = private_state_dir();
        let missing = check_posix_access(&state_dir).expect_err("missing socket");
        assert_eq!(missing.insecure_cause, Some(InsecureStateCause::Unreadable));

        let socket = state_dir.join(SOCKET_FILE);
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        posix_secure_file(&socket, EntryKind::Socket).expect("private socket");
        drop(listener);

        check_posix_access(&state_dir).expect("private dir is accepted");

        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let open_dir = check_posix_access(&state_dir).expect_err("open dir");
        assert_eq!(
            open_dir.insecure_cause,
            Some(InsecureStateCause::DaclNotExact)
        );

        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let token = state_dir.join(TOKEN_FILE);
        let real = state_dir.join("real-token");
        std::fs::rename(&token, &real).expect("move");
        std::os::unix::fs::symlink(&real, &token).expect("symlink");
        let linked = check_posix_access(&state_dir).expect_err("symlinked token");
        assert_eq!(linked.insecure_cause, Some(InsecureStateCause::Reparse));
    }
}
