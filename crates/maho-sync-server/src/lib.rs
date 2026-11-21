//! A self-hostable memory-sync server: a dumb, per-account ciphertext log that the mhc memory-sync
//! client already knows how to talk to (`/v1/sync/*` plus `/v1/me/events`). It never sees plaintext:
//! objects and keyring records arrive already sealed and signed by the devices, and the server only
//! orders them, checks that a writer starts from the head it read, keeps one append per idempotency
//! key, and tells connected devices that the head moved. Every account is confined to its own rows.
//!
//! Route contract, as the client sends it (`crates/omo/memory-core/src/sync`):
//! - `GET  /v1/sync/state` -> `{head:{seq,link,keyring_link},current_epoch,rotation_pending,oldest_retained_seq,snapshot:null}`
//! - `GET  /v1/sync/batches?after=n` -> `{entries:[{seq,kind,author:{kind,id},digest,object_id,batch_id}]}`
//! - `GET  /v1/sync/entries?after=n` -> the same rows, keyring rows included, for the account walk
//! - `GET  /v1/sync/keys` -> `{entries:[<full keyring rows>]}`
//! - `GET  /v1/sync/objects/{id}` -> the stored bytes; `GET /v1/sync/recovery-wraps` -> `{wraps:[{record,entry}]}`
//! - `POST /v1/sync/uploads {kind,bytes,sha256}` -> `{upload_id}`, then `PUT /v1/sync/uploads/{id}` raw bytes
//! - `POST /v1/sync/batches` / `PUT /v1/sync/keys` / `POST /v1/sync/recovery/join` -> `{seq}` or a
//!   `{error:{code}}` refusal (`stale_base` while `rotation_pending` or on a moved base)
//! - `GET  /v1/me/events` -> an SSE stream of `sync.advanced` / `sync.keyring.changed` frames carrying the head
//!
//! Tokens come from a small file (`token account device principal`, one per line): there is no
//! registration route and no hosted service behind it.

use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

pub mod storage;

pub use storage::{Store, StoreError};

/// The largest object a batch or snapshot may carry; the client's own batch limit is 8 MiB.
pub const MAX_OBJECT_BYTES: usize = 8 * 1024 * 1024;
/// Rows a pull page returns at most, matching the client's own page bound.
pub const MAX_PAGE: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceToken {
    pub token: String,
    pub account: String,
    pub device_id: String,
    pub sync_principal: String,
}

#[derive(Debug, Clone, Default)]
pub struct Tokens(pub Vec<DeviceToken>);

impl Tokens {
    pub fn find(&self, token: &str) -> Option<&DeviceToken> {
        self.0.iter().find(|row| row.token == token)
    }

    /// `token account device-id sync-principal` per line; blank lines and `#` comments are skipped.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut rows = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [token, account, device_id, principal] = fields.as_slice() else {
                return Err(format!(
                    "tokens line {}: expected `token account device-id sync-principal`",
                    index + 1
                ));
            };
            rows.push(DeviceToken {
                token: (*token).into(),
                account: (*account).into(),
                device_id: (*device_id).into(),
                sync_principal: (*principal).into(),
            });
        }
        Ok(Self(rows))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Self::parse(&text)
    }
}

#[derive(Debug, Clone)]
struct HeadEvent {
    account: String,
    seq: u64,
    keyring: bool,
}

#[derive(Clone)]
pub struct ServerState {
    store: Store,
    tokens: Arc<Tokens>,
    events: broadcast::Sender<HeadEvent>,
}

impl ServerState {
    pub fn new(store: Store, tokens: Tokens) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            store,
            tokens: Arc::new(tokens),
            events,
        }
    }

    pub fn in_memory(tokens: Tokens) -> Result<Self, StoreError> {
        Ok(Self::new(Store::open_in_memory()?, tokens))
    }

    pub fn open(path: &Path, tokens: Tokens) -> Result<Self, StoreError> {
        Ok(Self::new(Store::open(path)?, tokens))
    }

    /// The store, for an operator's own tooling (revoking a device sets `rotation_pending`).
    pub fn store(&self) -> &Store {
        &self.store
    }

    fn announce(&self, account: &str, seq: u64, keyring: bool) {
        let _ = self.events.send(HeadEvent {
            account: account.to_string(),
            seq,
            keyring,
        });
    }
}

/// The router this server serves. A caller mounts it on any `axum::serve` listener.
pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/v1/sync/state", get(state_route))
        .route("/v1/sync/batches", get(batches_route).post(append_batch))
        .route("/v1/sync/entries", get(entries_route))
        .route("/v1/sync/keys", get(keys_route).put(append_key))
        .route("/v1/sync/objects/{id}", get(object_route))
        .route("/v1/sync/uploads", post(reserve_route))
        .route("/v1/sync/uploads/{id}", put(upload_route))
        .route("/v1/sync/recovery-wraps", get(recovery_wraps_route))
        .route("/v1/sync/recovery/join", post(append_key))
        .route("/v1/me/events", get(events_route))
        .with_state(state)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    pub code: &'static str,
}

