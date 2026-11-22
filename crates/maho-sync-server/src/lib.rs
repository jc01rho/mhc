//! A self-hostable memory-sync server: a dumb, per-account ciphertext log that the mhc memory-sync
//! client already knows how to talk to (`/v1/sync/*`, `/v1/account/sync`, `/v1/devices*`, plus
//! `/v1/me/events`). It never sees plaintext: objects and keyring records arrive already sealed and
//! signed by the devices, and the server only orders them, checks that a writer starts from the
//! head it read, keeps one append per idempotency key, and tells connected devices that the head
//! moved. Every account is confined to its own rows.
//!
//! Tokens come from a small file (`token account device-id sync-principal [admin] [seal=...] [sign=...]`,
//! one per line): there is no registration route and no hosted service behind it.

use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
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
    pub sealing_public_key: String,
    pub signing_public_key: String,
    pub admin: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Tokens(pub Vec<DeviceToken>);

impl Tokens {
    pub fn find(&self, token: &str) -> Option<&DeviceToken> {
        self.0.iter().find(|row| row.token == token)
    }

    /// `token account device-id sync-principal [admin] [seal=...] [sign=...]` per line; blank
    /// lines and `#` comments are skipped.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut rows = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [token, account, device_id, principal, rest @ ..] = fields.as_slice() else {
                return Err(format!(
                    "tokens line {}: expected `token account device-id sync-principal`",
                    index + 1
                ));
            };
            let mut admin = false;
            let mut sealing_public_key = (*device_id).to_string();
            let mut signing_public_key = (*device_id).to_string();
            for option in rest {
                if *option == "admin" {
                    admin = true;
                } else if let Some(key) = option.strip_prefix("seal=")
                    && !key.is_empty()
                {
                    sealing_public_key = key.to_string();
                } else if let Some(key) = option.strip_prefix("sign=")
                    && !key.is_empty()
                {
                    signing_public_key = key.to_string();
                } else {
                    return Err(format!(
                        "tokens line {}: unknown field `{option}`",
                        index + 1
                    ));
                }
            }
            rows.push(DeviceToken {
                token: (*token).into(),
                account: (*account).into(),
                device_id: (*device_id).into(),
                sync_principal: (*principal).into(),
                sealing_public_key,
                signing_public_key,
                admin,
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
    confirmations: Arc<storage::Confirmations>,
}

impl ServerState {
    pub fn new(store: Store, tokens: Tokens) -> Self {
        for device in &tokens.0 {
            let _ = store.ensure_device(device);
        }
        let (events, _) = broadcast::channel(256);
        Self {
            store,
            tokens: Arc::new(tokens),
            events,
            confirmations: Arc::new(storage::Confirmations::default()),
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
        .route(
            "/v1/sync/pending",
            put(open_pending_route).get(list_pending_route),
        )
        .route("/v1/sync/pending/{id}", get(read_pending_route))
        .route("/v1/sync/pending/{id}/answer", post(answer_pending_route))
        .route("/v1/sync/pending/{id}/reveal", post(reveal_pending_route))
        .route("/v1/account/sync", put(set_account_sync_route))
        .route("/v1/confirm", post(confirm_route))
        .route("/v1/devices", get(devices_route))
        .route("/v1/devices/{id}/sync", put(set_device_sync_route))
        .route("/v1/admin/devices/{id}/revoke", post(revoke_device_route))
        .route("/v1/me/events", get(events_route))
        // Axum's default 2 MiB body limit is smaller than the client's 8 MiB object cap.
        .layer(DefaultBodyLimit::max(MAX_OBJECT_BYTES + 64 * 1024))
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
        StoreError::Refused("object_mismatch") => Refusal::new(400, "object_mismatch"),
        StoreError::Refused(code) => Refusal::new(409, code),
        StoreError::NotFound => Refusal::new(404, "not_found"),
        StoreError::Sqlite(_) => Refusal::new(503, "unavailable"),
    }
}

fn identify(state: &ServerState, headers: &HeaderMap) -> Result<DeviceToken, Refusal> {
    let unauthenticated = Refusal::new(401, "unauthenticated");
    let header = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or(unauthenticated)?;
    let token = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .ok_or(unauthenticated)?
        .trim();
    if token.is_empty() {
        return Err(unauthenticated);
    }
    state.tokens.find(token).cloned().ok_or(unauthenticated)
}

fn device_of(state: &ServerState, headers: &HeaderMap) -> Result<DeviceToken, Refusal> {
    let device = identify(state, headers)?;
    if state.store.is_revoked(&device.device_id).map_err(stored)? {
        return Err(Refusal::new(401, "device_revoked"));
    }
    Ok(device)
}

fn admin_only(state: &ServerState, headers: &HeaderMap) -> Result<DeviceToken, Refusal> {
    let device = device_of(state, headers)?;
    if !device.admin {
        return Err(Refusal::new(403, "forbidden"));
    }
    Ok(device)
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
        .rows_after(&device.account, query.after, MAX_PAGE)
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
        .chain_rows_after(&device.account, query.after, MAX_PAGE)
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
    #[serde(default)]
    kind: Option<String>,
    bytes: usize,
    #[serde(default)]
    sha256: Option<String>,
}

fn decode_digest(text: &str) -> Result<String, Refusal> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| Refusal::new(400, "malformed_digest"))?;
    if bytes.len() != 32 {
        return Err(Refusal::new(400, "malformed_digest"));
    }
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
    if let Some(kind) = &body.kind
        && kind != "batch"
    {
        return Err(Refusal::new(400, "malformed"));
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
    #[serde(default)]
    digest: Option<String>,
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
    if body
        .idempotency_key
        .as_deref()
        .is_some_and(|key| key.is_empty() || key.len() > 64)
    {
        return Err(Refusal::new(400, "malformed"));
    }
    let digest = body.digest.as_deref().map(decode_digest).transpose()?;
    let seq = state
        .store
        .append_batch(
            &device,
            storage::BatchAppend {
                upload_id: body.upload_id,
                digest,
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
    if !body.record.is_object() {
        return Err(Refusal::new(400, "malformed"));
    }
    if let Some(account) = body.record["account"].as_str()
        && account != device.account
    {
        return Err(Refusal::new(401, "cross_account"));
    }
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

// `devices_route` authenticates with `identify` rather than `device_of` so a revoked device can
// still read `GET /v1/devices` and discover its own revocation.
async fn devices_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    let device = identify(&state, &headers)?;
    let rows = state.store.devices(&device.account).map_err(stored)?;
    let devices: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "syncPrincipal": row.principal,
                "revokedAt": row.revoked_at,
                "current": row.device == device.device_id,
                "syncEnabled": row.sync_enabled,
            })
        })
        .collect();
    Ok(Json(json!({ "devices": devices })))
}

