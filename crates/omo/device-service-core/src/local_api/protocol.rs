//! Port of the parts of `local-api/protocol.ts` (and `protocol-status.ts`) mhc speaks: the handshake
//! messages, the response and event envelopes, `Status`, device rows and listing rows. A reader
//! ignores fields it does not define, so a newer service may add one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::errors::LocalApiError;
use super::policy::Policy;
pub use super::vocabulary::LOCAL_API_VERSION;
use super::vocabulary::{
    LISTING_ROWS, LISTING_SETTLED_BY_KINDS, LISTING_SOURCES, SESSION_ROW_STATES,
};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Policy>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingWaitKind {
    RateLimit,
    Retry,
}

/// A holder's retry wait on `rows: 6` / `rows: 7`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListingWait {
    pub kind: ListingWaitKind,
    pub attempt: u64,
    #[serde(rename = "maxAttempts")]
    pub max_attempts: u64,
    #[serde(rename = "delayMs")]
    pub delay_ms: u64,
    pub until: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ListingSettledBy {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "agent")]
    Agent {
        #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        #[serde(
            rename = "threadTitle",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        thread_title: Option<String>,
    },
    #[serde(rename = "pr-merged")]
    PrMerged {
        #[serde(
            rename = "pullRequestNumber",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pull_request_number: Option<u64>,
    },
    #[serde(rename = "pr-closed")]
    PrClosed {
        #[serde(
            rename = "pullRequestNumber",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pull_request_number: Option<u64>,
    },
    #[serde(rename = "inactivity")]
    Inactivity { days: u64 },
}

/// A session listing row across `rows` versions 2 through 7.
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
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_listed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_by: Option<ListingSettledBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<ListingWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
}

