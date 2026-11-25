use std::sync::{Arc, Mutex};

use device_service_core::listing_rows::{BoxFuture, SessionListClient};
use device_service_core::local_api::{ListingResult, LocalApiError};
use device_service_core::{NewestRowsListing, older_rows_version};
use serde_json::{Value, json};

struct RecordingListClient {
    newest: u64,
    asked: Arc<Mutex<Vec<Option<u64>>>>,
}

impl SessionListClient for RecordingListClient {
    fn list(
        &self,
        _filter: Option<Value>,
        rows: Option<u64>,
    ) -> BoxFuture<'_, Result<ListingResult, LocalApiError>> {
        self.asked.lock().expect("lock").push(rows);
        let newest = self.newest;
        Box::pin(async move {
            if rows.unwrap_or(1) > newest {
                Err(LocalApiError::new("invalid_request", "rows"))
            } else {
                Ok(ListingResult {
                    revision: 1,
                    rows: vec![json!({ "device": "dev-1", "session_id": "s1" })],
                    incomplete: Vec::new(),
                    devices: json!({}),
                    complete: true,
                })
            }
        })
    }
}

async fn rows_asked(newest: u64, listings: usize) -> Vec<Option<u64>> {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut listing = NewestRowsListing::new(RecordingListClient {
        newest,
        asked: Arc::clone(&asked),
    });
    for _ in 0..listings {
        listing.list(None).await.expect("listing");
    }
    asked.lock().expect("lock").clone()
}

#[tokio::test]
async fn steps_down_from_rows_7_one_version_per_invalid_request() {
    assert_eq!(rows_asked(7, 1).await, vec![Some(7)]);
    assert_eq!(rows_asked(6, 1).await, vec![Some(7), Some(6)]);
    assert_eq!(rows_asked(5, 1).await, vec![Some(7), Some(6), Some(5)]);
    assert_eq!(
        rows_asked(4, 1).await,
        vec![Some(7), Some(6), Some(5), Some(4)]
    );
    assert_eq!(
        rows_asked(1, 1).await,
        vec![Some(7), Some(6), Some(5), Some(4), Some(3), Some(2), None]
    );
    assert_eq!(older_rows_version(None), None);
}

#[tokio::test]
async fn remembers_accepted_rows_version_across_subsequent_listings() {
    assert_eq!(
        rows_asked(5, 3).await,
        vec![Some(7), Some(6), Some(5), Some(5), Some(5)]
    );
}

