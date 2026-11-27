//! What the credential-bearing calls answer (security design #14945, 5.0 to 5.7, 8.2): the non-secret part only. Each
//! answer is a closed shape, so a field cannot be added without a test noticing, and no answer has a field a secret
//! could ride in. The device-code event is the one exception by design: it is the argument of the person's own
//! `onUserCode` callback, never part of a result.

use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::auth_check::{AuthStatus, SessionAuthStatus};
use crate::schema::{Text, True};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub id: Text,
    pub name: Text,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalDelivery {
    pub tty: Text,
    pub session_id: u64,
    pub leader_pid: NonZeroU64,
    pub leader_started_at: Text,
    pub host_identity: Text,
    pub host_pid: NonZeroU64,
    pub host_started_at: Text,
    pub uid: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerDelivery {
    pub executable_identity: Text,
    pub pid: NonZeroU64,
    pub started_at: Text,
    pub allowlist_entry: Text,
}

/// The destination record of 8.2: where the secret went, by identity, never the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveredTo {
    Terminal(TerminalDelivery),
    Peer(PeerDelivery),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandedOver {
    pub delivered: True,
    pub device: Device,
    pub expires_at: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Text>,
    pub destination: DeliveredTo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCredentialDelivered {
    pub delivered: True,
    pub provider: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Text>,
    pub destination: DeliveredTo,
}

/// Where an adopted credential is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStorage {
    Omo,
    Login,
}

/// `account.adopt`'s answer: the device the credential belongs to and where it is now kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adopted {
    pub device: Device,
    pub storage: CredentialStorage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceServiceState {
    On,
    KeptOff,
    NotOn,
    Background,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedIn {
    pub api: Text,
    pub device: Device,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_device: Option<Text>,
    pub device_service: DeviceServiceState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyring {
    pub device_id: Text,
    pub signing_public_key: Text,
    pub sealing_public_key: Text,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncEnabled {
    pub enabled: True,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRecovered {
    pub recovered: True,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustAction {
    Ensure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustEnsured {
    pub action: TrustAction,
    pub generation: Text,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpLoggedIn {
    pub server: Text,
    pub stored: True,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Text>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpLoggedOut {
    pub server: Text,
    pub removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerStatus {
    pub server: Text,
    pub authenticated: bool,
    pub expired: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Text>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpStatus {
    pub servers: Vec<McpServerStatus>,
}

/// The device-code `code` event (5.4): only the person's callback receives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCode {
    pub verification_uri: String,
    pub user_code: String,
    pub matching_code: String,
    pub expires_at: String,
}

/// The credential-bearing calls with a closed answer, in the contract's order (`CUSTODY_RESULTS`).
pub const CUSTODY_RESULT_CALLS: [&str; 15] = [
    "account.credentials.handOver",
    "account.adopt",
    "account.signIn",
    "account.keys.init",
    "account.keys.status",
    "sync.enable",
    "sync.recover",
    "engine.auth.check",
    "engine.auth.credentials",
    "engine.auth.apiKey",
    "engine.auth.bearerToken",
    "engine.trust.ensure",
    "mcp.oauth.login",
    "mcp.oauth.logout",
    "mcp.oauth.status",
];

/// Answers a session principal receives where they differ from the person's and the script's (`SESSION_RESULTS`).
pub const SESSION_RESULT_CALLS: [&str; 1] = ["engine.auth.check"];

fn parses<T: serde::de::DeserializeOwned>(value: &Value) -> bool {
    serde_json::from_value::<T>(value.clone()).is_ok()
}

/// Whether `call` is one of the credential-bearing calls [`custody_answer_accepts`] decides.
pub fn is_custody_answer(call: &str) -> bool {
    CUSTODY_RESULT_CALLS.contains(&call)
}

/// Whether `value` is a valid answer of the credential-bearing call `call` to a person or a script; an unknown call
/// accepts nothing.
pub fn custody_answer_accepts(call: &str, value: &Value) -> bool {
    match call {
        "account.credentials.handOver" => parses::<HandedOver>(value),
        "account.adopt" => parses::<Adopted>(value),
        "account.signIn" => parses::<SignedIn>(value),
        "account.keys.init" | "account.keys.status" => parses::<Keyring>(value),
        "sync.enable" => parses::<SyncEnabled>(value),
        "sync.recover" => parses::<SyncRecovered>(value),
        "engine.auth.check" => parses::<AuthStatus>(value),
        "engine.auth.credentials" | "engine.auth.apiKey" | "engine.auth.bearerToken" => {
            parses::<ProviderCredentialDelivered>(value)
        }
        "engine.trust.ensure" => parses::<TrustEnsured>(value),
        "mcp.oauth.login" => parses::<McpLoggedIn>(value),
        "mcp.oauth.logout" => parses::<McpLoggedOut>(value),
        "mcp.oauth.status" => parses::<McpStatus>(value),
        _ => false,
    }
}

/// Whether `value` is a valid answer of `call` to a session principal, where that answer differs from a person's.
pub fn session_answer_accepts(call: &str, value: &Value) -> bool {
    match call {
        "engine.auth.check" => parses::<SessionAuthStatus>(value),
        _ => false,
    }
}