impl Refusal {
    const fn new(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(json!({ "error": { "code": self.code } }))).into_response()
    }
}

fn stored(error: StoreError) -> Refusal {
    match error {
        StoreError::Stale => Refusal::new(409, "stale_base"),
        StoreError::Refused(code) => Refusal::new(409, code),
        StoreError::NotFound => Refusal::new(404, "not_found"),
        StoreError::Sqlite(_) => Refusal::new(503, "unavailable"),
    }
}

fn device_of(state: &ServerState, headers: &HeaderMap) -> Result<DeviceToken, Refusal> {
    let unauthenticated = Refusal::new(401, "unauthenticated");
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(unauthenticated)?;
    state.tokens.find(token).cloned().ok_or(unauthenticated)
}

#[derive(Deserialize)]
struct AfterQuery {
    #[serde(default)]
    after: u64,
}

async fn state_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    Ok(Json(state.store.summary(&device.account).map_err(stored)?))
}

async fn batches_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<AfterQuery>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let rows = state
        .store
        .rows_after(&device.account, query.after, MAX_PAGE, false)
        .map_err(stored)?;
    Ok(Json(json!({ "entries": rows })))
}

async fn entries_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<AfterQuery>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let rows = state
        .store
        .rows_after(&device.account, query.after, MAX_PAGE, true)
        .map_err(stored)?;
    Ok(Json(json!({ "entries": rows })))
}

async fn keys_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let rows = state.store.keyring_rows(&device.account).map_err(stored)?;
    Ok(Json(json!({ "entries": rows })))
}

async fn recovery_wraps_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let rows = state
        .store
        .recovery_wrap_rows(&device.account)
        .map_err(stored)?;
    Ok(Json(json!({ "wraps": rows })))
}

async fn object_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, Refusal> {
    let device = device_of(&state, &headers)?;
    let bytes = state.store.object(&device.account, &id).map_err(stored)?;
    Ok((StatusCode::OK, bytes).into_response())
}

#[derive(Deserialize)]
struct ReserveBody {
    bytes: usize,
    #[serde(default)]
    sha256: Option<String>,
}

fn decode_digest(text: &str) -> Result<String, Refusal> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| Refusal::new(400, "malformed_digest"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn reserve_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<ReserveBody>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    if body.bytes > MAX_OBJECT_BYTES {
        return Err(Refusal::new(413, "too_large"));
    }
    let digest = body.sha256.as_deref().map(decode_digest).transpose()?;
    let upload_id = state
        .store
        .reserve(&device.account, body.bytes, digest)
        .map_err(stored)?;
    Ok(Json(json!({ "upload_id": upload_id })))
}

async fn upload_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<StatusCode, Refusal> {
    let device = device_of(&state, &headers)?;
    if body.len() > MAX_OBJECT_BYTES {
        return Err(Refusal::new(413, "too_large"));
    }
    state
        .store
        .upload(&device.account, &id, &body)
        .map_err(stored)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct BatchBody {
    upload_id: String,
    base_seq: u64,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    epoch: u64,
    #[serde(default)]
    link: String,
    #[serde(default)]
    record_sig: String,
}

async fn append_batch(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<BatchBody>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let seq = state
        .store
        .append_batch(
            &device,
            storage::BatchAppend {
                upload_id: body.upload_id,
                base_seq: body.base_seq,
                idempotency_key: body.idempotency_key,
                epoch: body.epoch.max(1),
                link: body.link,
                record_sig: body.record_sig,
            },
        )
        .map_err(stored)?;
    state.announce(&device.account, seq, false);
    Ok(Json(json!({ "seq": seq })))
}

#[derive(Deserialize)]
struct KeyringBody {
    record: Value,
    base_seq: u64,
    #[serde(default)]
    link: String,
    #[serde(default)]
    keyring_link: String,
    #[serde(default)]
    record_sig: String,
    #[serde(default)]
    wraps: Vec<Value>,
}

async fn append_key(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<KeyringBody>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let seq = state
        .store
        .append_keyring(
            &device,
            storage::KeyringAppend {
                record: body.record,
                base_seq: body.base_seq,
                link: body.link,
                keyring_link: body.keyring_link,
                record_sig: body.record_sig,
                wraps: body.wraps,
            },
        )
        .map_err(stored)?;
    state.announce(&device.account, seq, true);
    Ok(Json(json!({ "seq": seq })))
}

fn frame(event: &HeadEvent) -> Event {
    let kind = if event.keyring {
        "sync.keyring.changed"
    } else {
        "sync.advanced"
    };
    Event::default().data(json!({ "type": kind, "head": { "seq": event.seq } }).to_string())
}

async fn events_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, Refusal> {
    let device = device_of(&state, &headers)?;
    let receiver = state.events.subscribe();
    let account = device.account;
    let stream = futures_util::stream::unfold(receiver, move |mut receiver| {
        let account = account.clone();
        async move {
            loop {
                match receiver.recv().await {
                    Ok(event) if event.account == account => {
                        return Some((Ok(frame(&event)), receiver));
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
