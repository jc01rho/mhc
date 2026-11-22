//! End-to-end: the real mhc sync client, over HTTP, against `maho-sync-server`. Two devices of one
//! account pair through the routes the client already uses, a batch they upload reaches the other
//! device byte for byte, a repost keeps one row, a moved base is refused, a keyring append advances
//! and unfreezes the account, the device list reports revocations, the event stream announces the
//! new head, and another account can read none of it.

use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use maho_sync_server::{ServerState, Tokens, router};
use memory_core::sync::account::http::{AccountHttp, create_account_http};
use memory_core::sync::account::pairing_http::{DeviceSyncOff, PairingHttp, create_pairing_http};
use memory_core::sync::account::revocation_view::{RevocationView, read_revocation_view};
use memory_core::sync::crypto::entry::WrapEntry;
use memory_core::sync::crypto::envelope::parse_entry;
use memory_core::sync::crypto::parse::Principal;
use memory_core::sync::engine::adapters::http_transport::{
    ByteStream, HttpClient, HttpRequest, HttpResponse, HttpTransportOptions, StreamResponse,
    create_http_transport,
};
use memory_core::sync::engine::error::{EngineError, EngineResult};
use memory_core::sync::engine::ports::{
    AppendRequest, AppendResult, EntryKind, StreamHandlers, Transport,
};
use memory_core::sync::recovery::ports::KeyringAppendRequest;
use serde_json::{Value, json};

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
# token account device-id sync-principal [admin] [seal=...] [sign=...]
tok-a account-1 device-a device:a seal=seal-a sign=sign-a
tok-b account-1 device-b device:b seal=seal-b sign=sign-b
tok-x account-2 device-x device:x seal=seal-x sign=sign-x
tok-admin acct-admin device-admin device:admin admin
";

fn options(base: &str, token: &'static str) -> HttpTransportOptions {
    HttpTransportOptions {
        base_url: base.into(),
        token: Rc::new(move || Box::pin(async move { Ok(token.to_string()) })),
        client: Rc::new(ReqwestClient(reqwest::Client::new())),
    }
}

fn transport(
    base: &str,
    token: &'static str,
) -> memory_core::sync::engine::adapters::http_transport::HttpTransport {
    create_http_transport(options(base, token))
}

fn account_http(base: &str, token: &'static str) -> AccountHttp {
    create_account_http(options(base, token))
}

