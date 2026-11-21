//! Port of the parts of `local-api/protocol.ts` (and `protocol-status.ts`) mhc speaks: the handshake
//! messages, the response and event envelopes, `Status`, device rows and listing rows. A reader
//! ignores fields it does not define, so a newer service may add one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::errors::LocalApiError;
pub use super::vocabulary::LOCAL_API_VERSION;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// `desktop`, `omo` (a CLI or engine), `installer` or `sdk`.
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_home: Option<String>,
}

impl ClientInfo {
    /// A command-line or engine client: mhc speaks as the `omo` client kind, which carries no
    /// `data_home`.
    pub fn cli(version: impl Into<String>) -> Self {
        Self {
            name: "omo".into(),
            version: version.into(),
            data_home: None,
        }
    }
}

pub fn hello_message(client: &ClientInfo, client_nonce: &str) -> Value {
    json!({
        "type": "hello",
        "v": LOCAL_API_VERSION,
        "client": client,
        "client_nonce": client_nonce,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Welcome {
    pub v: u64,
    pub service_version: String,
    pub owner: String,
    pub server_proof: String,
    pub server_nonce: String,
    pub capabilities: Vec<String>,
    /// The service's current `Status`.
    pub state: Value,
}

impl Welcome {
    pub fn parse(message: &Value) -> Result<Self, LocalApiError> {
        let text = |key: &str| {
            message
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    LocalApiError::protocol_violation(format!("welcome.{key} is missing"))
                })
        };
        let v = message
            .get("v")
            .and_then(Value::as_u64)
            .ok_or_else(|| LocalApiError::protocol_violation("welcome.v is missing"))?;
        let capabilities = message
            .get("capabilities")
            .and_then(Value::as_array)
            .ok_or_else(|| LocalApiError::protocol_violation("welcome.capabilities is missing"))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        Ok(Self {
            v,
            service_version: text("service_version")?,
            owner: text("owner")?,
            server_proof: text("server_proof")?,
            server_nonce: text("server_nonce")?,
            capabilities,
            state: message.get("state").cloned().unwrap_or(Value::Null),
        })
    }

    pub fn to_message(&self) -> Value {
        json!({
            "type": "welcome",
            "v": self.v,
            "service_version": self.service_version,
            "owner": self.owner,
            "server_proof": self.server_proof,
            "server_nonce": self.server_nonce,
            "capabilities": self.capabilities,
            "state": self.state,
        })
    }
}

pub fn refused_message(reason: &str) -> Value {
    json!({ "type": "refused", "reason": reason })
}

pub fn ok_response(id: &str, data: Value) -> Value {
    json!({ "id": id, "type": "response", "ok": true, "data": data })
}

pub fn error_response(id: &str, error: &LocalApiError) -> Value {
    json!({ "id": id, "type": "response", "ok": false, "error": error.to_error_body() })
}

/// `{"type": "event", "kind": kind, ...fields}`.
pub fn event_message(kind: &str, fields: Value) -> Value {
    let mut message = match fields {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    message.insert("type".into(), Value::String("event".into()));
    message.insert("kind".into(), Value::String(kind.to_owned()));
    Value::Object(message)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

/// `Status`: `{state, since_ms, detail?, device, origin_configured, enabled, policy?}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub since_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    pub device: Option<DeviceRef>,
    pub origin_configured: bool,
    pub enabled: bool,
}

/// A `devices.list` row: `{device, label, kind, platform, last_seen_ms, online}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceRow {
    pub device: String,
    pub label: String,
    pub kind: String,
    pub platform: String,
    #[serde(default)]
    pub last_seen_ms: Option<i64>,
    pub online: bool,
}

/// A session listing row. `rows: 3` adds `durable_id`, `rows: 4` adds `model` and `archived`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListingRow {
    pub device: String,
    pub session_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub state: String,
    #[serde(default)]
    pub updated_ms: Option<i64>,
    pub source: String,
    #[serde(default)]
    pub as_of: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
}

impl ListingRow {
    pub fn parse(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}
