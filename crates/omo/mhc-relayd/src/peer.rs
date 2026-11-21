//! The peer link between two mhc-relayd (`docs/specs/mhc-relay-v1.md`).
//!
//! Plain TCP, loopback or tailnet only (`maho_service_endpoints::check_peer_host`): the overlay
//! encrypts and the link authenticates. Handshake, one per connection, each side with a fresh nonce:
//!
//! ```text
//! dialer   peer.hello {v, account, device, public_key, nonce: Nc}        --->
//!                     <--- peer.welcome {v, device, public_key, nonce: Ns, signature: S_s}  | peer.refused {reason}
//! dialer   peer.prove {signature: S_c}                                   --->
//!                     <--- peer.ready                                                      | peer.refused {reason}
//! ```
//!
//! `S_s` signs `mhc-relay-v1/server` + account + Nc + Ns + the dialer's key + the listener's key,
//! `S_c` signs `mhc-relay-v1/client` + account + Ns + Nc + the listener's key + the dialer's key (one
//! per line). Both nonces are in both transcripts, so a recorded handshake never proves a new one.
//! Each side admits only a public key its own `relayd.json` lists, under the same account. Then
//! `peer.request {id, op, ...}` / `peer.response {id, ok, data | error}`.

use std::sync::Arc;
use std::time::Duration;

use device_service_core::local_api::LocalApiError;
use device_service_core::local_api::proof::mint_nonce;
use maho_service_endpoints::check_peer_host;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::config::{PeerConfig, address_host};
use crate::identity::{decode_public_key, device_id_of, verify_signature};
use crate::ledger::Admission;
use crate::session_host::HostCommandOutcome;
use crate::state::{RelaydState, platform};
use crate::wire::{LineReader, write_value};

pub const PEER_PROTOCOL_VERSION: u64 = 1;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a peer call did not get an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerError {
    /// No connection: the device is offline or unreachable. Nothing was sent.
    Unreachable(String),
    /// The other side refused the link (reason), or this side refused to dial it. Nothing was sent.
    Refused(String),
    /// The request was sent and answered with an error.
    Failed { code: String, reason: String },
    /// The request may have been sent; no answer came.
    Lost(String),
}

impl PeerError {
    pub fn reason(&self) -> String {
        match self {
            Self::Unreachable(reason) | Self::Refused(reason) | Self::Lost(reason) => {
                reason.clone()
            }
            Self::Failed { code, reason } => format!("{code}: {reason}"),
        }
    }
}

fn transcript(
    role: &str,
    account: &str,
    first_nonce: &str,
    second_nonce: &str,
    first_key: &str,
    second_key: &str,
) -> Vec<u8> {
    format!(
        "mhc-relay-v1/{role}\n{account}\n{first_nonce}\n{second_nonce}\n{first_key}\n{second_key}"
    )
    .into_bytes()
}

fn refused(reason: &str) -> Value {
    json!({ "type": "peer.refused", "reason": reason })
}

fn text<'a>(message: &'a Value, key: &str) -> Option<&'a str> {
    message.get(key).and_then(Value::as_str)
}

/// The listening side of one connection: the handshake, then requests until the dialer closes.
pub async fn serve_peer(state: Arc<RelaydState>, stream: TcpStream) {
    let (read, mut write) = stream.into_split();
    let mut reader = LineReader::new(read);
    let handshake = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        accept_handshake(&state, &mut reader, &mut write),
    )
    .await;
    let admitted = match handshake {
        Ok(Ok(peer)) => peer,
        Ok(Err(reason)) => {
            let _ = write_value(&mut write, &refused(&reason)).await;
            return;
        }
        Err(_) => return,
    };
    state.record_peer_seen(&admitted.device_id);
    while let Ok(Some(request)) = reader.next_value().await {
        let id = text(&request, "id").unwrap_or_default().to_owned();
        let answer = match answer_request(&state, &admitted, &request).await {
            Ok(data) => json!({ "id": id, "type": "peer.response", "ok": true, "data": data }),
            Err((code, reason)) => json!({
                "id": id,
                "type": "peer.response",
                "ok": false,
                "error": { "code": code, "reason": reason },
            }),
        };
        if write_value(&mut write, &answer).await.is_err() {
            break;
        }
    }
}

struct AdmittedPeer {
    device_id: String,
}

