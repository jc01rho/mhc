//! End-to-end: the real mhc sync client, over HTTP, against `maho-sync-server`. Two devices of one
//! account pair through the routes the client already uses, a batch they upload reaches the other
//! device byte for byte, a repost keeps one row, a moved base is refused, the event stream announces
//! the new head, and another account can read none of it.

use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use maho_sync_server::{ServerState, Tokens, router};
use memory_core::sync::engine::adapters::http_transport::{
    ByteStream, HttpClient, HttpRequest, HttpResponse, HttpTransportOptions, StreamResponse,
    create_http_transport,
};
use memory_core::sync::engine::error::{EngineError, EngineResult};
use memory_core::sync::engine::ports::{
    AppendRequest, AppendResult, EntryKind, StreamHandlers, Transport,
};

struct ReqwestClient(reqwest::Client);

struct Body(reqwest::Response);

impl ByteStream for Body {
    fn next_chunk(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<Option<Vec<u8>>>> + '_>>
    {
        Box::pin(async move {
            self.0
                .chunk()
                .await
                .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
                .map_err(|error| EngineError::Other(error.to_string()))
        })
    }
}

impl HttpClient for ReqwestClient {
    fn send(
        &self,
        request: HttpRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<HttpResponse>> + '_>> {
        Box::pin(async move {
            let method = reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|error| EngineError::Other(error.to_string()))?;
            let mut builder = self.0.request(method, request.url);
            for (key, value) in request.headers {
                builder = builder.header(key, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let response = builder
                .send()
                .await
                .map_err(|error| EngineError::Other(error.to_string()))?;
            let status = response.status().as_u16();
            let body = response
                .bytes()
                .await
                .map_err(|error| EngineError::Other(error.to_string()))?
                .to_vec();
            Ok(HttpResponse { status, body })
        })
    }

    fn open_stream(
        &self,
        request: HttpRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<StreamResponse>> + '_>>
    {
        Box::pin(async move {
            let mut builder = self.0.get(request.url);
            for (key, value) in request.headers {
                builder = builder.header(key, value);
            }
            let response = builder
                .send()
                .await
                .map_err(|error| EngineError::Other(error.to_string()))?;
            Ok(StreamResponse {
                status: response.status().as_u16(),
                body: Box::new(Body(response)),
            })
        })
    }
}

const TOKENS: &str = "\
# token account device-id sync-principal
tok-a account-1 device-a device:a
tok-b account-1 device-b device:b
tok-x account-2 device-x device:x
";

fn transport(
    base: &str,
    token: &'static str,
) -> memory_core::sync::engine::adapters::http_transport::HttpTransport {
    create_http_transport(HttpTransportOptions {
        base_url: base.into(),
        token: Rc::new(move || Box::pin(async move { Ok(token.to_string()) })),
        client: Rc::new(ReqwestClient(reqwest::Client::new())),
    })
}

async fn serve() -> Result<(String, ServerState), Box<dyn std::error::Error>> {
    let tokens = Tokens::parse(TOKENS).map_err(std::io::Error::other)?;
    let state = ServerState::in_memory(tokens)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address: SocketAddr = listener.local_addr()?;
    let app = router(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((format!("http://{address}"), state))
}

fn digest_b64(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    URL_SAFE_NO_PAD.encode(Sha256::digest(bytes))
}

#[tokio::test]
async fn two_devices_pair_push_and_pull_through_the_real_client()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, _state) = serve().await?;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let a = transport(&base, "tok-a");
            let b = transport(&base, "tok-b");

            let state = a.pull(0, 500).await?;
            assert_eq!(state.entries.len(), 0);
            assert_eq!(state.head, 0);

            let object = b"sealed-batch-ciphertext".to_vec();
            let digest = hex(&object);
            let upload = a.reserve(object.len(), &digest).await?;
            a.upload(&upload, &object).await?;
            let appended = a
                .append(AppendRequest {
                    upload_id: upload.clone(),
                    digest: digest.clone(),
                    base_seq: 0,
                    batch_id: "batch-one".into(),
                    epoch: 1,
                    link: "link-1".into(),
                    record_sig: "sig-1".into(),
                })
                .await?;
            assert_eq!(appended, AppendResult::Ok { seq: 1 });

            let page = b.pull(0, 500).await?;
            assert_eq!(page.head, 1);
            assert_eq!(page.entries.len(), 1);
            let row = &page.entries[0];
            assert_eq!(row.kind, EntryKind::Batch);
            assert_eq!(row.seq, 1);
            assert_eq!(row.digest, hex(&object));
            assert_eq!(row.batch_id.as_deref(), Some("batch-one"));
            let downloaded = b.download(&row.object_id).await?;
            assert_eq!(downloaded, object, "the server hands back the sealed bytes");

            let repost = a
                .append(AppendRequest {
                    upload_id: upload.clone(),
                    digest: digest.clone(),
                    base_seq: 0,
                    batch_id: "batch-one".into(),
                    epoch: 1,
                    link: "link-1".into(),
                    record_sig: "sig-1".into(),
                })
                .await?;
            assert_eq!(
                repost,
                AppendResult::Ok { seq: 1 },
                "a repost answers its original seq, never a second row"
            );
            assert_eq!(b.pull(0, 500).await?.entries.len(), 1);

            let moved = a
                .append(AppendRequest {
                    upload_id: upload.clone(),
                    digest: digest.clone(),
                    base_seq: 0,
                    batch_id: "batch-two".into(),
                    epoch: 1,
                    link: "link-2".into(),
                    record_sig: "sig-2".into(),
                })
                .await?;
            assert_eq!(
                moved,
                AppendResult::StaleBase { head: 1 },
                "a writer that read an old head is refused"
            );

            let second = b"second-sealed-batch".to_vec();
            let second_digest = hex(&second);
            let upload2 = a.reserve(second.len(), &second_digest).await?;
            a.upload(&upload2, &second).await?;
            assert_eq!(
                a.append(AppendRequest {
                    upload_id: upload2,
                    digest: second_digest,
                    base_seq: 1,
                    batch_id: "batch-two".into(),
                    epoch: 1,
                    link: "link-2".into(),
                    record_sig: "sig-2".into(),
                })
                .await?,
                AppendResult::Ok { seq: 2 }
            );
            assert_eq!(b.pull(0, 500).await?.entries.len(), 2);

            let other = transport(&base, "tok-x");
            assert_eq!(
                other.pull(0, 500).await?.entries.len(),
                0,
                "another account reads none of it"
            );
            let refused = other.download(&row.object_id).await;
            assert!(refused.is_err(), "another account cannot fetch the object");

            let unknown = transport(&base, "tok-not-issued");
            assert!(
                unknown.pull(0, 500).await.is_err(),
                "an unknown token is refused"
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_pending_rotation_freezes_batches_until_a_keyring_record_lands()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, state) = serve().await?;
    state.store().revoke_device("account-1")?;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let a = transport(&base, "tok-a");
            let object = b"sealed".to_vec();
            let digest = hex(&object);
            let upload = a.reserve(object.len(), &digest).await?;
            a.upload(&upload, &object).await?;
            let refused = a
                .append(AppendRequest {
                    upload_id: upload,
                    digest,
                    base_seq: 0,
                    batch_id: "batch-while-frozen".into(),
                    epoch: 1,
                    link: "l".into(),
                    record_sig: "s".into(),
                })
                .await
                .expect_err("a frozen account refuses batch appends");
            match refused {
                EngineError::Http(error) => assert_eq!(error.code, "stale_base"),
                other => panic!("expected a stale_base refusal, got {other}"),
            }
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn the_event_stream_announces_a_new_head() -> Result<(), Box<dyn std::error::Error>> {
    let (base, _state) = serve().await?;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let b = transport(&base, "tok-b");
            let (sender, mut receiver) = tokio::sync::mpsc::channel::<u64>(4);
            let sender = Rc::new(sender);
            let handlers = StreamHandlers {
                on_advanced: Rc::new(move |head| {
                    let _ = sender.try_send(head);
                }),
                on_connected: Rc::new(|_| {}),
            };
            let unsubscribe = b.subscribe(handlers);

            let a = transport(&base, "tok-a");
            let object = b"sealed".to_vec();
            let digest = hex(&object);
            let upload = a.reserve(object.len(), &digest).await?;
            a.upload(&upload, &object).await?;
            a.append(AppendRequest {
                upload_id: upload,
                digest,
                base_seq: 0,
                batch_id: "batch-stream".into(),
                epoch: 1,
                link: "l".into(),
                record_sig: "s".into(),
            })
            .await?;

            let head = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
                .await
                .map_err(|_| "the event stream never announced the new head")?
                .ok_or("the event stream closed")?;
            assert_eq!(head, 1);
            unsubscribe();
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    assert_eq!(digest_b64(bytes).len(), 43);
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
