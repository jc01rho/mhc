use std::cell::RefCell;

use super::*;

#[test]
fn digests_round_trip_between_hex_and_base64url() {
    let hex_digest = "00ff10abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123";
    let wire = b64url_of_hex(hex_digest);
    assert_eq!(wire.len(), 43);
    assert_eq!(hex_of_b64url(&wire), hex_digest);
}

#[test]
fn only_advance_and_keyring_frames_with_a_numeric_head_fire() {
    assert_eq!(
        advanced_head("event: x\ndata: {\"type\":\"sync.advanced\",\"head\":{\"seq\":7}}"),
        Some(7)
    );
    assert_eq!(
        advanced_head("data:{\"type\":\"sync.keyring.changed\",\"head\":{\"seq\":9}}"),
        Some(9)
    );
    assert_eq!(
        advanced_head("data: {\"type\":\"other\",\"head\":{\"seq\":1}}"),
        None
    );
    assert_eq!(advanced_head("data: {\"type\":\"sync.advanced\"}"), None);
    assert_eq!(advanced_head(": keepalive"), None);
    assert_eq!(advanced_head("data: not json"), None);
}

#[test]
fn error_message_names_code_and_status() {
    assert_eq!(
        SyncHttpError::new("stale_base", 409, false).to_string(),
        "sync request refused: stale_base (409)"
    );
}

struct Scripted {
    responses: RefCell<Vec<(u16, String)>>,
    seen: RefCell<Vec<(String, String, Option<String>)>>,
}

impl HttpClient for Scripted {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        Box::pin(async move {
            let auth = request
                .headers
                .iter()
                .find(|(k, _)| k == "authorization")
                .map(|(_, v)| v.clone());
            self.seen
                .borrow_mut()
                .push((request.method.to_string(), request.url, auth));
            let (status, body) = self.responses.borrow_mut().remove(0);
            Ok(HttpResponse {
                status,
                body: body.into_bytes(),
            })
        })
    }

    fn open_stream(&self, _request: HttpRequest) -> PortFuture<'_, StreamResponse> {
        Box::pin(async { Err(EngineError::Other("no stream in this test".into())) })
    }
}

fn transport(responses: Vec<(u16, &str)>) -> (HttpTransport, Rc<Scripted>) {
    let client = Rc::new(Scripted {
        responses: RefCell::new(
            responses
                .into_iter()
                .map(|(s, b)| (s, b.to_string()))
                .collect(),
        ),
        seen: RefCell::new(Vec::new()),
    });
    let transport = create_http_transport(HttpTransportOptions {
        base_url: "https://svc".into(),
        token: Rc::new(|| Box::pin(async { Ok("tok".to_string()) })),
        client: client.clone(),
    });
    (transport, client)
}

fn block_on<T>(
    future: impl std::future::Future<Output = T>,
) -> Result<T, Box<dyn std::error::Error>> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?
        .block_on(future))
}

#[test]
fn pull_reads_state_then_batches_and_slices_to_the_limit() -> Result<(), Box<dyn std::error::Error>>
{
    let digest = b64url_of_hex(&"ab".repeat(32));
    let row = |seq: u64| {
        format!(
            r#"{{"seq":{seq},"kind":"batch","author":{{"kind":"device","id":"d"}},"digest":"{digest}","object_id":"o{seq}","batch_id":"b{seq}"}}"#
        )
    };
    let batches = format!(r#"{{"entries":[{},{},{}]}}"#, row(1), row(2), row(3));
    let (transport, client) = transport(vec![
        (
            200,
            r#"{"head":{"seq":5},"oldest_retained_seq":1,"snapshot":null}"#,
        ),
        (200, &batches),
    ]);
    let page = block_on(transport.pull(0, 2))??;
    assert!(page.snapshot.is_none());
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].author, "device:d");
    assert_eq!(page.entries[0].batch_id.as_deref(), Some("b1"));
    assert_eq!(page.entries[0].digest, "ab".repeat(32));
    assert!(page.more);
    assert_eq!(page.head, 5);
    let seen = client.seen.borrow();
    assert_eq!(seen[1].1, "https://svc/v1/sync/batches?after=0");
    assert_eq!(seen[1].2.as_deref(), Some("Bearer tok"));
    Ok(())
}

