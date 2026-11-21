//! Bearer tokens: which account, device and sync principal a request speaks for.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

/// The identity behind one token. The server derives an entry's author from this, never from the
/// request body: a device cannot claim another device's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub token: String,
    pub account: String,
    pub device: String,
    /// The account's name for this device in the device list, e.g. `device:abc`.
    pub sync_principal: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
    /// May call the `/v1/admin/*` routes.
    pub admin: bool,
}

impl Principal {
    /// The author object a sync row carries for this device.
    pub fn author(&self) -> Value {
        json!({
            "kind": "device",
            "id": self.device,
            "sealing_public_key": self.sealing_public_key,
            "signing_public_key": self.signing_public_key,
        })
    }
}

#[derive(Debug, Deserialize)]
struct TokenFile {
    tokens: Vec<TokenEntry>,
}

#[derive(Debug, Deserialize)]
struct TokenEntry {
    token: String,
    account: String,
    device: String,
    #[serde(default)]
    principal: Option<String>,
    #[serde(default)]
    sealing_public_key: Option<String>,
    #[serde(default)]
    signing_public_key: Option<String>,
    #[serde(default)]
    admin: bool,
}

/// The token file, indexed by token.
#[derive(Debug, Clone, Default)]
pub struct TokenTable {
    tokens: BTreeMap<String, Principal>,
}

impl TokenTable {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Reads `{"tokens": [...]}`; a duplicate or empty token is an error, never a silent override.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("tokens file {}: {error}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let file: TokenFile =
            serde_json::from_str(text).map_err(|error| format!("tokens file: {error}"))?;
        let mut tokens = BTreeMap::new();
        for entry in file.tokens {
            if entry.token.is_empty() {
                return Err("tokens file: an entry has an empty token".into());
            }
            if entry.account.is_empty() || entry.device.is_empty() {
                return Err(format!(
                    "tokens file: token for device {:?} names no account or device",
                    entry.device
                ));
            }
            let principal = entry
                .principal
                .unwrap_or_else(|| format!("device:{}", entry.device));
            let row = Principal {
                token: entry.token.clone(),
                account: entry.account,
                device: entry.device.clone(),
                sync_principal: principal,
                // A device row must carry non-empty key text; a token that names none falls back to
                // the device id so the row still parses. The docs ask for the real public keys.
                sealing_public_key: entry
                    .sealing_public_key
                    .filter(|key| !key.is_empty())
                    .unwrap_or_else(|| entry.device.clone()),
                signing_public_key: entry
                    .signing_public_key
                    .filter(|key| !key.is_empty())
                    .unwrap_or_else(|| entry.device.clone()),
                admin: entry.admin,
            };
            if tokens.insert(entry.token, row).is_some() {
                return Err("tokens file: the same token appears twice".into());
            }
        }
        Ok(Self { tokens })
    }

    pub fn lookup(&self, token: &str) -> Option<&Principal> {
        self.tokens.get(token)
    }

    pub fn principals(&self) -> impl Iterator<Item = &Principal> {
        self.tokens.values()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}
