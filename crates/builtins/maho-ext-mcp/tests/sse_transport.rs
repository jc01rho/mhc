//! Legacy HTTP+SSE transport (omo #11591): the native client against a loopback server speaking the
//! 2024-11-05 protocol (GET event stream, `endpoint` event, POSTed messages, replies on the stream).
use maho_ext_mcp::{
    errors::McpErrorKind,
    transport_sdk::{McpClient, McpTransportSpec},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
enum Stream {
    AnnouncesEndpoint,
    AnnouncesOtherOrigin,
    NeverAnnounces,
    Rejected,
}

/// One request the server saw: `GET /sse` or a POSTed message's method, with two of its headers.
#[derive(Clone, Debug)]
struct Seen {
    what: String,
    skill_header: Option<String>,
    protocol_version: Option<String>,
}

struct LegacyServer {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    opened: mpsc::UnboundedReceiver<()>,
    closed: mpsc::UnboundedReceiver<()>,
}

impl LegacyServer {
    fn seen(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|seen| seen.what.clone())
            .collect()
    }
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

async fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let end = loop {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
    let length = header(&head, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < end + length {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Some((head, bytes[end..end + length].to_vec()))
}

fn chunk(text: &str) -> Vec<u8> {
    format!("{:x}\r\n{text}\r\n", text.len()).into_bytes()
}

fn reply(message: &Value) -> Value {
    let result = match message["method"].as_str() {
        Some("initialize") => Some(
            json!({"protocolVersion":LEGACY_PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"legacy-sse","version":"1"}}),
        ),
        Some("tools/list") => {
            Some(json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}))
        }
        Some("tools/call") => {
            Some(json!({"content":[{"type":"text","text":message["params"]["arguments"]["text"]}]}))
        }
        _ => None,
    };
    match result {
        Some(result) => json!({"jsonrpc":"2.0","id":message["id"],"result":result}),
        None => {
            json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"Method not found"}})
        }
    }
}

/// GET /sse opens the event stream (and announces the POST endpoint unless told not to); each POST
/// to /messages is answered 202 and its reply travels the stream. Anything else gets 405, as the
/// servers in #9050 answer a Streamable HTTP POST.
async fn start_legacy_server(kind: Stream) -> LegacyServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (opened_tx, opened) = mpsc::unbounded_channel();
    let (closed_tx, closed) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = mpsc::unbounded_channel::<String>();
    let events_rx = Arc::new(tokio::sync::Mutex::new(Some(events_rx)));
    let record = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (record, opened_tx, closed_tx, events_tx, events_rx) = (
                record.clone(),
                opened_tx.clone(),
                closed_tx.clone(),
                events_tx.clone(),
                events_rx.clone(),
            );
            tokio::spawn(async move {
                let Some((head, body)) = read_request(&mut stream).await else {
                    return;
                };
                let mut request_line = head.lines().next().unwrap_or_default().split(' ');
                let method = request_line.next().unwrap_or_default().to_owned();
                let path = request_line.next().unwrap_or_default().to_owned();
                let seen = |what: String| Seen {
                    what,
                    skill_header: header(&head, "x-skill-header"),
                    protocol_version: header(&head, "mcp-protocol-version"),
                };
                if method == "GET" && path == "/sse" {
                    record.lock().unwrap().push(seen("GET /sse".into()));
                    if matches!(kind, Stream::Rejected) {
                        let _ = stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                        return;
                    }
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nTransfer-Encoding: chunked\r\n\r\n").await;
                    let endpoint = match kind {
                        Stream::AnnouncesEndpoint => Some("/messages?sessionId=legacy".to_owned()),
                        Stream::AnnouncesOtherOrigin => {
                            Some(format!("http://localhost:{port}/messages?sessionId=legacy"))
                        }
                        Stream::NeverAnnounces | Stream::Rejected => None,
                    };
                    if let Some(endpoint) = endpoint {
                        let _ = stream
                            .write_all(&chunk(&format!("event: endpoint\ndata: {endpoint}\n\n")))
                            .await;
                    }
                    let _ = opened_tx.send(());
                    let Some(mut events) = events_rx.lock().await.take() else {
                        return;
                    };
                    let (mut reader, mut writer) = stream.into_split();
                    let mut byte = [0u8; 64];
                    loop {
                        tokio::select! {
                            event = events.recv() => match event {
                                Some(event) => {
                                    if writer.write_all(&chunk(&event)).await.is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            },
                            read = reader.read(&mut byte) => match read {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            },
                        }
                    }
                    let _ = closed_tx.send(());
                    return;
                }
                if method != "POST" || !path.starts_with("/messages") {
                    record
                        .lock()
                        .unwrap()
                        .push(seen(format!("{method} {path}")));
                    let _ = stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    return;
                }
                let message: Value = serde_json::from_slice(&body).unwrap();
                record.lock().unwrap().push(seen(
                    message["method"].as_str().unwrap_or_default().to_owned(),
                ));
                if message.get("id").is_some() {
                    let _ =
                        events_tx.send(format!("event: message\ndata: {}\n\n", reply(&message)));
                }
                let _ = stream
                    .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 8\r\nConnection: close\r\n\r\nAccepted")
                    .await;
            });
        }
    });
    LegacyServer {
        url: format!("http://127.0.0.1:{port}/sse"),
        seen,
        opened,
        closed,
    }
}