#[test]
fn pull_below_retained_history_returns_the_snapshot_and_reads_from_it()
-> Result<(), Box<dyn std::error::Error>> {
    let (transport, client) = transport(vec![
        (
            200,
            r#"{"head":{"seq":9},"oldest_retained_seq":8,"snapshot":{"covers_seq":7,"chunk_object_ids":["c1","c2"]}}"#,
        ),
        (200, r#"{"entries":[]}"#),
    ]);
    let page = block_on(transport.pull(2, 100))??;
    assert_eq!(
        page.snapshot.map(|s| (s.covers_seq, s.object_ids)),
        Some((7, vec!["c1".to_string(), "c2".to_string()]))
    );
    assert_eq!(
        client.seen.borrow()[1].1,
        "https://svc/v1/sync/batches?after=7"
    );
    assert!(page.more);
    Ok(())
}

#[test]
fn stale_base_answers_the_head_from_state_and_5xx_is_unavailable()
-> Result<(), Box<dyn std::error::Error>> {
    let (transport, _) = transport(vec![
        (409, r#"{"error":{"code":"stale_base"}}"#),
        (
            200,
            r#"{"head":{"seq":12},"oldest_retained_seq":1,"snapshot":null}"#,
        ),
        (503, r#"{"error":{"code":"x"}}"#),
    ]);
    let request = || AppendRequest {
        upload_id: "u".into(),
        digest: "ab".repeat(32),
        base_seq: 1,
        batch_id: "b".into(),
        epoch: 1,
        link: "l".into(),
        record_sig: "s".into(),
    };
    assert_eq!(
        block_on(transport.append(request()))??,
        AppendResult::StaleBase { head: 12 }
    );
    let failure = block_on(transport.append(request()))?;
    match failure {
        Err(EngineError::Http(e)) => assert_eq!(
            (e.code.as_str(), e.status, e.transient),
            ("unavailable", 503, true)
        ),
        other => return Err(format!("expected an http error, got {:?}", other.map(|_| ())).into()),
    }
    Ok(())
}

/// Answers every request with 200 and the body that request needs, keeping each request's headers.
struct Recording {
    seen: RefCell<Vec<(String, Vec<(String, String)>)>>,
}

impl Recording {
    fn record(&self, request: &HttpRequest) -> String {
        let path = request
            .url
            .strip_prefix("https://svc")
            .unwrap_or(&request.url)
            .to_string();
        self.seen
            .borrow_mut()
            .push((path.clone(), request.headers.clone()));
        path
    }
}

impl HttpClient for Recording {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        let path = self.record(&request);
        let body = if path.starts_with("/v1/sync/state") {
            r#"{"head":{"seq":0},"oldest_retained_seq":1,"snapshot":null}"#
        } else if path.starts_with("/v1/sync/batches?") {
            r#"{"entries":[]}"#
        } else if path == "/v1/sync/uploads" {
            r#"{"upload_id":"u1"}"#
        } else if path == "/v1/sync/batches" {
            r#"{"seq":1}"#
        } else {
            "{}"
        };
        Box::pin(async move {
            Ok(HttpResponse {
                status: 200,
                body: body.as_bytes().to_vec(),
            })
        })
    }

    fn open_stream(&self, request: HttpRequest) -> PortFuture<'_, StreamResponse> {
        self.record(&request);
        Box::pin(async { Err(EngineError::Other("stream closed by the test".into())) })
    }
}

#[tokio::test]
async fn every_engine_request_declares_the_sync_contract() -> Result<(), Box<dyn std::error::Error>>
{
    use crate::sync::contract_version::misses_sync_contract;
    let client = Rc::new(Recording {
        seen: RefCell::new(Vec::new()),
    });
    let token: TokenSource = Rc::new(|| Box::pin(async { Ok("tok".to_string()) }));
    let transport = create_http_transport(HttpTransportOptions {
        base_url: "https://svc".into(),
        token: token.clone(),
        client: client.clone(),
    });
    transport.pull(0, 10).await?;
    transport.download("o1").await?;
    let upload = transport.reserve(3, &"ab".repeat(32)).await?;
    transport.upload(&upload, b"abc").await?;
    transport
        .append(AppendRequest {
            upload_id: upload,
            digest: "ab".repeat(32),
            base_seq: 0,
            batch_id: "b".into(),
            epoch: 1,
            link: "l".into(),
            record_sig: "s".into(),
        })
        .await?;
    let handlers = StreamHandlers {
        on_advanced: Rc::new(|_| {}),
        on_connected: Rc::new(|_| {}),
    };
    let mut up = false;
    assert!(
        stream_once("https://svc", &token, client.as_ref(), &handlers, &mut up)
            .await
            .is_err()
    );
    let seen = client.seen.borrow();
    let paths: Vec<&str> = seen.iter().map(|(path, _)| path.as_str()).collect();
    for route in [
        "/v1/sync/state",
        "/v1/sync/batches?after=0",
        "/v1/sync/objects/o1",
        "/v1/sync/uploads",
        "/v1/sync/uploads/u1",
        "/v1/sync/batches",
        "/v1/me/events",
    ] {
        assert!(paths.contains(&route), "{route} was not requested");
    }
    for (path, headers) in seen.iter() {
        // The event stream is not floored today, yet it carries the same headers (#13691).
        let floored = if path.starts_with("/v1/sync/") {
            path.clone()
        } else {
            format!("/v1/sync{path}")
        };
        assert!(!misses_sync_contract(&floored, headers), "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn token_and_network_failures_are_typed_without_leaking_detail() {
    struct Offline;
    impl HttpClient for Offline {
        fn send(&self, _: HttpRequest) -> PortFuture<'_, HttpResponse> {
            Box::pin(async { Err(EngineError::Other("raw network secret".into())) })
        }
        fn open_stream(&self, _: HttpRequest) -> PortFuture<'_, StreamResponse> {
            Box::pin(async { Err(EngineError::Other("raw network secret".into())) })
        }
    }
    let auth = create_http_transport(HttpTransportOptions {
        base_url: "https://test.invalid".into(),
        token: Rc::new(|| Box::pin(async { Err(EngineError::Other("raw token secret".into())) })),
        client: Rc::new(Offline),
    });
    assert!(matches!(auth.pull(0, 10).await, Err(EngineError::Auth)));
    let offline = create_http_transport(HttpTransportOptions {
        base_url: "https://test.invalid".into(),
        token: Rc::new(|| Box::pin(async { Ok("token".into()) })),
        client: Rc::new(Offline),
    });
    assert!(matches!(
        offline.pull(0, 10).await,
        Err(EngineError::Network)
    ));
}
