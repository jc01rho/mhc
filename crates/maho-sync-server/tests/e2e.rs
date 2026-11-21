//! End-to-end: the shipped sync client against a running `mhc-sync-server`.
//!
//! Every request below goes through `memory_core::sync`, the same code `mhc` runs, over a real
//! socket. The server is bound to 127.0.0.1:0 and driven with two accounts, so a passing test
//! proves the routes, the sequence, the idempotency, the keyring path, the event stream and the
//! account isolation against the real client rather than a hand-written HTTP script.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use maho_sync_server::{Config, Server};
use memory_core::support::sha256::{hex, sha256};
use memory_core::sync::account::http::{AccountHttp, create_account_http};
use memory_core::sync::engine::adapters::http_transport::{
    ByteStream, HttpClient, HttpRequest, HttpResponse, HttpTransportOptions, StreamResponse,
    TokenSource, create_http_transport,
};
use memory_core::sync::engine::error::EngineError;
use memory_core::sync::engine::ports::{
    AppendRequest, AppendResult, EntryKind, PortFuture, StreamHandlers, Transport,
};
use memory_core::sync::recovery::ports::KeyringAppendRequest;
use serde_json::{Value, json};

const ACCOUNT_A: &str = "acct-a";
const ACCOUNT_B: &str = "acct-b";
const TOKEN_A: &str = "tok-device-a";
const TOKEN_A_SECOND: &str = "tok-device-a2";
const TOKEN_B: &str = "tok-device-b";
const TOKEN_ADMIN: &str = "tok-admin";

struct ReqwestClient(reqwest::Client);

struct ReqwestStream(reqwest::Response);

impl ByteStream for ReqwestStream {
    fn next_chunk(&mut self) -> PortFuture<'_, Option<Vec<u8>>> {
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
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        Box::pin(async move {
            let mut builder = self.0.request(
                reqwest::Method::from_bytes(request.method.as_bytes())
                    .map_err(|error| EngineError::Other(error.to_string()))?,
                request.url,
            );
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

    fn open_stream(&self, request: HttpRequest) -> PortFuture<'_, StreamResponse> {
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
                body: Box::new(ReqwestStream(response)),
            })
        })
    }
}