fn pairing_http(base: &str, token: &'static str) -> PairingHttp {
    create_pairing_http(account_http(base, token))
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

fn bid(tag: &str) -> String {
    format!("{tag:_<22}")
}

fn digest_b64(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    URL_SAFE_NO_PAD.encode(Sha256::digest(bytes))
}

fn append_request(upload_id: &str, digest: &str, base_seq: u64, batch_id: &str) -> AppendRequest {
    AppendRequest {
        upload_id: upload_id.to_string(),
        digest: digest.to_string(),
        base_seq,
        batch_id: batch_id.to_string(),
        epoch: 1,
        link: format!("link-{batch_id}"),
        record_sig: format!("sig-{batch_id}"),
    }
}

async fn push_batch(
    transport: &impl Transport,
    bytes: &[u8],
    base_seq: u64,
    batch_id: &str,
) -> Result<AppendResult, Box<dyn std::error::Error>> {
    let digest = hex(bytes);
    let upload = transport.reserve(bytes.len(), &digest).await?;
    transport.upload(&upload, bytes).await?;
    Ok(transport
        .append(append_request(&upload, &digest, base_seq, batch_id))
        .await?)
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
            let batch_one = bid("one");
            let appended = a
                .append(append_request(&upload, &digest, 0, &batch_one))
                .await?;
            assert_eq!(appended, AppendResult::Ok { seq: 1 });

            let page = b.pull(0, 500).await?;
            assert_eq!(page.head, 1);
            assert_eq!(page.entries.len(), 1);
            let row = &page.entries[0];
            assert_eq!(row.kind, EntryKind::Batch);
            assert_eq!(row.seq, 1);
            assert_eq!(row.digest, hex(&object));
            assert_eq!(row.batch_id.as_deref(), Some(batch_one.as_str()));
            let downloaded = b.download(&row.object_id).await?;
            assert_eq!(downloaded, object, "the server hands back the sealed bytes");

            let repost = a
                .append(append_request(&upload, &digest, 0, &batch_one))
                .await?;
            assert_eq!(
                repost,
                AppendResult::Ok { seq: 1 },
                "a repost answers its original seq, never a second row"
            );
            assert_eq!(b.pull(0, 500).await?.entries.len(), 1);

            let batch_two = bid("two");
            let moved = a
                .append(append_request(&upload, &digest, 0, &batch_two))
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
                a.append(append_request(&upload2, &second_digest, 1, &batch_two))
                    .await?,
                AppendResult::Ok { seq: 2 }
            );
            assert_eq!(
                a.append(append_request(&upload2, &second_digest, 2, &batch_two))
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
            match other.download(&row.object_id).await {
                Err(EngineError::Http(error)) => {
                    assert_eq!((error.code.as_str(), error.status), ("not_found", 404));
                }
                other => return Err(format!("expected not_found, got {other:?}").into()),
            }

            let unknown = transport(&base, "tok-not-issued");
            match unknown.pull(0, 10).await {
                Err(EngineError::Http(error)) => {
                    assert_eq!(
                        (error.code.as_str(), error.status),
                        ("unauthenticated", 401)
                    );
                }
                other => return Err(format!("expected unauthenticated, got {other:?}").into()),
            }
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn an_object_uploaded_with_the_wrong_bytes_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, _state) = serve().await?;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let a = transport(&base, "tok-a");
            let payload = b"the reserved bytes".to_vec();
            let digest = hex(&payload);
            let upload = a.reserve(payload.len(), &digest).await?;
            match a.upload(&upload, b"other bytes!!").await {
                Err(EngineError::Http(error)) => assert_eq!(error.code, "digest_mismatch"),
                other => return Err(format!("expected digest_mismatch, got {other:?}").into()),
            }
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_keyring_append_lands_and_the_account_routes_read_it()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, _state) = serve().await?;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let account = account_http(&base, "tok-a");
            let a = transport(&base, "tok-a");

            // Unsorted keys test JCS canonicalization on the server against the client's canonical hash.
            let record = json!({
                "wraps_digest": null,
                "kind": "genesis",
                "fork_resolution": null,
                "epoch": 1,
                "account": "account-1",
                "members": [],
            });
            let wrap = WrapEntry {
                epoch: 1,
                id: "device:device-a".into(),
                wrap: "sealed-epoch-1".into(),
            };
            let seq = account
                .append_keys(KeyringAppendRequest {
                    record: record.clone(),
                    base_seq: 0,
                    link: "link-genesis".into(),
                    keyring_link: "keyring-genesis".into(),
                    record_sig: "sig-genesis".into(),
                    wraps: vec![wrap],
                })
                .await?
                .map_err(|refusal| format!("append_keys refused: {refusal:?}"))?;
            assert_eq!(seq, 1);

            let state = account.state().await?.ok_or("missing state")?;
            assert_eq!(state.head.seq, 1);
            assert_eq!(state.head.link, "link-genesis");
            assert_eq!(state.head.keyring_link, "keyring-genesis");
            assert_eq!(state.current_epoch, 1);
            assert!(!state.rotation_pending);

            let keys = account.list_keys().await?.ok_or("missing keys")?;
            assert_eq!(keys.len(), 1);
            assert_eq!(keys[0]["seq"], 1);
            assert_eq!(keys[0]["kind"], "keyring");
            assert_eq!(keys[0]["body"], record);

            let wraps = account
                .list_recovery_wraps()
                .await?
                .ok_or("missing wraps")?;
            assert_eq!(wraps.len(), 1);
            assert_eq!(wraps[0]["record"]["seq"], 1);
            assert_eq!(wraps[0]["entry"]["id"], "device:device-a");

            assert_eq!(
                push_batch(&a, b"after-keyring", 1, &bid("after")).await?,
                AppendResult::Ok { seq: 2 }
            );

            let entries = account
                .list_entries_after(0)
                .await?
                .ok_or("missing entries")?;
            assert_eq!(entries.len(), 2);
            for row in &entries {
                parse_entry(row).map_err(|reason| format!("envelope refused row: {reason}"))?;
            }
            let expected_digest = memory_core::sync::crypto::signatures::sha256_b64(
                &memory_core::sync::crypto::signatures::canonical(&record)?,
            );
            assert_eq!(
                entries[0]["digest"].as_str(),
                Some(expected_digest.as_str())
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_pending_rotation_freezes_batches_until_a_keyring_record_lands()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, _state) = serve().await?;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let a = transport(&base, "tok-a");
            let b = transport(&base, "tok-b");
            let account = account_http(&base, "tok-a");

            let before_id = bid("before");
            assert_eq!(
                push_batch(&a, b"before revoke", 0, &before_id).await?,
                AppendResult::Ok { seq: 1 }
            );

            let admin = reqwest::Client::new();
            let forbidden: Value = admin
                .post(format!("{base}/v1/admin/devices/device-b/revoke"))
                .bearer_auth("tok-x")
                .send()
                .await?
                .json()
                .await?;
            assert_eq!(forbidden["error"]["code"], "forbidden");

            let revoked: Value = admin
                .post(format!("{base}/v1/admin/devices/device-b/revoke"))
                .bearer_auth("tok-admin")
                .send()
                .await?
                .json()
                .await?;
            assert_eq!(revoked["revoked"], true);
            assert_eq!(revoked["account"], "account-1");

            match b.pull(0, 10).await {
                Err(EngineError::Http(error)) => assert_eq!(error.code, "device_revoked"),
                other => return Err(format!("expected device_revoked, got {other:?}").into()),
            }

            // A repost of a batch that landed before the freeze still answers its original seq.
            let stale_state = account.state().await?.ok_or("missing state")?;
            assert!(stale_state.rotation_pending);
            let page = a.pull(0, 10).await?;
            assert_eq!(
                a.append(AppendRequest {
                    upload_id: page.entries[0].object_id.clone(),
                    digest: page.entries[0].digest.clone(),
                    base_seq: 0,
                    batch_id: before_id,
                    epoch: 1,
                    link: "link-before".into(),
                    record_sig: "sig-before".into(),
                })
                .await?,
                AppendResult::Ok { seq: 1 }
            );

            assert_eq!(
                push_batch(&a, b"while-frozen", 1, &bid("frozen")).await?,
                AppendResult::StaleBase { head: 1 }
            );

            let rotation = json!({
                "kind": "rotation",
                "account": "account-1",
                "epoch": 2,
                "members": [],
                "wraps_digest": null,
                "fork_resolution": null,
            });
            let rotated = account
                .append_keys(KeyringAppendRequest {
                    record: rotation,
                    base_seq: 1,
                    link: "link-rotation".into(),
                    keyring_link: "keyring-rotation".into(),
                    record_sig: "sig-rotation".into(),
                    wraps: vec![],
                })
                .await?
                .map_err(|refusal| format!("rotation refused: {refusal:?}"))?;
            assert_eq!(rotated, 2);

            let cleared = account.state().await?.ok_or("missing state")?;
            assert!(!cleared.rotation_pending);
            assert_eq!(cleared.current_epoch, 2);

            assert_eq!(
                push_batch(&a, b"after-rotation", 2, &bid("resumed")).await?,
                AppendResult::Ok { seq: 3 }
            );

            let RevocationView::Usable {
                self_principal,
                self_revoked,
                revoked,
                ..
            } = read_revocation_view(&account).await?
            else {
                return Err("expected usable device list".into());
            };
            assert_eq!(self_principal, "device:a");
            assert!(!self_revoked);
            assert!(revoked.contains("device:b"));

            let RevocationView::Usable {
                self_principal: b_self,
                self_revoked: b_revoked,
                ..
            } = read_revocation_view(&account_http(&base, "tok-b")).await?
            else {
                return Err("revoked device must still read /v1/devices".into());
            };
            assert_eq!(b_self, "device:b");
            assert!(b_revoked);
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn the_device_list_marks_this_device_current_and_reports_a_revocation()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, _state) = serve().await?;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let account = account_http(&base, "tok-a");
            let RevocationView::Usable {
                self_principal,
                self_revoked,
                revoked,
                listed,
            } = read_revocation_view(&account).await?
            else {
                return Err("expected usable device list".into());
            };
            assert_eq!(self_principal, "device:a");
            assert!(!self_revoked);
            assert!(revoked.is_empty());
            assert!(listed.contains("device:a"));
            assert!(listed.contains("device:b"));
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn two_devices_pair_and_toggle_sync_through_pairing_http()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, _state) = serve().await?;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let a = pairing_http(&base, "tok-a");
            let b = pairing_http(&base, "tok-b");

            let opened = b.open_pending("account-1", "commit-1").await?;
            assert_eq!(opened.account, "account-1");
            assert_eq!(a.list_pending().await?.len(), 1);
            assert!(a.read_pending(&opened.id).await?.is_some());

            let approver = Principal {
                kind: "device".into(),
                id: "device-a".into(),
                sealing_public_key: "seal-a".into(),
                signing_public_key: "sign-a".into(),
            };
            let answered = a
                .answer_pending(&opened.id, &approver, "approver-nonce")
                .await?;
            assert_eq!(
                answered.answer.as_ref().map(|a| a.approver_nonce.as_str()),
                Some("approver-nonce")
            );

            let joiner = Principal {
                kind: "device".into(),
                id: "device-b".into(),
                sealing_public_key: "seal-b".into(),
                signing_public_key: "sign-b".into(),
            };
            let revealed = b
                .reveal_pending(&opened.id, &joiner, "joining-nonce")
                .await?;
            assert_eq!(
                revealed.reveal.as_ref().map(|r| r.joining_nonce.as_str()),
                Some("joining-nonce")
            );

            a.set_account_sync(true).await?;
            assert_eq!(a.disable_device_sync("device-b").await?, DeviceSyncOff::Off);
            a.set_device_sync("device-b", true).await?;
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
            let (connected_tx, connected_rx) = tokio::sync::oneshot::channel::<()>();
            let connected_tx = RefCell::new(Some(connected_tx));
            let (sender, mut receiver) = tokio::sync::mpsc::channel::<u64>(4);
            let sender = Rc::new(sender);
            let handlers = StreamHandlers {
                on_advanced: Rc::new(move |head| {
                    let _ = sender.try_send(head);
                }),
                on_connected: Rc::new(move |up| {
                    if up && let Some(tx) = connected_tx.borrow_mut().take() {
                        let _ = tx.send(());
                    }
                }),
            };
            let unsubscribe = b.subscribe(handlers);
            tokio::time::timeout(Duration::from_secs(10), connected_rx)
                .await
                .map_err(|_| "the event stream never connected")?
                .map_err(|_| "the connected channel dropped")?;

            let a = transport(&base, "tok-a");
            assert_eq!(
                push_batch(&a, b"sealed", 0, &bid("stream")).await?,
                AppendResult::Ok { seq: 1 }
            );

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
