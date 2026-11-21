//! The HTTP surface: every route the shipped sync client calls.
//!
//! `memory_core::sync::engine::adapters::http_transport` owns the batch log (state, batches,
//! objects, uploads, append) and the event stream; `memory_core::sync::account::http` and
//! `pairing_http` own the keyring, entries, recovery wraps and pairing routes; the account's
//! device list backs the rotation-on-revoke view. A refusal is always `{"error":{"code":...}}`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use futures::Stream;
use futures::stream::unfold;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::MAX_OBJECT_BYTES;
use crate::auth::{Principal, TokenTable};
use crate::canonical::{canonical_digest, digest_of};
use crate::store::{AppendRefusal, NewEntry, PendingField, Store};
use crate::wire::{EntryKind, EntryRow};

/// Rows one batch page may carry.
pub const MAX_PAGE: usize = 500;

/// The process state behind the router.
pub struct AppState {
    pub store: Store,
    pub tokens: TokenTable,
    events: Mutex<BTreeMap<String, broadcast::Sender<u64>>>,
}

impl AppState {
    pub fn new(store: Store, tokens: TokenTable) -> Self {
        Self {
            store,
            tokens,
            events: Mutex::new(BTreeMap::new()),
        }
    }

    fn subscribe(&self, account: &str) -> broadcast::Receiver<u64> {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        events
            .entry(account.to_string())
            .or_insert_with(|| broadcast::channel(64).0)
            .subscribe()
    }

    fn announce(&self, account: &str, head: u64) {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sender) = events.get(account) {
            // No subscriber is not an error: the stream is a trigger, never the data path.
            let _ = sender.send(head);
        }
    }
}

/// A refusal with the contract's `error.code` body.
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str) -> Self {
        Self { status, code }
    }

    fn unauthorized(code: &'static str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code)
    }

    fn not_found(code: &'static str) -> Self {
        Self::new(StatusCode::NOT_FOUND, code)
    }

    fn bad_request(code: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code)
    }

    fn conflict(code: &'static str) -> Self {
        Self::new(StatusCode::CONFLICT, code)
    }

    fn unavailable(error: impl std::fmt::Display) -> Self {
        eprintln!("mhc-sync-server: {error}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "unavailable")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            axum::Json(json!({"error": {"code": self.code}})),
        )
            .into_response()
    }
}

type ApiResult = Result<Response, ApiError>;

fn ok_json(value: Value) -> ApiResult {
    Ok(axum::Json(value).into_response())
}

/// The caller's identity: the bearer token decides the account, device and author. A revoked
/// device is refused here, on every route but the device list.
fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let principal = identify(state, headers)?;
    // A revoked device is out: rotation happens on the account's other devices.
    if state
        .store
        .is_revoked(&principal.device)
        .map_err(ApiError::unavailable)?
    {
        return Err(ApiError::unauthorized("device_revoked"));
    }
    Ok(principal)
}

/// The token's identity without the revocation check: a revoked device may still read the device
/// list, which is how it learns it was revoked.
fn identify(state: &AppState, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("unauthorized"))?;
    let token = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .ok_or_else(|| ApiError::unauthorized("unauthorized"))?
        .trim();
    if token.is_empty() {
        return Err(ApiError::unauthorized("unauthorized"));
    }
    state
        .tokens
        .lookup(token)
        .cloned()
        .ok_or_else(|| ApiError::unauthorized("unauthorized"))
}

fn admin_only(state: &AppState, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let principal = authenticate(state, headers)?;
    if !principal.admin {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
    }
    Ok(principal)
}

#[derive(Deserialize)]
struct AfterQuery {
    #[serde(default)]
    after: Option<u64>,
}