/// A server bound to a loopback port, torn down when the test ends.
struct Fixture {
    base_url: String,
    handle: tokio::task::JoinHandle<std::io::Result<()>>,
    /// Kept so the database and token files outlive the server.
    _dir: tempfile::TempDir,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let tokens = dir.path().join("tokens.json");
    let table = json!({
        "tokens": [
            {
                "token": TOKEN_A,
                "account": ACCOUNT_A,
                "device": "device-a",
                "principal": "device:device-a",
                "sealing_public_key": "seal-a",
                "signing_public_key": "sign-a"
            },
            {
                "token": TOKEN_A_SECOND,
                "account": ACCOUNT_A,
                "device": "device-a2",
                "principal": "device:device-a2",
                "sealing_public_key": "seal-a2",
                "signing_public_key": "sign-a2"
            },
            {
                "token": TOKEN_B,
                "account": ACCOUNT_B,
                "device": "device-b",
                "principal": "device:device-b",
                "sealing_public_key": "seal-b",
                "signing_public_key": "sign-b"
            },
            {
                "token": TOKEN_ADMIN,
                "account": "acct-admin",
                "device": "device-admin",
                "principal": "device:device-admin",
                "admin": true
            }
        ]
    });
    std::fs::write(&tokens, table.to_string()).expect("token file");
    let db = dir.path().join("sync.db");
    let config = Config::new(db, tokens);
    let server = Server::bind(config, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("bind");
    let base_url = server.base_url().expect("base url");
    let handle = server.spawn();
    Fixture {
        base_url,
        handle,
        _dir: dir,
    }
}

fn options(fixture: &Fixture, token: &'static str) -> HttpTransportOptions {
    let source: TokenSource = Rc::new(move || Box::pin(async move { Ok(token.to_string()) }));
    HttpTransportOptions {
        base_url: fixture.base_url.clone(),
        token: source,
        client: Rc::new(ReqwestClient(reqwest::Client::new())),
    }
}

fn transport(fixture: &Fixture, token: &'static str) -> impl Transport {
    create_http_transport(options(fixture, token))
}

fn account_http(fixture: &Fixture, token: &'static str) -> AccountHttp {
    create_account_http(options(fixture, token))
}

/// A contract-shaped batch id: 22 characters of unpadded base64url, which is what the engine's
/// sealed batch id always is and the only shape the server puts on a row.
fn bid(tag: &str) -> String {
    format!("{tag:_<22}")
}

fn digest_hex(bytes: &[u8]) -> String {
    hex(&sha256(bytes))
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

/// Reserve, upload and append one batch through the real transport.
async fn push_batch(
    transport: &impl Transport,
    bytes: &[u8],
    base_seq: u64,
    batch_id: &str,
) -> AppendResult {
    let digest = digest_hex(bytes);
    let upload_id = transport
        .reserve(bytes.len(), &digest)
        .await
        .expect("reserve");
    transport.upload(&upload_id, bytes).await.expect("upload");
    transport
        .append(append_request(&upload_id, &digest, base_seq, batch_id))
        .await
        .expect("append")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn local() -> tokio::task::LocalSet {
    tokio::task::LocalSet::new()
}

#[test]
fn a_batch_lands_once_and_another_device_reads_it_back() {
    let runtime = runtime();
    let local = local();
    runtime.block_on(local.run_until(async {
        let fixture = start().await;
        let a = transport(&fixture, TOKEN_A);
        // A second device of the same account: the log is per account, not per device.
        let b = transport(&fixture, TOKEN_A_SECOND);

        let payload = b"sealed batch bytes".to_vec();
        let digest = digest_hex(&payload);
        let upload_id = a.reserve(payload.len(), &digest).await.expect("reserve");
        a.upload(&upload_id, &payload).await.expect("upload");
        assert_eq!(
            a.append(append_request(&upload_id, &digest, 0, &bid("one")))
                .await
                .expect("append"),
            AppendResult::Ok { seq: 1 }
        );

        let page = b.pull(0, 500).await.expect("pull");
        assert_eq!(page.head, 1);
        assert!(!page.more);
        assert_eq!(page.entries.len(), 1);
        let entry = &page.entries[0];
        assert_eq!(entry.seq, 1);
        assert_eq!(entry.kind, EntryKind::Batch);
        assert_eq!(entry.digest, digest);
        assert_eq!(entry.author, "device:device-a");
        assert_eq!(entry.batch_id.as_deref(), Some(bid("one").as_str()));

        // B downloads the same object and the digest the server stored matches the bytes.
        let bytes = b.download(&entry.object_id).await.expect("download");
        assert_eq!(digest_hex(&bytes), digest);
        assert_eq!(bytes, payload);
    }));
}

#[test]
fn a_stale_base_is_refused_and_a_repost_answers_its_original_seq() {
    let runtime = runtime();
    let local = local();
    runtime.block_on(local.run_until(async {
        let fixture = start().await;
        let a = transport(&fixture, TOKEN_A);

        assert_eq!(
            push_batch(&a, b"first", 0, &bid("one")).await,
            AppendResult::Ok { seq: 1 }
        );

        // A second batch that claims base 0 after the head moved is stale, and the client learns the
        // head from the state route.
        let payload = b"second".to_vec();
        let digest = digest_hex(&payload);
        let upload_id = a.reserve(payload.len(), &digest).await.expect("reserve");
        a.upload(&upload_id, &payload).await.expect("upload");
        assert_eq!(
            a.append(append_request(&upload_id, &digest, 0, &bid("two")))
                .await
                .expect("append"),
            AppendResult::StaleBase { head: 1 }
        );

        // The same batch id reposted at the base its own append moved answers the original seq
        // rather than a stale refusal: a push resumed after a crash must not duplicate.
        assert_eq!(
            a.append(append_request(&upload_id, &digest, 1, &bid("two")))
                .await
                .expect("append"),
            AppendResult::Ok { seq: 2 }
        );
        assert_eq!(
            a.append(append_request(&upload_id, &digest, 2, &bid("two")))
                .await
                .expect("repost"),
            AppendResult::Ok { seq: 2 }
        );

        let page = a.pull(0, 500).await.expect("pull");
        assert_eq!(page.head, 2);
        assert_eq!(page.entries.len(), 2);
    }));
}

#[test]
fn an_object_uploaded_with_the_wrong_bytes_is_refused() {
    let runtime = runtime();
    let local = local();
    runtime.block_on(local.run_until(async {
        let fixture = start().await;
        let a = transport(&fixture, TOKEN_A);

        let payload = b"the reserved bytes".to_vec();
        let digest = digest_hex(&payload);
        let upload_id = a.reserve(payload.len(), &digest).await.expect("reserve");
        let refused = a.upload(&upload_id, b"other bytes!!").await;
        match refused {
            Err(EngineError::Http(error)) => assert_eq!(error.code, "digest_mismatch"),
            other => panic!("expected a digest refusal, got {other:?}"),
        }
    }));
}

#[test]
fn a_keyring_append_lands_and_the_account_routes_read_it() {
    let runtime = runtime();
    let local = local();
    runtime.block_on(local.run_until(async {
        let fixture = start().await;
        let account = account_http(&fixture, TOKEN_A);

        let record = json!({
            "kind": "genesis",
            "account": ACCOUNT_A,
            "epoch": 1,
            "members": [],
            "wraps_digest": null,
            "fork_resolution": null,
        });
        let request = KeyringAppendRequest {
            record: record.clone(),
            base_seq: 0,
            link: "link-genesis".into(),
            keyring_link: "keyring-genesis".into(),
            record_sig: "sig-genesis".into(),
            wraps: vec![],
        };
        assert_eq!(
            account.append_keys(request).await.expect("append keys"),
            Ok(1)
        );

        let state = account.state().await.expect("state").expect("state body");
        assert_eq!(state.head.seq, 1);
        assert_eq!(state.head.link, "link-genesis");
        assert_eq!(state.head.keyring_link, "keyring-genesis");
        assert_eq!(state.current_epoch, 1);
        assert!(!state.rotation_pending);

        let keys = account.list_keys().await.expect("keys").expect("keys body");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0]["seq"], 1);
        assert_eq!(keys[0]["kind"], "keyring");
        assert_eq!(keys[0]["body"], record);

        let entries = account
            .list_entries_after(0)
            .await
            .expect("entries")
            .expect("entries body");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["seq"], 1);

        // A keyring record advances the head too, so the next batch bases on it.
        assert_eq!(
            push_batch(
                &transport(&fixture, TOKEN_A),
                b"after-keyring",
                1,
                &bid("after")
            )
            .await,
            AppendResult::Ok { seq: 2 }
        );
    }));
}

