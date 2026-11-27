//! The inputs of the credential-bearing calls (security design #14945, sections 4.2, 4.3, 5.0 to 5.7). No input
//! carries a secret as text: an incoming credential is a readable handle the caller already holds (5.3), and an
//! outgoing one names an explicit destination the core verifies (5.2), never a default picked because an argument was
//! left out. The calls that take callbacks (`account.signIn`, `sync.enable`, `sync.recover`) have no JSON form.

use std::io::Read;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema::Text;

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalDestination {}

/// A peer process is named with its start time: a pid alone is not an identity, and nothing names a path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerDestination {
    pub pid: NonZeroU64,
    pub started_at: Text,
}

/// Where a credential goes (5.2): an admitted terminal, or a peer process the core opens its channel to and verifies
/// by executable identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    Terminal(TerminalDestination),
    Peer(PeerDestination),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandOverWhat {
    AccessToken,
    StoredCredential,
}

/// `account.adopt`: the credential is a stream the caller already holds and the core reads once. A string, a path
/// or an environment name is not one, so this input has no JSON form.
pub struct AccountAdopt<R: Read> {
    pub credential: R,
    pub replace: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCredentialsHandOver {
    pub what: HandOverWhat,
    pub destination: Destination,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EngineAuthCheck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineAuthCredentials {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
    pub destination: Destination,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineAuthApiKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Text>,
    pub destination: Destination,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineAuthBearerToken {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_expiry_ms: Option<u64>,
    pub destination: Destination,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EngineTrustEnsure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_pid: Option<NonZeroU64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpOauthLogin {
    pub server: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<Text>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpOauthLogout {
    pub server: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<Text>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct McpOauthStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<Text>,
}

fn parses<T: serde::de::DeserializeOwned>(input: &Value) -> bool {
    serde_json::from_value::<T>(input.clone()).is_ok()
}

/// Whether `input` is inside the input shape of the credential-bearing call `call`. A call whose input carries a
/// handle or a callback (`account.adopt`, `account.signIn`, `sync.enable`, `sync.recover`) and an unknown call
/// accept no JSON value.
pub fn custody_input_accepts(call: &str, input: &Value) -> bool {
    match call {
        "account.credentials.handOver" => parses::<AccountCredentialsHandOver>(input),
        "account.keys.init" | "account.keys.status" => input.is_object(),
        "engine.auth.check" => parses::<EngineAuthCheck>(input),
        "engine.auth.credentials" => parses::<EngineAuthCredentials>(input),
        "engine.auth.apiKey" => parses::<EngineAuthApiKey>(input),
        "engine.auth.bearerToken" => parses::<EngineAuthBearerToken>(input),
        "engine.trust.ensure" => parses::<EngineTrustEnsure>(input),
        "mcp.oauth.login" => serde_json::from_value::<McpOauthLogin>(input.clone())
            .is_ok_and(|login| login.scopes.is_none_or(|scopes| !scopes.is_empty())),
        "mcp.oauth.logout" => parses::<McpOauthLogout>(input),
        "mcp.oauth.status" => parses::<McpOauthStatus>(input),
        _ => false,
    }
}