#[derive(Deserialize)]
struct ReserveRequest {
    #[serde(default)]
    kind: Option<String>,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct AppendRequest {
    upload_id: String,
    digest: String,
    base_seq: u64,
    idempotency_key: String,
    #[serde(default)]
    epoch: u64,
    #[serde(default)]
    link: String,
    #[serde(default)]
    record_sig: String,
}

#[derive(Deserialize)]
struct KeyringAppend {
    record: Value,
    base_seq: u64,
    link: String,
    keyring_link: String,
    record_sig: String,
    #[serde(default)]
    wraps: Vec<Value>,
}

#[derive(Deserialize)]
struct EnabledBody {
    enabled: bool,
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

/// The whole route table.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/sync/state", get(state_route))
        .route("/v1/sync/batches", get(batches).post(append))
        .route("/v1/sync/objects/{id}", get(object))
        .route("/v1/sync/uploads", post(reserve))
        .route("/v1/sync/uploads/{id}", put(fill))
        .route("/v1/sync/entries", get(entries))
        .route("/v1/sync/keys", get(keys).put(append_keyring))
        .route("/v1/sync/recovery-wraps", get(recovery_wraps))
        .route("/v1/sync/recovery/join", post(append_keyring))
        .route("/v1/sync/pending", put(open_pending).get(list_pending))
        .route("/v1/sync/pending/{id}", get(read_pending))
        .route("/v1/sync/pending/{id}/answer", post(answer_pending))
        .route("/v1/sync/pending/{id}/reveal", post(reveal_pending))
        .route("/v1/account/sync", put(set_account_sync))
        .route("/v1/devices", get(devices))
        .route("/v1/devices/{id}/sync", put(set_device_sync))
        .route("/v1/me/events", get(events))
        .route("/v1/admin/devices/{id}/revoke", post(revoke))
        // Room for one object plus framing; the reserve route enforces the exact cap.
        .layer(DefaultBodyLimit::max(MAX_OBJECT_BYTES + 64 * 1024))
        .with_state(state)
}

/// `GET /v1/sync/state`: the head the engine pages from and the epoch/rotation the keyring path
/// reads. Nothing is compacted, so `oldest_retained_seq` is 0 and there is never a snapshot.
async fn state_route(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let head = state
        .store
        .head(&principal.account)
        .map_err(ApiError::unavailable)?;
    ok_json(json!({
        "head": {
            "seq": head.seq,
            "link": head.link,
            "keyring_link": head.keyring_link,
        },
        "oldest_retained_seq": 0,
        "snapshot": Value::Null,
        "current_epoch": head.current_epoch,
        "rotation_pending": head.rotation_pending,
    }))
}

/// `GET /v1/sync/batches?after=n`: the engine's pull page.
async fn batches(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AfterQuery>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let rows = state
        .store
        .entries_after(&principal.account, query.after.unwrap_or(0), MAX_PAGE, None)
        .map_err(ApiError::unavailable)?;
    let entries: Vec<Value> = rows.iter().map(EntryRow::listing).collect();
    ok_json(json!({"entries": entries}))
}

/// `GET /v1/sync/entries?after=n`: the account chain's full rows.
async fn entries(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AfterQuery>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let rows = state
        .store
        .entries_after(&principal.account, query.after.unwrap_or(0), MAX_PAGE, None)
        .map_err(ApiError::unavailable)?;
    let entries: Vec<Value> = rows.iter().map(EntryRow::full).collect();
    ok_json(json!({"entries": entries}))
}

/// `GET /v1/sync/keys`: every keyring record, which the chain pairs with `/v1/sync/entries`.
async fn keys(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let rows = state
        .store
        .entries_after(&principal.account, 0, MAX_PAGE, Some(EntryKind::Keyring))
        .map_err(ApiError::unavailable)?;
    let entries: Vec<Value> = rows.iter().map(EntryRow::full).collect();
    ok_json(json!({"entries": entries}))
}

/// `GET /v1/sync/recovery-wraps`: each keyring record's wraps, keyed by that record.
async fn recovery_wraps(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let rows = state
        .store
        .entries_after(&principal.account, 0, MAX_PAGE, Some(EntryKind::Keyring))
        .map_err(ApiError::unavailable)?;
    let wraps: Vec<Value> = rows.iter().flat_map(EntryRow::recovery_wraps).collect();
    ok_json(json!({"wraps": wraps}))
}

/// `GET /v1/sync/objects/{id}`: raw ciphertext, digest-checked by the client on arrival.
async fn object(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let bytes = state
        .store
        .object(&principal.account, &id)
        .map_err(ApiError::unavailable)?
        .ok_or_else(|| ApiError::not_found("not_found"))?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    )
        .into_response())
}

/// `POST /v1/sync/uploads`: reserve a slot for exactly this many bytes with this digest.
async fn reserve(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<ReserveRequest>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    if request.bytes > MAX_OBJECT_BYTES as u64 {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large"));
    }
    if request.sha256.len() != 43 {
        return Err(ApiError::bad_request("malformed"));
    }
    if let Some(kind) = &request.kind
        && kind != "batch"
    {
        return Err(ApiError::bad_request("malformed"));
    }
    let upload_id = state
        .store
        .reserve(&principal.account, request.bytes, &request.sha256)
        .map_err(ApiError::unavailable)?;
    ok_json(json!({"upload_id": upload_id}))
}