#[cfg(unix)]
mod unix_tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use device_service_core::NewestRowsListing;
    use device_service_core::local_api::access::{
        EntryKind, SOCKET_FILE, UNIX_ENDPOINT, ensure_private_dir, posix_secure_file, write_token,
    };
    use device_service_core::local_api::framing::encode_line;
    use device_service_core::local_api::proof::{
        client_proof, mint_id, mint_nonce, proof_matches, server_proof,
    };
    use device_service_core::local_api::protocol::{
        ClientInfo, ListingRow, ListingWait, ListingWaitKind, Welcome, error_response,
        event_message, ok_response,
    };
    use device_service_core::local_api::{ConnectOptions, LocalApiError, connect_local_api};
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::task::JoinHandle;

    const DURABLE_ID: &str = "01a1c608-a959-7764-a0db-94773c516e26";
    const ENGINE_VERSION: &str = "2026.11.19-3";

    fn full_row() -> Value {
        json!({
            "device": "dev-peer-1",
            "session_id": "c2Vzc2lvbi1rZXktMDAwMQ",
            "title": "Test",
            "project": "omo",
            "path": "/home/user/omo",
            "state": "running",
            "updated_ms": 1_790_000_000_000_i64,
            "source": "live",
            "as_of": 1_790_000_100_000_i64,
            "kind": "interactive",
            "durable_id": DURABLE_ID,
            "model": { "provider": "anthropic", "id": "claude-opus-5-5" },
            "archived": true,
            "waiting": {
                "kind": "rate_limit",
                "attempt": 2,
                "maxAttempts": 5,
                "delayMs": 30_000,
                "until": "2026-11-21T01:30:00.000Z",
            },
            "engine_version": ENGINE_VERSION,
        })
    }

    fn row_for_version(rows: Option<u64>) -> Value {
        let mut row = full_row();
        let obj = row.as_object_mut().expect("object");
        let version = rows.unwrap_or(1);
        if version < 2 {
            obj.remove("kind");
        }
        if version < 3 {
            obj.remove("durable_id");
        }
        if version < 4 {
            obj.remove("model");
            obj.remove("archived");
        }
        if version < 6 {
            obj.remove("waiting");
        }
        if version < 7 {
            obj.remove("engine_version");
        }
        row
    }

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
        rows_newest: u64,
        list_calls: Arc<AtomicUsize>,
        asked: Arc<Mutex<Vec<Option<u64>>>>,
    ) -> JoinHandle<()> {
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
                let mut extra_events: Vec<Value> = Vec::new();
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
                        list_calls.fetch_add(1, Ordering::SeqCst);
                        let rows_ver = request.get("rows").and_then(Value::as_u64);
                        asked.lock().expect("lock").push(rows_ver);
                        if rows_ver.unwrap_or(1) > rows_newest {
                            error_response(&id, &LocalApiError::new("invalid_request", "rows"))
                        } else {
                            let query_id = mint_id().expect("query id");
                            extra_events.push(event_message(
                                "sessions.listing_begin",
                                json!({ "query_id": query_id, "revision": 1 }),
                            ));
                            extra_events.push(event_message(
                                "sessions.listing_rows",
                                json!({
                                    "query_id": query_id,
                                    "revision": 1,
                                    "rows": [row_for_version(rows_ver)],
                                }),
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
    async fn rows_7_returns_waiting_and_engine_version_in_one_call() {
        let (_dir, state_dir, token) = private_state_dir();
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let owner = fake_owner(&state_dir, token, 7, Arc::clone(&calls), Arc::clone(&asked));
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");
        let mut listing = NewestRowsListing::new(&client);
        let res = listing.list(None).await.expect("list");
        let row = ListingRow::parse(&res.rows[0]).expect("parse row");
        assert_eq!(row.durable_id.as_deref(), Some(DURABLE_ID));
        assert_eq!(row.kind.as_deref(), Some("interactive"));
        assert_eq!(row.archived, Some(true));
        assert_eq!(
            row.waiting,
            Some(ListingWait {
                kind: ListingWaitKind::RateLimit,
                attempt: 2,
                max_attempts: 5,
                delay_ms: 30_000,
                until: "2026-11-21T01:30:00.000Z".into(),
            })
        );
        assert_eq!(row.engine_version.as_deref(), Some(ENGINE_VERSION));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        client.close().await;
        owner.await.expect("owner");
    }

    #[tokio::test]
    async fn rows_6_returns_waiting_without_engine_version_and_steps_down_once() {
        let (_dir, state_dir, token) = private_state_dir();
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let owner = fake_owner(&state_dir, token, 6, Arc::clone(&calls), Arc::clone(&asked));
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");
        let mut listing = NewestRowsListing::new(&client);
        let res = listing.list(None).await.expect("list");
        let row = ListingRow::parse(&res.rows[0]).expect("parse row");
        assert!(row.waiting.is_some());
        assert!(row.engine_version.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(*asked.lock().expect("lock"), vec![Some(7), Some(6)]);
        client.close().await;
        owner.await.expect("owner");
    }

    #[tokio::test]
    async fn rows_4_returns_model_and_archived_without_rows_6_or_7_fields() {
        let (_dir, state_dir, token) = private_state_dir();
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let owner = fake_owner(&state_dir, token, 4, Arc::clone(&calls), Arc::clone(&asked));
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");
        let mut listing = NewestRowsListing::new(&client);
        let res = listing.list(None).await.expect("list");
        let row = ListingRow::parse(&res.rows[0]).expect("parse row");
        assert_eq!(row.durable_id.as_deref(), Some(DURABLE_ID));
        assert_eq!(row.archived, Some(true));
        assert!(row.model.is_some());
        assert!(row.waiting.is_none());
        assert!(row.engine_version.is_none());
        client.close().await;
        owner.await.expect("owner");
    }

    #[tokio::test]
    async fn rows_3_returns_durable_id_without_model_or_archived() {
        let (_dir, state_dir, token) = private_state_dir();
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let owner = fake_owner(&state_dir, token, 3, Arc::clone(&calls), Arc::clone(&asked));
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");
        let mut listing = NewestRowsListing::new(&client);
        let res = listing.list(None).await.expect("list");
        let row = ListingRow::parse(&res.rows[0]).expect("parse row");
        assert_eq!(row.durable_id.as_deref(), Some(DURABLE_ID));
        assert!(row.model.is_none());
        assert!(row.archived.is_none());
        client.close().await;
        owner.await.expect("owner");
    }

    #[tokio::test]
    async fn rows_1_falls_back_in_seven_calls_and_reuses_baseline_on_second_call() {
        let (_dir, state_dir, token) = private_state_dir();
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let owner = fake_owner(&state_dir, token, 1, Arc::clone(&calls), Arc::clone(&asked));
        let client = connect_local_api(ConnectOptions::new(&state_dir, ClientInfo::cli("0.0.0")))
            .await
            .expect("connect");
        let mut listing = NewestRowsListing::new(&client);
        let res = listing.list(None).await.expect("first list");
        let row = ListingRow::parse(&res.rows[0]).expect("parse row");
        assert!(row.kind.is_none());
        assert!(row.durable_id.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 7);
        assert_eq!(listing.current_rows(), None);

        listing.list(None).await.expect("second list");
        assert_eq!(calls.load(Ordering::SeqCst), 8);
        client.close().await;
        owner.await.expect("owner");
    }
}