impl ListingRow {
    pub fn parse(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}

pub fn validate_sessions_list_rows(rows: u64) -> bool {
    LISTING_ROWS.contains(&rows)
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const DEVICE_MAX_CHARS: usize = 64;
const SESSION_ID_MAX_CHARS: usize = 128;
const SESSION_KIND_MAX_CHARS: usize = 32;
const TITLE_MAX_CHARS: usize = 256;
const PROJECT_MAX_CHARS: usize = 128;
const PATH_MAX_CHARS: usize = 1024;
const MODEL_PROVIDER_MAX_CHARS: usize = 64;
const MODEL_ID_MAX_CHARS: usize = 128;
const SETTLED_DAYS_MAX: u64 = 36_500;
const SETTLED_THREAD_TITLE_MAX_CHARS: usize = 256;
const ENGINE_VERSION_MAX_CHARS: usize = 128;
const WAIT_UNTIL_MAX_CHARS: usize = 27;

fn invalid_row(what: &str) -> LocalApiError {
    LocalApiError::protocol_violation(format!("invalid {what}"))
}

fn check_keys(
    map: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    what: &str,
) -> Result<(), LocalApiError> {
    for key in required {
        if !map.contains_key(*key) {
            return Err(invalid_row(what));
        }
    }
    for key in map.keys() {
        let name = key.as_str();
        if !required.contains(&name) && !optional.contains(&name) {
            return Err(invalid_row(what));
        }
    }
    Ok(())
}

fn text_bound(value: &Value, min: usize, max: usize, what: &str) -> Result<String, LocalApiError> {
    let Some(text) = value.as_str() else {
        return Err(invalid_row(what));
    };
    let len = text.encode_utf16().count();
    if len < min || len > max {
        return Err(invalid_row(what));
    }
    Ok(text.to_owned())
}

fn uint_bound(value: &Value, min: u64, max: u64, what: &str) -> Result<u64, LocalApiError> {
    let Some(n) = value.as_u64() else {
        return Err(invalid_row(what));
    };
    if n < min || n > max {
        return Err(invalid_row(what));
    }
    Ok(n)
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn parse_ascii_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<u32>().ok()
}

fn is_iso_utc_millis(until: &str) -> bool {
    if !until.is_ascii() {
        return false;
    }
    let bytes = until.as_bytes();
    let (year, rest): (i32, &[u8]) = if bytes.len() == 24 {
        let Some(y) = parse_ascii_u32(&bytes[0..4]) else {
            return false;
        };
        (y as i32, &bytes[4..])
    } else if bytes.len() == 27 && (bytes[0] == b'+' || bytes[0] == b'-') {
        let Some(y) = parse_ascii_u32(&bytes[1..7]) else {
            return false;
        };
        let signed = if bytes[0] == b'-' {
            -(y as i32)
        } else {
            y as i32
        };
        (signed, &bytes[7..])
    } else {
        return false;
    };

    if rest.len() != 20
        || rest[0] != b'-'
        || rest[3] != b'-'
        || rest[6] != b'T'
        || rest[9] != b':'
        || rest[12] != b':'
        || rest[15] != b'.'
        || rest[19] != b'Z'
    {
        return false;
    }
    let Some(month) = parse_ascii_u32(&rest[1..3]).and_then(|m| u8::try_from(m).ok()) else {
        return false;
    };
    let Some(day) = parse_ascii_u32(&rest[4..6]).and_then(|d| u8::try_from(d).ok()) else {
        return false;
    };
    let Some(hour) = parse_ascii_u32(&rest[7..9]) else {
        return false;
    };
    let Some(minute) = parse_ascii_u32(&rest[10..12]) else {
        return false;
    };
    let Some(second) = parse_ascii_u32(&rest[13..15]) else {
        return false;
    };
    if parse_ascii_u32(&rest[16..19]).is_none() {
        return false;
    }
    (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && hour <= 23
        && minute <= 59
        && second <= 59
}

fn validate_waiting(value: &Value) -> Result<ListingWait, LocalApiError> {
    let Some(map) = value.as_object() else {
        return Err(invalid_row("waiting"));
    };
    check_keys(
        map,
        &["kind", "attempt", "maxAttempts", "delayMs", "until"],
        &[],
        "waiting",
    )?;
    let kind = match map.get("kind").and_then(Value::as_str) {
        Some("rate_limit") => ListingWaitKind::RateLimit,
        Some("retry") => ListingWaitKind::Retry,
        _ => return Err(invalid_row("waiting kind")),
    };
    let max_attempts = uint_bound(
        &map["maxAttempts"],
        1,
        MAX_SAFE_INTEGER,
        "waiting maxAttempts",
    )?;
    let attempt = uint_bound(&map["attempt"], 1, max_attempts, "waiting attempt")?;
    let delay_ms = uint_bound(&map["delayMs"], 0, MAX_SAFE_INTEGER, "waiting delayMs")?;
    let until = text_bound(&map["until"], 1, WAIT_UNTIL_MAX_CHARS, "waiting until")?;
    if !is_iso_utc_millis(&until) {
        return Err(invalid_row("waiting until"));
    }
    Ok(ListingWait {
        kind,
        attempt,
        max_attempts,
        delay_ms,
        until,
    })
}

fn validate_settled_by(value: &Value) -> Result<Option<ListingSettledBy>, LocalApiError> {
    let Some(map) = value.as_object() else {
        return Err(invalid_row("settled_by"));
    };
    let Some(kind) = map.get("kind").and_then(Value::as_str) else {
        return Err(invalid_row("settled_by kind"));
    };
    if !LISTING_SETTLED_BY_KINDS.contains(&kind) {
        return Ok(None);
    }
    match kind {
        "user" => {
            check_keys(map, &["kind"], &[], "settled_by")?;
            Ok(Some(ListingSettledBy::User))
        }
        "agent" => {
            check_keys(map, &["kind"], &["threadId", "threadTitle"], "settled_by")?;
            let thread_id = map
                .get("threadId")
                .map(|v| text_bound(v, 1, SESSION_ID_MAX_CHARS, "settled_by threadId"))
                .transpose()?;
            let thread_title = map
                .get("threadTitle")
                .map(|v| {
                    text_bound(
                        v,
                        0,
                        SETTLED_THREAD_TITLE_MAX_CHARS,
                        "settled_by threadTitle",
                    )
                })
                .transpose()?;
            Ok(Some(ListingSettledBy::Agent {
                thread_id,
                thread_title,
            }))
        }
        "pr-merged" | "pr-closed" => {
            check_keys(map, &["kind"], &["pullRequestNumber"], "settled_by")?;
            let pull_request_number = map
                .get("pullRequestNumber")
                .map(|v| uint_bound(v, 0, MAX_SAFE_INTEGER, "settled_by pullRequestNumber"))
                .transpose()?;
            Ok(Some(if kind == "pr-merged" {
                ListingSettledBy::PrMerged {
                    pull_request_number,
                }
            } else {
                ListingSettledBy::PrClosed {
                    pull_request_number,
                }
            }))
        }
        "inactivity" => {
            check_keys(map, &["kind", "days"], &[], "settled_by")?;
            let days = uint_bound(&map["days"], 0, SETTLED_DAYS_MAX, "settled_by days")?;
            Ok(Some(ListingSettledBy::Inactivity { days }))
        }
        _ => Ok(None),
    }
}

fn valid_row_kind(kind: &str) -> bool {
    let bytes = kind.as_bytes();
    (1..=SESSION_KIND_MAX_CHARS).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// Validates one `sessions.listing_rows` row against the local API v1 contract (`rows` 2..=7).
pub fn validate_listing_row(value: &Value) -> Result<ListingRow, LocalApiError> {
    let Some(map) = value.as_object() else {
        return Err(invalid_row("listing row"));
    };
    check_keys(
        map,
        &[
            "device",
            "session_id",
            "title",
            "project",
            "path",
            "state",
            "updated_ms",
            "source",
            "as_of",
        ],
        &[
            "kind",
            "parent",
            "parent_listed",
            "durable_id",
            "model",
            "archived",
            "settled",
            "settled_by",
            "waiting",
            "engine_version",
        ],
        "listing row",
    )?;

    if map.contains_key("parent") != map.contains_key("parent_listed") {
        return Err(invalid_row("parent_listed"));
    }

    let device = text_bound(&map["device"], 1, DEVICE_MAX_CHARS, "device")?;
    let session_id = text_bound(&map["session_id"], 1, SESSION_ID_MAX_CHARS, "session_id")?;
    let title = text_bound(&map["title"], 0, TITLE_MAX_CHARS, "title")?;
    let project = text_bound(&map["project"], 1, PROJECT_MAX_CHARS, "project")?;
    let path = text_bound(&map["path"], 1, PATH_MAX_CHARS, "path")?;
    let state = map["state"]
        .as_str()
        .filter(|s| SESSION_ROW_STATES.contains(s))
        .ok_or_else(|| invalid_row("row state"))?
        .to_owned();
    let updated_ms = i64::try_from(uint_bound(
        &map["updated_ms"],
        0,
        MAX_SAFE_INTEGER,
        "updated_ms",
    )?)
    .map_err(|_| invalid_row("updated_ms"))?;
    let source = map["source"]
        .as_str()
        .filter(|s| LISTING_SOURCES.contains(s))
        .ok_or_else(|| invalid_row("row source"))?
        .to_owned();
    let as_of = i64::try_from(uint_bound(&map["as_of"], 0, MAX_SAFE_INTEGER, "as_of")?)
        .map_err(|_| invalid_row("as_of"))?;

    let kind = match map.get("kind") {
        None => None,
        Some(v) => {
            let Some(s) = v.as_str() else {
                return Err(invalid_row("row kind"));
            };
            if !valid_row_kind(s) {
                return Err(invalid_row("row kind"));
            }
            Some(s.to_owned())
        }
    };
    let parent = map
        .get("parent")
        .map(|v| text_bound(v, 1, SESSION_ID_MAX_CHARS, "parent"))
        .transpose()?;
    let parent_listed = map
        .get("parent_listed")
        .map(|v| v.as_bool().ok_or_else(|| invalid_row("parent_listed")))
        .transpose()?;
    let durable_id = map
        .get("durable_id")
        .map(|v| text_bound(v, 1, SESSION_ID_MAX_CHARS, "durable_id"))
        .transpose()?;
    let model = match map.get("model") {
        None => None,
        Some(v) => {
            let Some(m) = v.as_object() else {
                return Err(invalid_row("model"));
            };
            check_keys(m, &["provider", "id"], &[], "model")?;
            let provider = text_bound(
                &m["provider"],
                1,
                MODEL_PROVIDER_MAX_CHARS,
                "model provider",
            )?;
            let id = text_bound(&m["id"], 1, MODEL_ID_MAX_CHARS, "model id")?;
            Some(json!({ "provider": provider, "id": id }))
        }
    };
    let archived = match map.get("archived") {
        None => None,
        Some(Value::Bool(true)) => Some(true),
        Some(_) => return Err(invalid_row("archived")),
    };
    let (settled, settled_by) = match (map.get("settled"), map.get("settled_by")) {
        (None, None) => (None, None),
        (None, Some(_)) => return Err(invalid_row("settled_by")),
        (Some(v), by) => {
            let s = v.as_bool().ok_or_else(|| invalid_row("settled"))?;
            match by {
                None => (Some(s), None),
                Some(_) if !s => return Err(invalid_row("settled_by")),
                Some(by_val) => (Some(s), validate_settled_by(by_val)?),
            }
        }
    };
    let waiting = map.get("waiting").map(validate_waiting).transpose()?;
    let engine_version = map
        .get("engine_version")
        .map(|v| text_bound(v, 1, ENGINE_VERSION_MAX_CHARS, "engine_version"))
        .transpose()?;

    Ok(ListingRow {
        device,
        session_id,
        title: Some(title),
        project: Some(project),
        path: Some(path),
        state,
        updated_ms: Some(updated_ms),
        source,
        as_of: Some(as_of),
        kind,
        parent,
        parent_listed,
        durable_id,
        model,
        archived,
        settled,
        settled_by,
        waiting,
        engine_version,
    })
}