async fn accept_handshake(
    state: &RelaydState,
    reader: &mut LineReader<OwnedReadHalf>,
    write: &mut OwnedWriteHalf,
) -> Result<AdmittedPeer, String> {
    let hello = reader
        .next_value()
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "malformed".to_owned())?;
    if text(&hello, "type") != Some("peer.hello") {
        return Err("malformed".into());
    }
    if hello.get("v").and_then(Value::as_u64) != Some(PEER_PROTOCOL_VERSION) {
        return Err("version".into());
    }
    if !state.serving() {
        return Err("service_disabled".into());
    }
    if text(&hello, "account") != Some(state.config.account.as_str()) {
        return Err("account_mismatch".into());
    }
    let dialer_key_text = text(&hello, "public_key").ok_or("malformed")?.to_owned();
    let dialer_nonce = text(&hello, "nonce").ok_or("malformed")?.to_owned();
    let dialer_key = decode_public_key(&dialer_key_text).ok_or("malformed")?;
    let dialer_id = device_id_of(&dialer_key.to_bytes());
    let listed = state
        .config
        .peers
        .iter()
        .any(|peer| peer.device_id().as_deref() == Some(dialer_id.as_str()));
    if !listed {
        return Err("unknown_device".into());
    }
    let nonce = mint_nonce().map_err(|error| error.to_string())?;
    let own_key = state.identity.public_key();
    let signature = state.identity.sign(&transcript(
        "server",
        &state.config.account,
        &dialer_nonce,
        &nonce,
        &dialer_key_text,
        &own_key,
    ));
    let welcome = json!({
        "type": "peer.welcome",
        "v": PEER_PROTOCOL_VERSION,
        "device": { "id": state.identity.id(), "name": state.identity.name(), "platform": platform() },
        "public_key": own_key,
        "nonce": nonce,
        "signature": signature,
    });
    write_value(write, &welcome)
        .await
        .map_err(|error| error.to_string())?;
    let prove = reader
        .next_value()
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "malformed".to_owned())?;
    let proof = text(&prove, "signature").unwrap_or_default();
    let expected = transcript(
        "client",
        &state.config.account,
        &nonce,
        &dialer_nonce,
        &own_key,
        &dialer_key_text,
    );
    if text(&prove, "type") != Some("peer.prove")
        || !verify_signature(&dialer_key, &expected, proof)
    {
        return Err("bad_signature".into());
    }
    write_value(write, &json!({ "type": "peer.ready" }))
        .await
        .map_err(|error| error.to_string())?;
    Ok(AdmittedPeer {
        device_id: dialer_id,
    })
}

async fn answer_request(
    state: &RelaydState,
    peer: &AdmittedPeer,
    request: &Value,
) -> Result<Value, (String, String)> {
    let fail = |code: &str, reason: &str| Err((code.to_owned(), reason.to_owned()));
    if text(request, "type") != Some("peer.request") {
        return fail("malformed", "not a peer.request");
    }
    if !state.serving() {
        return fail("service_disabled", "remote sessions are off on this device");
    }
    match text(request, "op") {
        Some("describe") => Ok(json!({
            "device": { "id": state.identity.id(), "name": state.identity.name(), "platform": platform() },
            "status": state.status()["state"],
            "control_attached": state.control_attached(),
        })),
        Some("sessions") => {
            if !state.control_attached() {
                return fail(
                    "forbidden",
                    "this device does not let other devices view its sessions",
                );
            }
            let sessions = state
                .host
                .list_sessions(&state.agent_dirs())
                .await
                .map_err(|error| ("host_not_answering".to_owned(), error))?;
            Ok(json!({ "sessions": sessions }))
        }
        Some("command") => {
            let (Some(session_id), Some(command_id), Some(rpc)) = (
                text(request, "session_id"),
                text(request, "command_id"),
                request.get("rpc"),
            ) else {
                return fail(
                    "invalid_request",
                    "command needs session_id, command_id and rpc",
                );
            };
            if !state.control_attached() {
                return Ok(
                    json!({ "state": "engine_refused", "reason": "peer_unauthorized", "retryable": false }),
                );
            }
            let key = format!("{}/{command_id}", peer.device_id);
            let admission = state.ledger().admit(&key, session_id, rpc);
            match admission {
                Admission::Conflict => fail(
                    "command_id_conflict",
                    "another command used this command_id",
                ),
                Admission::Repeat(Some(outcome)) => Ok(outcome),
                Admission::Repeat(None) => {
                    Ok(json!({ "state": "uncertain", "reason": "unconfirmed", "retryable": false }))
                }
                Admission::New => {
                    let outcome = command_state(
                        state
                            .host
                            .command(&state.agent_dirs(), session_id, rpc)
                            .await,
                    );
                    state.ledger().settle(&key, outcome.clone());
                    Ok(outcome)
                }
            }
        }
        _ => fail("unknown_type", "unknown peer op"),
    }
}

/// A host outcome as the terminal `session.command_state` fields.
pub fn command_state(outcome: HostCommandOutcome) -> Value {
    match outcome {
        HostCommandOutcome::Applied(receipt) => {
            json!({ "state": "engine_applied", "retryable": false, "receipt": receipt })
        }
        HostCommandOutcome::Refused(reason) => {
            json!({ "state": "engine_refused", "reason": reason, "retryable": false })
        }
        HostCommandOutcome::NotListed => {
            json!({ "state": "engine_refused", "reason": "route_unlisted", "retryable": false })
        }
        HostCommandOutcome::Uncertain(reason) => {
            json!({ "state": "uncertain", "reason": reason, "retryable": false })
        }
    }
}