#[test]
fn the_event_stream_reaches_another_device_after_an_append() {
    let runtime = runtime();
    let local = local();
    runtime.block_on(local.run_until(async {
        let fixture = start().await;
        let a = transport(&fixture, TOKEN_A);
        // The stream is per account: the second device of the same account is the subscriber.
        let subscriber = transport(&fixture, TOKEN_A_SECOND);

        let (connected_tx, connected_rx) = tokio::sync::oneshot::channel::<()>();
        let (advanced_tx, mut advanced_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let connected = RefCell::new(Some(connected_tx));
        let handlers = StreamHandlers {
            on_advanced: Rc::new(move |head: u64| {
                let _ = advanced_tx.send(head);
            }),
            on_connected: Rc::new(move |up: bool| {
                if up {
                    let sender = connected.borrow_mut().take();
                    if let Some(sender) = sender {
                        let _ = sender.send(());
                    }
                }
            }),
        };
        let unsubscribe = subscriber.subscribe(handlers);

        // The subscription exists before the server answers the stream request, so a connected
        // signal here means a later append cannot be missed.
        tokio::time::timeout(Duration::from_secs(10), connected_rx)
            .await
            .expect("the event stream connects")
            .expect("connected signal");

        assert_eq!(
            push_batch(&a, b"broadcast me", 0, &bid("stream")).await,
            AppendResult::Ok { seq: 1 }
        );

        let head = tokio::time::timeout(Duration::from_secs(10), async {
            let mut seen = 0;
            while let Some(head) = advanced_rx.recv().await {
                if head >= 1 {
                    seen = head;
                    break;
                }
            }
            seen
        })
        .await
        .expect("an advanced frame arrives");
        assert_eq!(head, 1);
        unsubscribe();
    }));
}

#[test]
fn another_account_cannot_read_this_accounts_object() {
    let runtime = runtime();
    let local = local();
    runtime.block_on(local.run_until(async {
        let fixture = start().await;
        let a = transport(&fixture, TOKEN_A);
        let b = transport(&fixture, TOKEN_B);

        let payload = b"account a only".to_vec();
        let digest = digest_hex(&payload);
        let upload_id = a.reserve(payload.len(), &digest).await.expect("reserve");
        a.upload(&upload_id, &payload).await.expect("upload");
        a.append(append_request(&upload_id, &digest, 0, &bid("only")))
            .await
            .expect("append");

        // B's pull sees nothing of A's log.
        let page = b.pull(0, 500).await.expect("pull");
        assert!(page.entries.is_empty());
        assert_eq!(page.head, 0);

        // B's download of A's object id is a refusal, not the bytes.
        match b.download(&upload_id).await {
            Err(EngineError::Http(error)) => {
                assert_eq!((error.code.as_str(), error.status), ("not_found", 404));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }

        // An unknown token is refused outright.
        let unknown = transport(&fixture, "tok-nobody");
        match unknown.pull(0, 10).await {
            Err(EngineError::Http(error)) => {
                assert_eq!((error.code.as_str(), error.status), ("unauthorized", 401));
            }
            other => panic!("expected an auth refusal, got {other:?}"),
        }
    }));
}

/// The device list the rotation-on-revoke path reads reports the account's own device as current.
#[test]
fn the_device_list_marks_this_device_current_and_reports_a_revocation() {
    let runtime = runtime();
    let local = local();
    runtime.block_on(local.run_until(async {
        let fixture = start().await;
        let account = account_http(&fixture, TOKEN_A);
        let view = memory_core::sync::account::revocation_view::read_revocation_view(&account)
            .await
            .expect("device list");
        match view {
            memory_core::sync::account::revocation_view::RevocationView::Usable {
                self_principal,
                self_revoked,
                revoked,
                listed,
            } => {
                assert_eq!(self_principal, "device:device-a");
                assert!(!self_revoked);
                assert!(revoked.is_empty());
                assert!(listed.contains("device:device-a"));
            }
            other => panic!("expected a usable device list, got {other:?}"),
        }
    }));
}

/// Revoking one device sets `rotation_pending` on its account: the account's other devices answer
/// `stale_base` on batch appends until a keyring append rotates the epoch and clears it.
#[test]
fn revoking_a_device_holds_appends_until_a_keyring_append_lands() {
    let runtime = runtime();
    let local = local();
    runtime.block_on(local.run_until(async {
        let fixture = start().await;
        let a = transport(&fixture, TOKEN_A);
        let second = transport(&fixture, TOKEN_A_SECOND);
        let account = account_http(&fixture, TOKEN_A);

        assert_eq!(
            push_batch(&a, b"before revoke", 0, &bid("before")).await,
            AppendResult::Ok { seq: 1 }
        );

        // Only an admin token may revoke; a device token is refused.
        let admin = reqwest::Client::new();
        let refused: Value = admin
            .post(format!(
                "{}/v1/admin/devices/device-a2/revoke",
                fixture.base_url
            ))
            .bearer_auth(TOKEN_B)
            .send()
            .await
            .expect("revoke request")
            .json()
            .await
            .expect("revoke body");
        assert_eq!(refused["error"]["code"], "forbidden");

        let revoked: Value = admin
            .post(format!(
                "{}/v1/admin/devices/device-a2/revoke",
                fixture.base_url
            ))
            .bearer_auth(TOKEN_ADMIN)
            .send()
            .await
            .expect("revoke request")
            .json()
            .await
            .expect("revoke body");
        assert_eq!(revoked["revoked"], true);
        assert_eq!(revoked["account"], ACCOUNT_A);

        // The revoked device itself is out, and the account's remaining device is held.
        match second.pull(0, 10).await {
            Err(EngineError::Http(error)) => assert_eq!(error.code, "device_revoked"),
            other => panic!("expected the revoked device to be refused, got {other:?}"),
        }
        let state = account.state().await.expect("state").expect("state body");
        assert!(state.rotation_pending);
        let payload = b"held".to_vec();
        let digest = digest_hex(&payload);
        let upload_id = a.reserve(payload.len(), &digest).await.expect("reserve");
        a.upload(&upload_id, &payload).await.expect("upload");
        assert_eq!(
            a.append(append_request(&upload_id, &digest, 1, &bid("held")))
                .await
                .expect("append"),
            AppendResult::StaleBase { head: 1 }
        );

        // A keyring append is the rotation: it lands and clears the hold.
        let record = json!({
            "kind": "rotation",
            "account": ACCOUNT_A,
            "epoch": 2,
            "members": [],
            "wraps_digest": null,
            "fork_resolution": null,
        });
        assert_eq!(
            account
                .append_keys(KeyringAppendRequest {
                    record,
                    base_seq: 1,
                    link: "link-rotation".into(),
                    keyring_link: "keyring-rotation".into(),
                    record_sig: "sig-rotation".into(),
                    wraps: vec![],
                })
                .await
                .expect("append rotation"),
            Ok(2)
        );
        let state = account.state().await.expect("state").expect("state body");
        assert!(!state.rotation_pending);
        assert_eq!(state.current_epoch, 2);
        assert_eq!(
            a.append(append_request(&upload_id, &digest, 2, &bid("held")))
                .await
                .expect("append after rotation"),
            AppendResult::Ok { seq: 3 }
        );

        // The device list reports the revoked row's timestamp and the caller's own row as current.
        let view = memory_core::sync::account::revocation_view::read_revocation_view(&account)
            .await
            .expect("device list");
        match view {
            memory_core::sync::account::revocation_view::RevocationView::Usable {
                self_principal,
                self_revoked,
                revoked,
                ..
            } => {
                assert_eq!(self_principal, "device:device-a");
                assert!(!self_revoked);
                assert!(revoked.contains("device:device-a2"));
            }
            other => panic!("expected a usable device list, got {other:?}"),
        }
    }));
}