/// `PUT /v1/sync/uploads/{id}`: the object bytes, checked against the reserved digest.
async fn fill(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    if body.len() > MAX_OBJECT_BYTES {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large"));
    }
    let upload = state
        .store
        .upload(&principal.account, &id)
        .map_err(ApiError::unavailable)?
        .ok_or_else(|| ApiError::not_found("not_found"))?;
    if upload.size != body.len() as u64 || upload.sha256 != digest_of(&body) {
        return Err(ApiError::conflict("digest_mismatch"));
    }
    state
        .store
        .fill(&principal.account, &id, &body)
        .map_err(ApiError::unavailable)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /v1/sync/batches`: append the uploaded object at `base_seq + 1`.
async fn append(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<AppendRequest>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    if request.idempotency_key.is_empty() || request.idempotency_key.len() > 64 {
        return Err(ApiError::bad_request("malformed"));
    }
    let upload = state
        .store
        .upload(&principal.account, &request.upload_id)
        .map_err(ApiError::unavailable)?
        .ok_or_else(|| ApiError::not_found("not_found"))?;
    if !upload.filled || upload.sha256 != request.digest {
        return Err(ApiError::bad_request("object_mismatch"));
    }
    let entry = NewEntry {
        base_seq: request.base_seq,
        kind: EntryKind::Batch,
        epoch: request.epoch.max(1),
        author: principal.author(),
        digest: request.digest,
        link: request.link,
        keyring_link: None,
        record_sig: request.record_sig,
        object_id: Some(request.upload_id),
        batch_id: Some(request.idempotency_key),
        body: None,
        wraps: None,
    };
    match state
        .store
        .append(&principal.account, entry)
        .map_err(ApiError::unavailable)?
    {
        Ok((seq, landed)) => {
            if landed {
                state.announce(&principal.account, seq);
            }
            ok_json(json!({"seq": seq}))
        }
        Err(AppendRefusal::StaleBase) => Err(ApiError::conflict("stale_base")),
    }
}

/// `PUT /v1/sync/keys` and `POST /v1/sync/recovery/join`: a signed keyring record.
///
/// The digest is the canonical hash of the record, which is exactly what the device signed, and
/// the server computes it rather than trusting the body: the chain links records by that digest.
async fn append_keyring(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<KeyringAppend>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    if !request.record.is_object() {
        return Err(ApiError::bad_request("malformed"));
    }
    let record_account = request.record["account"].as_str().unwrap_or_default();
    if record_account != principal.account {
        return Err(ApiError::unauthorized("cross_account"));
    }
    let entry = NewEntry {
        base_seq: request.base_seq,
        kind: EntryKind::Keyring,
        epoch: request.record["epoch"].as_u64().unwrap_or(1).max(1),
        author: principal.author(),
        digest: canonical_digest(&request.record),
        link: request.link,
        keyring_link: Some(request.keyring_link),
        record_sig: request.record_sig,
        object_id: None,
        batch_id: None,
        body: Some(request.record),
        wraps: Some(Value::Array(request.wraps)),
    };
    match state
        .store
        .append(&principal.account, entry)
        .map_err(ApiError::unavailable)?
    {
        Ok((seq, landed)) => {
            if landed {
                state.announce(&principal.account, seq);
            }
            ok_json(json!({"seq": seq}))
        }
        Err(AppendRefusal::StaleBase) => Err(ApiError::conflict("stale_base")),
    }
}

/// `GET /v1/devices`: the rows the rotation-on-revoke view reads.
async fn devices(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    let principal = identify(&state, &headers)?;
    let rows = state
        .store
        .devices(&principal.account)
        .map_err(ApiError::unavailable)?;
    let devices: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "syncPrincipal": row.principal,
                "revokedAt": row.revoked_at,
                "current": row.device == principal.device,
                "syncEnabled": row.sync_enabled,
            })
        })
        .collect();
    ok_json(json!({"devices": devices}))
}

/// `PUT /v1/account/sync`: sync on or off for the whole account.
async fn set_account_sync(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<EnabledBody>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    state
        .store
        .set_account_sync(&principal.account, body.enabled)
        .map_err(ApiError::unavailable)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `PUT /v1/devices/{id}/sync`: sync on or off for one device of the account.
async fn set_device_sync(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<EnabledBody>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let changed = state
        .store
        .set_device_sync(&principal.account, &id, body.enabled)
        .map_err(ApiError::unavailable)?;
    if !changed {
        return Err(ApiError::not_found("not_found"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `PUT /v1/sync/pending`: open a pairing row.
async fn open_pending(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<PendingOpen>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    if body.account != principal.account {
        return Err(ApiError::unauthorized("cross_account"));
    }
    if body.commitment.is_empty() {
        return Err(ApiError::bad_request("malformed"));
    }
    let row = state
        .store
        .open_pending(&principal.account, &body.commitment)
        .map_err(ApiError::unavailable)?;
    ok_json(pending_json(&row))
}

fn pending_json(row: &crate::store::PendingRow) -> Value {
    json!({
        "id": row.id,
        "account": row.account,
        "commitment": row.commitment,
        "answer": row.answer,
        "reveal": row.reveal,
        "expires_at": row.expires_at,
    })
}

/// `GET /v1/sync/pending`: the account's open rows.
async fn list_pending(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let rows = state
        .store
        .list_pending(&principal.account)
        .map_err(ApiError::unavailable)?;
    ok_json(json!({"pending": rows.iter().map(pending_json).collect::<Vec<_>>()}))
}

/// `GET /v1/sync/pending/{id}`.
async fn read_pending(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let row = state
        .store
        .pending(&principal.account, &id)
        .map_err(ApiError::unavailable)?
        .ok_or_else(|| ApiError::not_found("not_found"))?;
    ok_json(pending_json(&row))
}

/// `POST /v1/sync/pending/{id}/answer`: the approving device's half.
async fn answer_pending(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<PendingAnswer>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let value = json!({"principal": body.principal, "approver_nonce": body.approver_nonce});
    settle_pending(&state, &principal, &id, PendingField::Answer, value)
}

/// `POST /v1/sync/pending/{id}/reveal`: the joining device's half.
async fn reveal_pending(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<PendingReveal>,
) -> ApiResult {
    let principal = authenticate(&state, &headers)?;
    let value = json!({"principal": body.principal, "joining_nonce": body.joining_nonce});
    settle_pending(&state, &principal, &id, PendingField::Reveal, value)
}

fn settle_pending(
    state: &AppState,
    principal: &Principal,
    id: &str,
    field: PendingField,
    value: Value,
) -> ApiResult {
    let written = state
        .store
        .set_pending_field(&principal.account, id, field, &value)
        .map_err(ApiError::unavailable)?;
    if !written {
        return Err(ApiError::conflict("not_found"));
    }
    let row = state
        .store
        .pending(&principal.account, id)
        .map_err(ApiError::unavailable)?
        .ok_or_else(|| ApiError::not_found("not_found"))?;
    ok_json(pending_json(&row))
}

/// `POST /v1/admin/devices/{id}/revoke`: mark a device revoked and hold the account's appends
/// until a keyring append rotates the epoch.
async fn revoke(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    admin_only(&state, &headers)?;
    let account = state
        .store
        .revoke(&id)
        .map_err(ApiError::unavailable)?
        .ok_or_else(|| ApiError::not_found("not_found"))?;
    ok_json(json!({"device": id, "account": account, "revoked": true}))
}

/// `GET /v1/me/events`: the trigger stream. Each `sync.advanced` frame carries the head that a
/// landed append moved to; the client reads the data itself through the pull routes.
async fn events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, ApiError> {
    let principal = authenticate(&state, &headers)?;
    let head = state
        .store
        .head(&principal.account)
        .map_err(ApiError::unavailable)?;
    let receiver = state.subscribe(&principal.account);
    // The first frame states the current head, so a device that connects late still learns it.
    let first = Some(advanced_frame(head.seq));
    let stream = unfold(
        (receiver, first),
        |(mut receiver, mut pending)| async move {
            loop {
                if let Some(frame) = pending.take() {
                    return Some((Ok(frame), (receiver, pending)));
                }
                match receiver.recv().await {
                    Ok(seq) => return Some((Ok(advanced_frame(seq)), (receiver, pending))),
                    // A lagging subscriber skipped frames; the head it reads next is still current.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Ok(Sse::new(stream))
}

fn advanced_frame(seq: u64) -> Event {
    Event::default().data(json!({"type": "sync.advanced", "head": {"seq": seq}}).to_string())
}