struct PeerLink {
    reader: LineReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

async fn dial(state: &RelaydState, peer: &PeerConfig) -> Result<PeerLink, PeerError> {
    let host = address_host(&peer.address)
        .ok_or_else(|| PeerError::Refused("malformed_address".into()))?;
    check_peer_host(host).map_err(|refusal| PeerError::Refused(refusal.to_string()))?;
    let expected_key = decode_public_key(&peer.public_key)
        .ok_or_else(|| PeerError::Refused("malformed_public_key".into()))?;
    let stream = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect(peer.address.trim()),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return Err(PeerError::Unreachable(error.to_string())),
        Err(_) => return Err(PeerError::Unreachable("the connection timed out".into())),
    };
    let (read, mut write) = stream.into_split();
    let mut reader = LineReader::new(read);
    let handshake = async {
        let nonce = mint_nonce().map_err(|error| PeerError::Refused(error.to_string()))?;
        let own_key = state.identity.public_key();
        let hello = json!({
            "type": "peer.hello",
            "v": PEER_PROTOCOL_VERSION,
            "account": state.config.account,
            "device": { "id": state.identity.id(), "name": state.identity.name(), "platform": platform() },
            "public_key": own_key,
            "nonce": nonce,
        });
        let lost = |error: LocalApiError| PeerError::Unreachable(error.to_string());
        write_value(&mut write, &hello).await.map_err(lost)?;
        let welcome = reader
            .next_value()
            .await
            .map_err(lost)?
            .ok_or_else(|| PeerError::Unreachable("closed".into()))?;
        if text(&welcome, "type") == Some("peer.refused") {
            return Err(PeerError::Refused(
                text(&welcome, "reason").unwrap_or("refused").to_owned(),
            ));
        }
        let listener_key = text(&welcome, "public_key").unwrap_or_default().to_owned();
        let listener_nonce = text(&welcome, "nonce").unwrap_or_default().to_owned();
        let signature = text(&welcome, "signature").unwrap_or_default();
        let expected = transcript(
            "server",
            &state.config.account,
            &nonce,
            &listener_nonce,
            &own_key,
            &listener_key,
        );
        if text(&welcome, "type") != Some("peer.welcome")
            || decode_public_key(&listener_key) != Some(expected_key)
            || !verify_signature(&expected_key, &expected, signature)
        {
            return Err(PeerError::Refused("bad_signature".into()));
        }
        let proof = state.identity.sign(&transcript(
            "client",
            &state.config.account,
            &listener_nonce,
            &nonce,
            &listener_key,
            &own_key,
        ));
        write_value(
            &mut write,
            &json!({ "type": "peer.prove", "signature": proof }),
        )
        .await
        .map_err(lost)?;
        let ready = reader
            .next_value()
            .await
            .map_err(lost)?
            .ok_or_else(|| PeerError::Unreachable("closed".into()))?;
        match text(&ready, "type") {
            Some("peer.ready") => Ok(()),
            Some("peer.refused") => Err(PeerError::Refused(
                text(&ready, "reason").unwrap_or("refused").to_owned(),
            )),
            _ => Err(PeerError::Refused("malformed".into())),
        }
    };
    let outcome = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await;
    match outcome {
        Ok(Ok(())) => Ok(PeerLink { reader, write }),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(PeerError::Unreachable("the handshake timed out".into())),
    }
}

/// One request to `peer` over a fresh link, answered within `timeout`.
pub async fn peer_request(
    state: &RelaydState,
    peer: &PeerConfig,
    op: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, PeerError> {
    let mut link = dial(state, peer).await?;
    let mut request = match params {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    request.insert("id".into(), Value::String("1".into()));
    request.insert("type".into(), Value::String("peer.request".into()));
    request.insert("op".into(), Value::String(op.to_owned()));
    write_value(&mut link.write, &Value::Object(request))
        .await
        .map_err(|error| PeerError::Unreachable(error.to_string()))?;
    let answer = match tokio::time::timeout(timeout, link.reader.next_value()).await {
        Ok(Ok(Some(answer))) => answer,
        Ok(Ok(None)) => return Err(PeerError::Lost("the link closed before the answer".into())),
        Ok(Err(error)) => return Err(PeerError::Lost(error.to_string())),
        Err(_) => return Err(PeerError::Lost("no answer in time".into())),
    };
    if let Some(device) = peer.device_id() {
        state.record_peer_seen(&device);
    }
    if answer.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(answer.get("data").cloned().unwrap_or(Value::Null));
    }
    let error = answer.get("error").cloned().unwrap_or(Value::Null);
    Err(PeerError::Failed {
        code: text(&error, "code").unwrap_or("internal").to_owned(),
        reason: text(&error, "reason").unwrap_or_default().to_owned(),
    })
}