async fn revoke_device_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, Refusal> {
    admin_only(&state, &headers)?;
    let account = state
        .store
        .revoke(&id)
        .map_err(stored)?
        .ok_or_else(|| Refusal::new(404, "not_found"))?;
    Ok(Json(json!({
        "device": id,
        "account": account,
        "revoked": true,
    })))
}

#[derive(Deserialize)]
struct EnabledBody {
    enabled: bool,
    #[serde(default, rename = "confirmToken")]
    confirm_token: Option<String>,
}

async fn set_account_sync_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<EnabledBody>,
) -> Result<StatusCode, Refusal> {
    let device = device_of(&state, &headers)?;
    state
        .store
        .set_account_sync(&device.account, body.enabled)
        .map_err(stored)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ConfirmBody {
    action: String,
    target: String,
}

async fn confirm_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<ConfirmBody>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    if body.action != "sync.disable_device" {
        return Err(Refusal::new(400, "malformed"));
    }
    let exists = state
        .store
        .devices(&device.account)
        .map_err(stored)?
        .iter()
        .any(|row| row.device == body.target);
    if !exists {
        return Err(Refusal::new(404, "not_found"));
    }
    let token = state.confirmations.issue(&device.account, &body.target);
    Ok(Json(json!({ "ready": true, "token": token })))
}

async fn set_device_sync_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<EnabledBody>,
) -> Result<StatusCode, Refusal> {
    let device = device_of(&state, &headers)?;
    if !body.enabled
        && let Some(token) = body.confirm_token.as_deref()
        && !state.confirmations.spend(&device.account, &id, token)
    {
        return Err(Refusal::new(403, "forbidden"));
    }
    let changed = state
        .store
        .set_device_sync(&device.account, &id, body.enabled)
        .map_err(stored)?;
    if !changed {
        return Err(Refusal::new(404, "not_found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PendingOpen {
    account: String,
    commitment: String,
}

#[derive(Deserialize)]
struct PendingAnswer {
    principal: Value,
    approver_nonce: String,
}

#[derive(Deserialize)]
struct PendingReveal {
    principal: Value,
    joining_nonce: String,
}

fn pending_json(row: &storage::PendingRow) -> Value {
    json!({
        "id": row.id,
        "account": row.account,
        "commitment": row.commitment,
        "answer": row.answer,
        "reveal": row.reveal,
        "expires_at": row.expires_at,
    })
}

async fn open_pending_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<PendingOpen>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    if body.account != device.account {
        return Err(Refusal::new(401, "cross_account"));
    }
    if body.commitment.is_empty() {
        return Err(Refusal::new(400, "malformed"));
    }
    let row = state
        .store
        .open_pending(&device.account, &body.commitment)
        .map_err(stored)?;
    Ok(Json(pending_json(&row)))
}

async fn list_pending_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let rows = state.store.list_pending(&device.account).map_err(stored)?;
    Ok(Json(json!({
        "pending": rows.iter().map(pending_json).collect::<Vec<_>>()
    })))
}

async fn read_pending_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let row = state
        .store
        .pending(&device.account, &id)
        .map_err(stored)?
        .ok_or_else(|| Refusal::new(404, "not_found"))?;
    Ok(Json(pending_json(&row)))
}

async fn answer_pending_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<PendingAnswer>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let value = json!({ "principal": body.principal, "approver_nonce": body.approver_nonce });
    settle_pending(&state, &device, &id, storage::PendingField::Answer, &value)
}

async fn reveal_pending_route(
    State(state): State<ServerState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<PendingReveal>,
) -> Result<Json<Value>, Refusal> {
    let device = device_of(&state, &headers)?;
    let value = json!({ "principal": body.principal, "joining_nonce": body.joining_nonce });
    settle_pending(&state, &device, &id, storage::PendingField::Reveal, &value)
}

fn settle_pending(
    state: &ServerState,
    device: &DeviceToken,
    id: &str,
    field: storage::PendingField,
    value: &Value,
) -> Result<Json<Value>, Refusal> {
    let written = state
        .store
        .set_pending_field(&device.account, id, field, value)
        .map_err(stored)?;
    if !written {
        return Err(Refusal::new(409, "not_found"));
    }
    let row = state
        .store
        .pending(&device.account, id)
        .map_err(stored)?
        .ok_or_else(|| Refusal::new(404, "not_found"))?;
    Ok(Json(pending_json(&row)))
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