async fn connect(server: &LegacyServer, root: &std::path::Path) -> Arc<McpClient> {
    let spec = McpTransportSpec::Sse {
        url: server.url.parse().unwrap(),
        headers: BTreeMap::from([("X-Skill-Header".to_owned(), "present".to_owned())]),
    };
    McpClient::materialize(
        "legacy-sse",
        &spec,
        Arc::new(Mutex::new(
            maho_ext_mcp::log::McpLogger::new("legacy-sse", root, None).unwrap(),
        )),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_legacy_server_is_reached_over_the_get_stream_and_the_announced_post_endpoint() {
    let server = start_legacy_server(Stream::AnnouncesEndpoint).await;
    let root = tempfile::tempdir().unwrap();
    let client = connect(&server, root.path()).await;

    client.initialize(WAIT).await.unwrap();
    let tools = client.request("tools/list", json!({}), WAIT).await.unwrap();
    let called = client
        .request(
            "tools/call",
            json!({"name":"echo","arguments":{"text":"hi"}}),
            WAIT,
        )
        .await
        .unwrap();

    assert_eq!(tools["tools"][0]["name"], "echo");
    assert_eq!(called["content"][0]["text"], "hi");
    assert_eq!(
        client.negotiated_version().as_deref(),
        Some(LEGACY_PROTOCOL_VERSION)
    );
    assert_eq!(
        server.seen(),
        [
            "GET /sse",
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call"
        ]
    );
    let seen = server.seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .all(|seen| seen.skill_header.as_deref() == Some("present")),
        "{seen:?}"
    );
    // The version header follows the negotiation: none on the GET or on initialize, then the
    // version the server chose.
    assert_eq!(
        seen.iter()
            .map(|seen| seen.protocol_version.as_deref())
            .collect::<Vec<_>>(),
        [
            None,
            None,
            Some(LEGACY_PROTOCOL_VERSION),
            Some(LEGACY_PROTOCOL_VERSION),
            Some(LEGACY_PROTOCOL_VERSION)
        ]
    );
    client.close().await.unwrap();
}

#[tokio::test]
async fn a_server_that_never_announces_its_endpoint_fails_the_connect_at_the_request_budget_and_close_ends_the_stream()
 {
    let mut server = start_legacy_server(Stream::NeverAnnounces).await;
    let root = tempfile::tempdir().unwrap();
    let client = connect(&server, root.path()).await;

    let connecting = {
        let client = client.clone();
        tokio::spawn(async move { client.initialize(Duration::from_millis(300)).await })
    };
    tokio::time::timeout(WAIT, server.opened.recv())
        .await
        .expect("the GET stream to open")
        .unwrap();
    let error = tokio::time::timeout(WAIT, connecting)
        .await
        .expect("the connect to settle")
        .unwrap()
        .expect_err("a connect without an endpoint");

    assert_eq!(error.kind, McpErrorKind::Timeout, "{error}");
    client.close().await.unwrap();
    tokio::time::timeout(WAIT, server.closed.recv())
        .await
        .expect("the client to close the GET stream")
        .unwrap();
    assert_eq!(server.seen(), ["GET /sse"]);
}

#[tokio::test]
async fn an_endpoint_on_another_origin_is_refused_before_any_message_is_posted() {
    let mut server = start_legacy_server(Stream::AnnouncesOtherOrigin).await;
    let root = tempfile::tempdir().unwrap();
    let client = connect(&server, root.path()).await;

    let error = tokio::time::timeout(WAIT, client.initialize(Duration::from_secs(60)))
        .await
        .expect("the refusal not to wait for the request budget")
        .expect_err("a cross-origin endpoint");

    assert!(
        error
            .message
            .starts_with("Endpoint origin does not match connection origin: "),
        "{error}"
    );
    tokio::time::timeout(WAIT, server.closed.recv())
        .await
        .expect("the refused stream to close")
        .unwrap();
    assert_eq!(server.seen(), ["GET /sse"]);
    client.close().await.unwrap();
}

#[tokio::test]
async fn a_rejected_event_stream_fails_the_connect_at_once() {
    let server = start_legacy_server(Stream::Rejected).await;
    let root = tempfile::tempdir().unwrap();
    let client = connect(&server, root.path()).await;

    let error = tokio::time::timeout(WAIT, client.initialize(Duration::from_secs(60)))
        .await
        .expect("the rejection not to wait for the request budget")
        .expect_err("a rejected stream");

    assert_eq!(error.message, "HTTP 405");
    assert_eq!(server.seen(), ["GET /sse"]);
    client.close().await.unwrap();
}
